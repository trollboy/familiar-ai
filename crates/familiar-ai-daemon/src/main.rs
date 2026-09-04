mod command;
mod daemon_cli;
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

#[cfg(feature = "tray")]
use crate::command::daemon_command_from_tray;
use crate::command::{handle_commands, CommandState, DaemonCommand};
use crate::daemon_cli::Cli;
use crate::pid::{remove_pid_file, write_pid_file};
use crate::shutdown::{shutdown_signal, TerminationSignals};

/// State assembled at startup, shared between daemon work and (optionally) tray.
#[allow(dead_code)]
struct DaemonState {
    ownership: familiar_ai_daemon::worker_lock::WorkerLock,
    config: Config,
    config_path: PathBuf,
    paths: AppPaths,
    db: Arc<Mutex<Database>>,
    status: Arc<Mutex<AppStatus>>,
    pid_path: PathBuf,
    router: Arc<InferenceRouter>,
    control: familiar_ai_daemon::control_plane::ControlPlaneService,
    control_socket: PathBuf,
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

    // Claim before database mutation or socket binding. Its lifetime covers
    // the entire host and it is released only after DaemonState drops.
    let persistent_installation = paths.data_dir.join("installation-id");
    let persistent_generation = paths.data_dir.join("control-plane.generation");
    if persistent_installation.exists() {
        std::fs::copy(
            &persistent_installation,
            paths.runtime_dir.join("installation-id"),
        )?;
    }
    if persistent_generation.exists() {
        std::fs::copy(
            &persistent_generation,
            paths.runtime_dir.join("control-plane.generation"),
        )?;
    }
    let requested_socket = config
        .daemon
        .socket_path
        .clone()
        .unwrap_or_else(|| paths.runtime_dir.join("control-plane.sock"));
    let ownership = familiar_ai_daemon::worker_lock::WorkerLock::acquire_with_socket(
        &paths.runtime_dir,
        &requested_socket,
    )?;
    std::fs::write(
        &persistent_generation,
        format!("{}\n", ownership.claim().generation),
    )?;
    if !persistent_installation.exists() {
        std::fs::write(
            &persistent_installation,
            format!("{}\n", ownership.claim().installation_id),
        )?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &persistent_installation,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )?;
    }

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
        db_lock.conn().execute(
            "INSERT OR IGNORE INTO control_plane_installation(singleton,installation_id,created_at) VALUES(1,?1,datetime('now'))",
            [&ownership.claim().installation_id],
        ).map_err(|e| familiar_ai_core::FamiliarError::Database(e.to_string()))?;
        let stored_installation: String = db_lock
            .conn()
            .query_row(
                "SELECT installation_id FROM control_plane_installation WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| familiar_ai_core::FamiliarError::Database(e.to_string()))?;
        if stored_installation != ownership.claim().installation_id {
            return Err(familiar_ai_core::FamiliarError::Config("control-plane installation identity disagrees with the durable installation record; restore the matching identity or database backup".into()));
        }
        db_lock.conn().execute("UPDATE control_plane_claim_generations SET generation=?1 WHERE singleton=1 AND generation<?1",[ownership.claim().generation as i64]).map_err(|e|familiar_ai_core::FamiliarError::Database(e.to_string()))?;
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

    let control = familiar_ai_daemon::control_plane::ControlPlaneService::new(
        db.clone(),
        familiar_ai_core::control_plane::SchedulingPolicy {
            global_ceiling: config.daemon.global_concurrency_ceiling,
            default_project_ceiling: config.daemon.default_project_concurrency_ceiling,
        },
        ownership.claim().generation,
    );
    let survivors = familiar_ai_daemon::control_worker::verified_live_workers(&control)?;
    control.recover(&survivors)?;
    control.reconcile_filesystem()?;
    let internal = familiar_ai_core::control_plane::CapabilityScope {
        client_class: familiar_ai_core::control_plane::ClientClass::Internal,
        project_id: None,
        execution_id: None,
        attempt: None,
        worker_id: None,
        authorities: vec![familiar_ai_core::control_plane::Authority::Control],
    };
    let operator = control.mint_session(
        &internal,
        familiar_ai_core::control_plane::CapabilityScope {
            client_class: familiar_ai_core::control_plane::ClientClass::Operator,
            project_id: None,
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![
                familiar_ai_core::control_plane::Authority::Control,
                familiar_ai_core::control_plane::Authority::Observe,
            ],
        },
        24 * 60 * 60,
    )?;
    let credential_path = paths.runtime_dir.join("operator.credential");
    std::fs::write(&credential_path, operator.credential.as_bytes())?;
    #[cfg(unix)]
    std::fs::set_permissions(
        &credential_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )?;
    let control_socket = config
        .daemon
        .socket_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(&ownership.claim().socket_path));

    Ok((
        DaemonState {
            ownership,
            config,
            config_path,
            paths,
            db,
            status,
            pid_path,
            router,
            control,
            control_socket,
        },
        log_guard,
    ))
}

/// Spawn the watcher, heartbeat, and command handler. Returns when shutdown_rx fires.
async fn daemon_run(
    state: &DaemonState,
    termination: &mut TerminationSignals,
    command_rx: mpsc::Receiver<DaemonCommand>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let _control_host = match familiar_ai_daemon::local_transport::LocalHost::bind(
        &state.control_socket,
        state.ownership.claim().owner_nonce.clone(),
        state.control.clone(),
    )
    .await
    {
        Ok(host) => Some(host),
        Err(error) => {
            tracing::error!(error=%error, "control-plane socket failed closed");
            return;
        }
    };
    let control_worker = tokio::spawn(familiar_ai_daemon::control_worker::run(
        state.control.clone(),
        state.paths.runtime_dir.join("capabilities"),
        shutdown_rx.clone(),
    ));
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
        let context_service = familiar_ai_daemon::context_service::ContextService::with_cache_dir(
            state.paths.data_dir.join("repomaps"),
        );
        let handler_task = tokio::spawn(async move {
            handle_watcher_events(
                event_rx,
                db_clone,
                status_clone,
                summary_tx_clone,
                max_size,
                context_service,
            )
            .await;
        });

        Some((watcher_task, handler_task))
    } else {
        tracing::info!("file watcher disabled or no paths configured");
        None
    };

    // Spawn the PRD-071 batch-review poller. It exits immediately (a no-op
    // task) when no repository configures a batch-tier worker, so this is
    // always safe to spawn.
    let batch_review_handle = tokio::spawn(familiar_ai_daemon::batch_review::run(
        state.db.clone(),
        state.config.clone(),
        state.paths.clone(),
        shutdown_rx.clone(),
    ));

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
        _ = shutdown_signal(termination) => {
            tracing::info!("os shutdown signal received");
        }
        _ = shutdown_rx_local.changed() => {
            tracing::info!("internal shutdown received");
        }
    }

    let _ = shutdown_tx.send(true);
    heartbeat_handle.abort();
    command_handle.abort();

    // Drain the independent subsystems CONCURRENTLY. Awaiting three
    // five-second timeouts in sequence made worst-case graceful shutdown
    // fifteen seconds, which overruns a supervisor's TERM budget (and the
    // integration test's ten) even though nothing here depends on anything
    // else finishing first (FAM-BUG-050). Concurrent draining bounds the
    // whole shutdown at one timeout.
    const DRAIN: Duration = Duration::from_secs(5);
    let watcher_drain = async {
        if let Some((watcher_task, handler_task)) = watcher_handle {
            let _ = tokio::time::timeout(DRAIN, watcher_task).await;
            handler_task.abort();
        }
    };
    let summary_drain = async {
        if let Some(handle) = summary_handle {
            let _ = tokio::time::timeout(DRAIN, handle).await;
        }
    };
    tokio::join!(
        async {
            let _ = tokio::time::timeout(DRAIN, control_worker).await;
        },
        // PRD-071's batch-review poller joins the concurrent drain rather
        // than adding another sequential timeout to the shutdown path.
        async {
            let _ = tokio::time::timeout(DRAIN, batch_review_handle).await;
        },
        watcher_drain,
        summary_drain,
    );
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

    let code = runtime.block_on(async {
        match run_headless().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("fatal: {e}");
                ExitCode::FAILURE
            }
        }
    });
    // Dropping a multi-thread runtime waits for blocking tasks with no
    // bound, so a parked `spawn_blocking` (a watcher read, a SQLite call)
    // keeps the process alive after every graceful step has finished — the
    // daemon looked hung to any supervisor timing its shutdown
    // (FAM-BUG-050). Bound the teardown; the work is already signalled.
    runtime.shutdown_timeout(Duration::from_secs(2));
    code
}

#[cfg(not(feature = "tray"))]
async fn run_headless() -> familiar_ai_core::Result<()> {
    // Register termination handling BEFORE bootstrap writes the PID file:
    // that file is how a supervisor learns it may signal us, and until the
    // handler exists SIGTERM kills us outright (FAM-BUG-050).
    let mut termination = TerminationSignals::register()?;
    let (state, _log_guard) = bootstrap()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (_command_tx, command_rx) = mpsc::channel::<DaemonCommand>(64);

    daemon_run(
        &state,
        &mut termination,
        command_rx,
        shutdown_tx,
        shutdown_rx,
    )
    .await;

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
            // Tray build: bootstrap ran on the main thread before any runtime
            // existed, so registration happens here, first thing on the
            // daemon runtime — the narrow window is inherent to that build
            // shape and is documented in FAM-BUG-050.
            let mut termination =
                TerminationSignals::register().expect("register termination signals");
            daemon_run(
                &state_for_daemon,
                &mut termination,
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
            let opt = daemon_command_from_tray(tc);
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
    context_service: familiar_ai_daemon::context_service::ContextService,
) {
    while let Some(event) = rx.recv().await {
        context_service.apply(&event);
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
