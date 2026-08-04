mod cli;
mod command;
mod dashboard;
mod pid;
mod shutdown;
mod summary_worker;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use figment::providers::Serialized;
use figment::Figment;
use tokio::sync::mpsc;

use familiar_ai_core::config::{Config, LoggingConfig};
use familiar_ai_core::models::NewProject;
use familiar_ai_core::{AppPaths, AppStatus, CanonicalFileIdentity, VersionInfo};
use familiar_ai_llm::InferenceRouter;
use familiar_ai_storage::{
    Database, FileSummaryRepository, LifecycleChange, LifecycleRepository, ProjectRepository,
    RetirementReason, ReviewRepository,
};
use familiar_ai_watcher::{FileWatcher, WatcherEvent};

use crate::summary_worker::{run_repository_scan, SummaryRequest, SummaryWorker};

use crate::cli::Cli;
use crate::command::{handle_commands, CommandState, DaemonCommand};
use crate::pid::{remove_pid_file, write_pid_file};
use crate::shutdown::shutdown_signal;

/// State assembled at startup, shared between daemon work and (optionally) tray.
#[allow(dead_code)]
struct DaemonState {
    config: Config,
    config_path: PathBuf,
    paths: AppPaths,
    db: Arc<Mutex<Database>>,
    status: Arc<Mutex<AppStatus>>,
    pid_path: PathBuf,
    router: Arc<InferenceRouter>,
}

fn bootstrap() -> familiar_ai_core::Result<(DaemonState, familiar_ai_logging::LogGuard)> {
    let cli = Cli::parse();

    let paths = AppPaths::resolve()?;
    paths.ensure_dirs()?;

    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(|| paths.config_dir.join("config.toml"));

    let config = if let Some(ref level) = cli.log_level {
        let overrides = Figment::from(Serialized::defaults(Config {
            logging: LoggingConfig {
                level: level.clone(),
                ..Default::default()
            },
            ..Default::default()
        }));
        Config::load_with_overrides(Some(&config_path), overrides)?
    } else {
        Config::load(Some(&config_path))?
    };

    let log_guard = familiar_ai_logging::init_logging(&config.logging, Some(&paths.log_dir))?;

    let version = VersionInfo::current();
    tracing::info!("{version}");
    tracing::info!(
        config_dir = %paths.config_dir.display(),
        data_dir = %paths.data_dir.display(),
        state_dir = %paths.state_dir.display(),
        "daemon starting"
    );

    let db_path = config.database.resolve_path(&paths.data_dir);
    tracing::info!(db_path = %db_path.display(), "opening database");
    let db = Arc::new(Mutex::new(Database::open(&db_path)?));
    {
        let db_lock = db.lock().unwrap();
        let migrations_applied = db_lock.run_migrations()?;
        let interrupted_reviews = ReviewRepository::new(db_lock.conn()).recover_incomplete()?;
        if interrupted_reviews > 0 {
            tracing::warn!(
                count = interrupted_reviews,
                "marked incomplete review cycles interrupted without replay"
            );
        }
        if migrations_applied > 0 {
            tracing::info!(count = migrations_applied, "applied database migrations");
        } else {
            tracing::info!("database schema up to date");
        }
    }

    let pid_path = config
        .daemon
        .pid_file
        .clone()
        .unwrap_or_else(|| paths.pid_path.clone());
    write_pid_file(&pid_path)?;
    tracing::info!(pid_file = %pid_path.display(), "PID file written");

    let status = Arc::new(Mutex::new(AppStatus::new()));
    {
        let db_lock = db.lock().unwrap();
        let active = db_lock.list_active_projects().unwrap_or_default();
        let mut s = status.lock().unwrap();
        s.active_projects = active.len();
        // local_llm_enabled starts false; the async daemon_run entry point
        // will flip it to true after the manager actually loads the backend
        // (if config.inference.text.mode != familiar_ai_core::config::InferenceMode::Disabled is set).
        s.local_llm_enabled = false;
        // mcp_enabled means "MCP capability is compiled in and the binary exists",
        // not "an MCP session is currently active". MCP runs as a separate process
        // (familiar-ai-mcp binary) spawned per-session by the client.
        s.mcp_enabled = true;
        tracing::info!(
            active_projects = s.active_projects,
            "loaded existing projects"
        );
    }

    // NOTE: Each process (daemon, MCP binary) constructs its own LlmManager
    // from the on-disk config. They do not share runtime state. If the user
    // toggles LLM via the tray menu, the MCP binary spawned later by Claude
    // Code will not see the runtime change — it only reads the config file.
    // A future shared state store or IPC layer is out of scope for PRD-008.
    let router = Arc::new(InferenceRouter::new(&config.inference));

    Ok((
        DaemonState {
            config,
            config_path,
            paths,
            db,
            status,
            pid_path,
            router,
        },
        log_guard,
    ))
}

/// Spawn the watcher, heartbeat, and command handler. Returns when shutdown_rx fires.
async fn daemon_run(
    state: &DaemonState,
    command_rx: mpsc::Receiver<DaemonCommand>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let command_state = Arc::new(Mutex::new(CommandState::new()));

    // If configured, try to load the LLM backend on startup. Failures are
    // logged but do not block daemon startup.
    if state.config.inference.text.mode != familiar_ai_core::config::InferenceMode::Disabled {
        match state.router.enable().await {
            Ok(()) => {
                tracing::info!("LLM backend loaded");
                state.status.lock().unwrap().local_llm_enabled = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load LLM backend on startup");
            }
        }
    }

    // Spawn summary worker if enabled
    let (summary_tx, summary_handle) = if state.config.summary.enabled {
        let (tx, rx) = mpsc::channel::<SummaryRequest>(state.config.summary.max_pending_files);
        let worker = SummaryWorker::new(
            state.db.clone(),
            state.config.summary.clone(),
            command_state.clone(),
        );
        let worker_shutdown = shutdown_rx.clone();
        let handle = tokio::spawn(async move {
            worker.run(rx, worker_shutdown).await;
        });
        (Some(tx), Some(handle))
    } else {
        tracing::info!("summary worker disabled");
        (None, None)
    };

    // Spawn file watcher if enabled
    let watcher_handle = if state.config.watcher.enabled && !state.config.watcher.paths.is_empty() {
        let (event_tx, event_rx) = mpsc::channel::<WatcherEvent>(256);
        let watcher = FileWatcher::new(state.config.watcher.clone());
        let watcher_shutdown = shutdown_rx.clone();

        let watcher_task = tokio::spawn(async move {
            if let Err(e) = watcher.run(event_tx, watcher_shutdown).await {
                tracing::error!(error = %e, "watcher failed");
            }
        });

        let db_clone = state.db.clone();
        let status_clone = state.status.clone();
        let summary_tx_clone = summary_tx.clone();
        let max_size = state.config.summary.max_file_size_bytes;
        let handler_task = tokio::spawn(async move {
            handle_watcher_events(event_rx, db_clone, status_clone, summary_tx_clone, max_size)
                .await;
        });

        Some((watcher_task, handler_task))
    } else {
        tracing::info!("file watcher disabled or no paths configured");
        None
    };

    // Spawn heartbeat
    let heartbeat_status = state.status.clone();
    let interval = state.config.daemon.heartbeat_interval_secs;
    let heartbeat_handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let mut s = heartbeat_status.lock().unwrap();
            s.record_heartbeat();
            tracing::info!(
                last_heartbeat = %s.last_heartbeat,
                uptime_secs = (chrono::Utc::now() - s.startup_time).num_seconds(),
                active_projects = s.active_projects,
                "heartbeat"
            );
        }
    });

    // Spawn command handler
    let cmd_status = state.status.clone();
    let cmd_state = command_state.clone();
    let cmd_router = state.router.clone();
    let cmd_shutdown_tx = shutdown_tx.clone();
    let command_handle = tokio::spawn(async move {
        handle_commands(
            command_rx,
            cmd_status,
            cmd_state,
            cmd_router,
            cmd_shutdown_tx,
        )
        .await;
    });

    // Spawn dashboard if enabled
    let _dashboard_handle = if state.config.dashboard.enabled {
        let dash_state = dashboard::DashboardState {
            db: state.db.clone(),
            status: state.status.clone(),
            router: state.router.clone(),
            start_time: chrono::Utc::now(),
        };
        let dash_shutdown = shutdown_rx.clone();
        let bind = state.config.dashboard.bind_address.clone();
        Some(tokio::spawn(async move {
            dashboard::run_dashboard(dash_state, bind, dash_shutdown).await;
        }))
    } else {
        None
    };

    // Wait for shutdown
    let mut shutdown_rx_local = shutdown_rx;
    tokio::select! {
        _ = shutdown_signal() => {
            tracing::info!("os shutdown signal received");
        }
        _ = shutdown_rx_local.changed() => {
            tracing::info!("internal shutdown received");
        }
    }

    let _ = shutdown_tx.send(true);
    heartbeat_handle.abort();
    command_handle.abort();

    if let Some((watcher_task, handler_task)) = watcher_handle {
        let _ = tokio::time::timeout(Duration::from_secs(5), watcher_task).await;
        handler_task.abort();
    }

    if let Some(handle) = summary_handle {
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
    drop(summary_tx);
}

// =====================================================================
// Entry points: tray vs headless
// =====================================================================

#[cfg(not(feature = "tray"))]
fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fatal: failed to build tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(async {
        match run_headless().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("fatal: {e}");
                ExitCode::FAILURE
            }
        }
    })
}

#[cfg(not(feature = "tray"))]
async fn run_headless() -> familiar_ai_core::Result<()> {
    let (state, _log_guard) = bootstrap()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (_command_tx, command_rx) = mpsc::channel::<DaemonCommand>(64);

    daemon_run(&state, command_rx, shutdown_tx, shutdown_rx).await;

    remove_pid_file(&state.pid_path)?;
    tracing::info!("familiar-ai-daemon stopped");
    Ok(())
}

#[cfg(feature = "tray")]
fn main() -> ExitCode {
    let (state, _log_guard) = match bootstrap() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fatal: {e}");
            return ExitCode::FAILURE;
        }
    };

    if !state.config.tray.enabled {
        // Tray disabled in config — run headless on a tokio runtime
        return run_with_tray_feature_but_disabled(state);
    }

    // Tray enabled: run tokio in a worker thread, tray on main thread
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => Arc::new(rt),
        Err(e) => {
            eprintln!("fatal: failed to build tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (command_tx, command_rx) = mpsc::channel::<DaemonCommand>(64);

    // Spawn daemon_run on the tokio runtime
    let state_arc = Arc::new(state);
    let state_for_daemon = state_arc.clone();
    let shutdown_tx_for_daemon = shutdown_tx.clone();
    let shutdown_rx_for_daemon = shutdown_rx.clone();

    let runtime_for_daemon = runtime.clone();
    let daemon_handle = std::thread::spawn(move || {
        runtime_for_daemon.block_on(async move {
            daemon_run(
                &state_for_daemon,
                command_rx,
                shutdown_tx_for_daemon,
                shutdown_rx_for_daemon,
            )
            .await;
        });
    });

    // Tray needs a TrayCommand sender, which we bridge to DaemonCommand
    let (tray_tx, mut tray_rx) = mpsc::channel::<familiar_ai_tray::TrayCommand>(64);
    let bridge_command_tx = command_tx.clone();
    let bridge_runtime = runtime.clone();
    bridge_runtime.spawn(async move {
        while let Some(tc) = tray_rx.recv().await {
            let opt: Option<DaemonCommand> = tc.into();
            if let Some(dc) = opt {
                let _ = bridge_command_tx.send(dc).await;
            }
        }
    });

    let tray_app = familiar_ai_tray::TrayApp::new(
        state_arc.config.tray.clone(),
        state_arc.status.clone(),
        state_arc.db.clone(),
        tray_tx,
        state_arc.config_path.clone(),
    );

    let tray_result = tray_app.run();
    if let Err(e) = tray_result {
        tracing::error!(error = %e, "tray exited with error");
    }

    // Tray exited — signal shutdown
    let _ = shutdown_tx.send(true);
    let _ = daemon_handle.join();

    if let Err(e) = remove_pid_file(&state_arc.pid_path) {
        tracing::warn!(error = %e, "failed to remove pid file");
    }
    tracing::info!("familiar-ai-daemon stopped");
    ExitCode::SUCCESS
}

#[cfg(feature = "tray")]
fn run_with_tray_feature_but_disabled(state: DaemonState) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fatal: failed to build tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(async {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (_command_tx, command_rx) = mpsc::channel::<DaemonCommand>(64);
        daemon_run(&state, command_rx, shutdown_tx, shutdown_rx).await;
        if let Err(e) = remove_pid_file(&state.pid_path) {
            tracing::warn!(error = %e, "failed to remove pid file");
        }
        tracing::info!("familiar-ai-daemon stopped");
    });
    ExitCode::SUCCESS
}

fn lookup_project_id(db: &Arc<Mutex<Database>>, repo_root: &std::path::Path) -> Option<i64> {
    let repo_str = repo_root.to_string_lossy().to_string();
    let db_lock = db.lock().unwrap();
    db_lock
        .get_project_by_repo_root(&repo_str)
        .ok()
        .flatten()
        .map(|p| p.id)
}

async fn handle_watcher_events(
    mut rx: mpsc::Receiver<WatcherEvent>,
    db: Arc<Mutex<Database>>,
    status: Arc<Mutex<AppStatus>>,
    summary_tx: Option<mpsc::Sender<SummaryRequest>>,
    max_file_size_bytes: u64,
) {
    while let Some(event) = rx.recv().await {
        match event {
            WatcherEvent::RepoDiscovered { repo_root } => {
                let repo_str = repo_root.to_string_lossy().to_string();
                let project_id = {
                    let db_lock = db.lock().unwrap();
                    match db_lock.get_project_by_repo_root(&repo_str) {
                        Ok(Some(project)) => {
                            tracing::debug!(project = %project.name, "repo already registered");
                            Some(project.id)
                        }
                        Ok(None) => None,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to check project");
                            None
                        }
                    }
                };

                let resolved_pid = if let Some(id) = project_id {
                    Some(id)
                } else {
                    let name = repo_root
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| repo_str.clone());

                    let new_project = NewProject {
                        name: name.clone(),
                        repo_root: repo_str.clone(),
                        ignored_paths: vec![],
                        token_budget: None,
                    };

                    let db_lock = db.lock().unwrap();
                    match db_lock.create_project(&new_project) {
                        Ok(project) => {
                            tracing::info!(
                                project_id = project.id,
                                name = %project.name,
                                "auto-registered project from watcher"
                            );
                            drop(db_lock);
                            let mut s = status.lock().unwrap();
                            s.active_projects += 1;
                            Some(project.id)
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to register project");
                            None
                        }
                    }
                };

                // Initial scan to populate file summaries
                if let (Some(pid), Some(tx)) = (resolved_pid, summary_tx.as_ref()) {
                    run_repository_scan(&db, &repo_root, pid, tx, max_file_size_bytes);
                }
            }
            WatcherEvent::FileCreated { path, repo_root }
            | WatcherEvent::FileChanged { path, repo_root } => {
                tracing::debug!(
                    path = %path.display(),
                    repo = repo_root.as_ref().map(|r| r.display().to_string()).unwrap_or_default(),
                    "file changed"
                );
                if let (Some(repo), Some(tx)) = (repo_root.as_ref(), summary_tx.as_ref()) {
                    if let Some(pid) = lookup_project_id(&db, repo) {
                        let identity = match CanonicalFileIdentity::from_observed(pid, repo, &path)
                        {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!(error=%e,"rejected watcher identity");
                                continue;
                            }
                        };
                        let meta = match std::fs::metadata(&path) {
                            Ok(v) if v.is_file() && v.len() <= max_file_size_bytes => v,
                            Ok(_) => {
                                let _ = db.lock().unwrap().retire_absent(
                                    pid,
                                    identity.path(),
                                    RetirementReason::Ineligible,
                                    "watcher",
                                    None,
                                );
                                continue;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                let _ = db.lock().unwrap().retire_absent(
                                    pid,
                                    identity.path(),
                                    RetirementReason::Deleted,
                                    "watcher",
                                    None,
                                );
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!(error=%e,"source unavailable; leaving active summary untouched");
                                continue;
                            }
                        };
                        if let Err(error) = std::fs::read_to_string(&path) {
                            if error.kind() == std::io::ErrorKind::InvalidData {
                                let _ = db.lock().unwrap().retire_absent(
                                    pid,
                                    identity.path(),
                                    RetirementReason::Ineligible,
                                    "watcher",
                                    None,
                                );
                            } else {
                                tracing::warn!(error=%error,"source unreadable; active summary retained");
                            }
                            continue;
                        }
                        let change = if db
                            .lock()
                            .unwrap()
                            .get_file_summary_by_path(pid, identity.path())
                            .ok()
                            .flatten()
                            .is_some()
                        {
                            LifecycleChange::Modify
                        } else {
                            LifecycleChange::Create
                        };
                        let deferred = tx.capacity() == 0;
                        if let Err(e) = db.lock().unwrap().observe_change(
                            pid,
                            identity.path(),
                            change,
                            "watcher",
                            deferred,
                        ) {
                            tracing::warn!(error=%e,"failed to persist lifecycle observation");
                            continue;
                        }
                        let _ = meta;
                        if tx
                            .try_send(SummaryRequest {
                                project_id: pid,
                                repo_root: repo.clone(),
                                path: path.clone(),
                            })
                            .is_err()
                        {
                            tracing::debug!(path=%path.display(),"summary dispatch deferred; durable work retained");
                        }
                    }
                }
            }
            WatcherEvent::FileRemoved { path, repo_root } => {
                tracing::debug!(
                    path = %path.display(),
                    repo = repo_root.as_ref().map(|r| r.display().to_string()).unwrap_or_default(),
                    "file removed"
                );
                if let Some(repo) = repo_root.as_ref() {
                    if let Some(pid) = lookup_project_id(&db, repo) {
                        if let Ok(identity) = CanonicalFileIdentity::from_observed(pid, repo, &path)
                        {
                            match std::fs::symlink_metadata(&path){Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{if let Err(e)=db.lock().unwrap().retire_absent(pid,identity.path(),RetirementReason::Deleted,"watcher",None){tracing::warn!(error=%e,"failed to retire removed file");}},Ok(_)=>tracing::debug!("removal observation contradicted by current source; reconciliation required"),Err(e)=>tracing::warn!(error=%e,"removal is uncertain; active summary retained")}
                        }
                    }
                }
            }
            WatcherEvent::FileRenamed {
                old_path,
                new_path,
                repo_root,
            } => {
                tracing::debug!(
                    old = %old_path.display(),
                    new = %new_path.display(),
                    repo = repo_root.as_ref().map(|r| r.display().to_string()).unwrap_or_default(),
                    "file renamed"
                );
                if let (Some(repo), Some(tx)) = (repo_root.as_ref(), summary_tx.as_ref()) {
                    if let Some(pid) = lookup_project_id(&db, repo) {
                        let old = CanonicalFileIdentity::from_observed(pid, repo, &old_path);
                        let new = CanonicalFileIdentity::from_observed(pid, repo, &new_path);
                        if let (Ok(old), Ok(new)) = (old, new) {
                            let old_absent = matches!(std::fs::symlink_metadata(&old_path), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
                            let new_valid = std::fs::metadata(&new_path)
                                .map(|m| m.is_file() && m.len() <= max_file_size_bytes)
                                .unwrap_or(false);
                            if old_absent && new_valid {
                                match db.lock().unwrap().observe_exact_rename(
                                    pid,
                                    old.path(),
                                    new.path(),
                                    "watcher",
                                    tx.capacity() == 0,
                                ) {
                                    Ok(_) => {
                                        let _ = tx.try_send(SummaryRequest {
                                            project_id: pid,
                                            repo_root: repo.clone(),
                                            path: new_path,
                                        });
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = %e, "exact rename failed closed")
                                    }
                                }
                            } else {
                                tracing::debug!(
                                    "rename state is ambiguous; complete scan required"
                                );
                            }
                        }
                    }
                }
            }
            WatcherEvent::FileAmbiguous {
                paths,
                repo_root,
                detail,
            } => {
                tracing::warn!(?paths, repo=?repo_root, %detail, "ambiguous watcher observation; complete scan required");
            }
            WatcherEvent::WatchError { message } => {
                tracing::warn!(error = %message, "watcher error");
            }
        }
    }
}
