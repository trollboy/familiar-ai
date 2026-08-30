use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use familiar_ai_core::{
    load_manifest, resolve_run_prd, validate_graph, validate_recovery_attribution, AppPaths,
    BacklogDiscovery, BacklogManager, BacklogRecoveryAction, BacklogStatusStore,
    BootstrapApplyResult, Config, FilesystemBacklogDiscovery, ProfiledFilesystemBacklogDiscovery,
};
use familiar_ai_daemon::drive::{drive, DriveSummary, DriveWarrant};
use familiar_ai_daemon::plan::{
    approve as approve_plan, generate as generate_plan, print_summary, reject as reject_plan,
};
use familiar_ai_daemon::run::{
    build_agent, execute_with_config, resolved_agent_entries, resolved_remediation_entry, AgentSet,
};
use familiar_ai_storage::{
    Database, ExecutionHistoryRepository, SqliteBacklogRepository, SqliteBootstrapRepository,
};

#[derive(Debug, Parser)]
#[command(name = "familiar-ai", about = "Familiar command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate prerequisites without claiming a PRD or invoking a model.
    Preflight,
    /// Select the next eligible repository PRD without executing it.
    Next,
    /// Execute a repository PRD with the configured coding agent.
    Run { prd_path: PathBuf },
    /// Continue one durable partial, or inspect/schedule all durable partials.
    Resume {
        /// PRD identifier (for example PRD-123), or `all`.
        prd: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Execute eligible backlog PRDs unattended until the backlog is empty,
    /// nothing is eligible, or the budget warrant is exhausted. Flags may only
    /// tighten the configured warrant, never loosen it.
    Drive {
        #[arg(long)]
        max_prds: Option<u64>,
        #[arg(long)]
        max_cost_microusd: Option<u64>,
        #[arg(long)]
        max_duration_ms: Option<u64>,
        #[arg(long)]
        max_parallel_components: Option<usize>,
        #[arg(long)]
        worktree_root: Option<PathBuf>,
    },
    /// List recent standalone executions.
    History {
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u8).range(1..=100))]
        limit: u8,
        #[arg(long)]
        verbose: bool,
    },
    /// Summarize known standalone execution usage and cost.
    Usage,
    /// Render one unattended driver session: what got built, what stopped and
    /// why, what it cost, and what needs human judgment. Defaults to the most
    /// recent session.
    Report { session_id: Option<String> },
    /// Publish, check, merge, deploy to staging, and smoke-test one reviewed
    /// worktree under the configured finite delivery policy.
    Deliver { ownership_record: PathBuf },
    /// Install and operate a bounded native-supervised worker.
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
    /// Inspect or roll back the historical backlog bootstrap.
    Backlog {
        #[command(subcommand)]
        command: BacklogCommand,
    },
    /// Draft or decide a human-reviewed PRD proposal batch.
    Plan {
        #[command(subcommand)]
        command: Option<PlanCommand>,
        /// Design documents supplied to the configured planner agent.
        design_docs: Vec<PathBuf>,
    },
    /// Query durable execution-era state (backlog, sessions, attempts,
    /// worktrees, review findings, budgets, delivery, recovery events, and
    /// pending human gates) for the current repository. Read-only; prints
    /// one JSON object per invocation.
    Stewardship {
        #[command(subcommand)]
        command: StewardshipCommand,
    },
}

#[derive(Debug, Subcommand)]
enum StewardshipCommand {
    /// List the backlog graph.
    Backlog {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List driver sessions, most recent first.
    Sessions {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List one session's attempts, including worktree/branch identity.
    Attempts {
        session_id: String,
        #[arg(long)]
        cursor: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List execution checkpoints (worktree/branch identity, recovery phase).
    Checkpoints {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List audited backlog recovery events.
    Recovery {
        #[arg(long)]
        cursor: Option<i64>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List delivery authority decisions.
    Delivery {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show one session's warrant, cost, and delivery-warrant consumption.
    Budget { session_id: String },
    /// Show review disposition and blocking scope findings for one session.
    Review { session_id: String },
    /// List stopped attempts and blocked checkpoints awaiting a human decision.
    Gates {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
enum PlanCommand {
    /// Re-validate and admit a proposal batch to the ordinary backlog.
    Approve {
        batch_id: String,
        #[arg(long)]
        actor: String,
    },
    /// Record a rejection and remove its proposal files.
    Reject {
        batch_id: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
enum BacklogCommand {
    /// Deterministically check structured PRD metadata migration state. Never writes PRDs.
    MetadataCheck,
    Bootstrap {
        #[command(subcommand)]
        command: BootstrapCommand,
    },
    /// Return one claimed PRD to pending without altering its claim or worktree history.
    Release {
        prd_path: PathBuf,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
    /// MANUAL OVERRIDE: bypass PRD-011's normal completion-evidence predicate.
    Complete {
        prd_path: PathBuf,
        /// Mandatory explicit human authority in the form human:<identity>.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason for the manual override.
        #[arg(long)]
        reason: String,
    },
    /// Declare, under recorded human authority, that a `pending` PRD was
    /// completed outside Familiar's tracking (a fresh database, a restored
    /// machine, or work merged before Familiar tracked it).
    RecordComplete {
        prd_path: PathBuf,
        /// Mandatory explicit human authority in the form human:<identity>.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason for the recorded completion.
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    /// Install and start the native per-user supervisor definition.
    Install { repository: PathBuf },
    /// Stop and remove the definition, preserving logs, database, and history.
    Uninstall { repository: PathBuf },
    /// Show native supervisor state and every validation blocker.
    Status { repository: PathBuf },
    /// Validate the platform and definition without installing or claiming work.
    Validate { repository: PathBuf },
    /// Run a harmless fail-once fixture proving restart recovery and one report.
    Test,
    /// Generate a launchd plist using this exact executable.
    Plist {
        repository: PathBuf,
        #[arg(long)]
        label: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// launchd entry point: run one configured warrant and emit its report.
    Run {
        repository: PathBuf,
        #[arg(long, default_value_t = 1)]
        max_prds: u64,
    },
}

#[derive(Debug, Subcommand)]
enum BootstrapCommand {
    Status,
    Rollback {
        run_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        actor: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Preflight => match preflight_command() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Next => match next() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Plan {
            command,
            design_docs,
        } => match plan(command, &design_docs) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Run { prd_path } => match run(&prd_path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let code = error.exit_code();
                eprintln!("error: {error}");
                code.and_then(|value| u8::try_from(value).ok())
                    .map_or(ExitCode::FAILURE, ExitCode::from)
            }
        },
        Command::Resume { prd, dry_run } => match resume_command(&prd, dry_run) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Drive {
            max_prds,
            max_cost_microusd,
            max_duration_ms,
            max_parallel_components,
            worktree_root,
        } => match drive_command(
            max_prds,
            max_cost_microusd,
            max_duration_ms,
            max_parallel_components,
            worktree_root,
        ) {
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::History { limit, verbose } => match history(limit, verbose) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Usage => match usage() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Report { session_id } => match report_command(session_id.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Deliver { ownership_record } => match deliver_command(&ownership_record) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Worker { command } => match worker_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Stewardship { command } => match stewardship_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Backlog { command } => match backlog(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
    }
}

fn resume_command(prd: &str, dry_run: bool) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|e| e.to_string())?;
    let db_path = config.database.resolve_path(&paths.data_dir);
    let _ownership = if dry_run {
        None
    } else {
        Some(
            familiar_ai_daemon::worker_lock::WorkerLock::acquire_repository(
                &paths.runtime_dir,
                &repository.key,
            )
            .map_err(|e| format!("cannot acquire mutating orchestrator ownership: {e}"))?,
        )
    };
    let db = Database::open(&db_path).map_err(|e| e.to_string())?;
    if !dry_run {
        db.run_migrations().map_err(|e| e.to_string())?;
    }
    let repository_config = config.repository(&repository.worktree);
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| e.to_string())?;
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    let dependencies = discovered
        .iter()
        .map(|entry| {
            (
                entry.id.to_string(),
                entry.dependencies.iter().map(ToString::to_string).collect(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let completed = discovered
        .iter()
        .filter(|entry| entry.location == familiar_ai_core::PrdLocation::Archived)
        .map(|entry| entry.id.to_string())
        .collect::<BTreeSet<_>>();
    let active_prds = discovered
        .iter()
        .filter(|entry| entry.location == familiar_ai_core::PrdLocation::Active)
        .map(|entry| (entry.id.to_string(), entry.path.to_string()))
        .collect::<BTreeMap<_, _>>();
    let candidates = if prd == "all" {
        familiar_ai_daemon::resume::discover_with_legacy(
            &db,
            &repository.key,
            &paths.state_dir,
            &active_prds,
            !dry_run,
        )?
    } else {
        vec![familiar_ai_daemon::resume::one(&db, &repository.key, prd)?]
    };
    print!("{}", familiar_ai_daemon::resume::render(&candidates));
    let (waves, blocked) = if prd == "all" {
        familiar_ai_daemon::resume::plan_waves(
            &candidates,
            &dependencies,
            &completed,
            config.driver.max_concurrency,
        )
    } else if candidates[0].valid {
        (vec![vec![0]], Vec::new())
    } else {
        (
            Vec::new(),
            vec![(0, candidates[0].reason.clone().unwrap_or_default())],
        )
    };
    for (wave, indexes) in waves.iter().enumerate() {
        println!(
            "wave={}\t{}",
            wave + 1,
            indexes
                .iter()
                .map(|index| candidates[*index].prd_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    for (index, reason) in &blocked {
        println!("blocked\t{}\treason={reason}", candidates[*index].prd_id);
    }
    if dry_run {
        return Ok(());
    }
    if let Some(invalid) = candidates.iter().find(|c| !c.valid) {
        if prd != "all" {
            return Err(format!(
                "{}: {}",
                invalid.prd_id,
                invalid.reason.as_deref().unwrap_or("invalid_checkpoint")
            ));
        }
    }
    let (implementation_entry, reviewer_entry) = resolved_agent_entries(&config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    let remediation = build_agent(&resolved_remediation_entry(&config)?);
    let agents = AgentSet {
        implementation: implementation.as_ref(),
        reviewer: reviewer.as_ref(),
        remediation: remediation.as_ref(),
    };
    let mut failures = blocked
        .into_iter()
        .map(|(index, reason)| format!("{}: {reason}", candidates[index].prd_id))
        .collect::<Vec<_>>();
    let mut failed_prds = BTreeSet::new();
    for wave in waves {
        let runnable = wave
            .into_iter()
            .filter(|index| {
                let failed_dependencies = dependencies
                    .get(&candidates[*index].prd_id)
                    .into_iter()
                    .flatten()
                    .filter(|dependency| failed_prds.contains(*dependency))
                    .cloned()
                    .collect::<Vec<_>>();
                if failed_dependencies.is_empty() {
                    true
                } else {
                    failures.push(format!(
                        "{}: dependency_failed: {}",
                        candidates[*index].prd_id,
                        failed_dependencies.join(",")
                    ));
                    failed_prds.insert(candidates[*index].prd_id.clone());
                    false
                }
            })
            .collect::<Vec<_>>();
        let results = std::thread::scope(|scope| {
            let handles = runnable
                .iter()
                .map(|index| {
                    let candidate = &candidates[*index];
                    scope.spawn(|| {
                        let resumed = familiar_ai_daemon::run::resume_implemented_checkpoint(
                            &candidate.worktree,
                            &candidate.prd_id,
                            &agents,
                            &config,
                            &paths,
                        );
                        (
                            candidate.prd_id.clone(),
                            candidate.worktree.clone(),
                            resumed,
                        )
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join())
                .collect::<Vec<_>>()
        });
        for result in results {
            match result {
                Ok((_, _, Ok(_))) => {}
                Ok((id, worktree, Err(error))) => {
                    match handle_attached_review(Err(error), &worktree, &config, &paths, &agents) {
                        Ok(()) => {}
                        Err(error) => {
                            failed_prds.insert(id.clone());
                            failures.push(format!("{id}: {error}"));
                        }
                    }
                }
                Err(_) => failures.push("resume_worker_panicked".into()),
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn worker_command(command: WorkerCommand) -> Result<(), String> {
    match command {
        WorkerCommand::Install { repository } => {
            let (spec, repository, paths) = worker_spec(&repository)?;
            let changed =
                familiar_ai_daemon::supervisor::install(&spec, &repository, &paths.log_dir)?;
            println!(
                "installed={} changed={} definition={}",
                spec.label,
                changed,
                spec.definition.display()
            );
            Ok(())
        }
        WorkerCommand::Uninstall { repository } => {
            let (spec, _, _) = worker_spec(&repository)?;
            let removed = familiar_ai_daemon::supervisor::uninstall(&spec)?;
            println!(
                "uninstalled={} removed={} durable_history=preserved",
                spec.label, removed
            );
            Ok(())
        }
        WorkerCommand::Status { repository } => {
            let (spec, repository, _) = worker_spec(&repository)?;
            let status = familiar_ai_daemon::supervisor::status(&spec, &repository);
            println!(
                "backend={:?}\ninstalled={}\ndefinition={}\nstate={}",
                status.backend,
                status.installed,
                status.definition.display(),
                status.supervisor_state
            );
            for blocker in &status.blockers {
                println!("blocker={blocker}");
            }
            if status.blockers.is_empty() {
                Ok(())
            } else {
                Err(format!("worker has {} blocker(s)", status.blockers.len()))
            }
        }
        WorkerCommand::Validate { repository } => {
            let (spec, repository, _) = worker_spec(&repository)?;
            familiar_ai_daemon::supervisor::validate(&spec, &repository)
                .map_err(|v| v.join("; "))?;
            println!(
                "valid=true backend={:?} definition={}",
                spec.backend,
                spec.definition.display()
            );
            Ok(())
        }
        WorkerCommand::Test => {
            // Detection is deliberately first: unsupported hosts cannot even
            // begin the fixture, much less claim production work.
            let backend = familiar_ai_daemon::supervisor::detect()?;
            let root = std::env::temp_dir()
                .join(format!("familiar-ai-worker-fixture-{}", std::process::id()));
            let first = familiar_ai_daemon::supervisor::run_fixture(&root);
            if first.is_ok() {
                return Err("fixture did not request its failure restart".into());
            }
            let result = familiar_ai_daemon::supervisor::run_fixture(&root)?;
            let again = familiar_ai_daemon::supervisor::run_fixture(&root)?;
            std::fs::remove_dir_all(&root).map_err(|e| format!("cannot clean fixture: {e}"))?;
            if result != again {
                return Err("fixture recovery was not idempotent".into());
            }
            println!("backend={backend:?} {result}");
            Ok(())
        }
        WorkerCommand::Plist {
            repository,
            label,
            output,
        } => {
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let repository = repository
                .canonicalize()
                .map_err(|error| error.to_string())?;
            let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
            std::fs::create_dir_all(&paths.log_dir).map_err(|error| error.to_string())?;
            let toolchain_path = std::env::var("PATH")
                .map_err(|_| "PATH is required to generate a launchd worker plist".to_owned())?;
            let rendered = familiar_ai_daemon::launchd::plist(
                &label,
                &executable,
                &repository,
                &paths.log_dir.join(format!("{label}.stdout.log")),
                &paths.log_dir.join(format!("{label}.stderr.log")),
                &toolchain_path,
                10,
                1,
            )?;
            std::fs::write(&output, rendered).map_err(|error| error.to_string())?;
            println!("plist={}", output.display());
            Ok(())
        }
        WorkerCommand::Run {
            repository,
            max_prds,
        } => {
            std::env::set_current_dir(&repository).map_err(|error| error.to_string())?;
            let summary = drive_command(Some(max_prds), None, None, None, None)?;
            report_command(Some(&summary.session_id))?;
            if summary.termination.worker_should_restart() {
                return Err(format!(
                    "worker session {} requires supervisor restart after {}",
                    summary.session_id,
                    summary.termination.as_str()
                ));
            }
            Ok(())
        }
    }
}

fn worker_spec(
    repository: &std::path::Path,
) -> Result<(familiar_ai_daemon::supervisor::Spec, PathBuf, AppPaths), String> {
    // Platform detection must happen before canonicalization/config loading so
    // unsupported platforms fail before any work-like activity.
    familiar_ai_daemon::supervisor::detect()?;
    let executable = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let repository = repository
        .canonicalize()
        .map_err(|e| format!("cannot resolve repository {}: {e}", repository.display()))?;
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let spec =
        familiar_ai_daemon::supervisor::spec(&executable, &repository, &paths, &config.worker)?;
    Ok((spec, repository, paths))
}

fn deliver_command(ownership_record: &std::path::Path) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|e| e.to_string())?;
    let _ownership = familiar_ai_daemon::worker_lock::WorkerLock::acquire_repository(
        &paths.runtime_dir,
        &repository.key,
    )
    .map_err(|e| format!("cannot acquire mutating orchestrator ownership: {e}"))?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let repository_config = config.repository(&repository.worktree);
    let policy = repository_config.delivery_policy()?;
    let result = familiar_ai_daemon::delivery::deliver(ownership_record, policy)?;
    println!(
        "delivery_session={} prd={} phase={} pr={}",
        result.session_id,
        result.prd_id,
        result.phase,
        result
            .pr_number
            .map(|number| number.to_string())
            .unwrap_or_else(|| "unknown".into())
    );
    Ok(())
}

fn preflight_command() -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|error| error.to_string())?;
    let (implementation_entry, reviewer_entry) = resolved_agent_entries(&config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    let remediation = build_agent(&resolved_remediation_entry(&config)?);
    let report = familiar_ai_daemon::preflight::run(
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        },
        &config,
        &repository.worktree,
    );
    for check in &report.checks {
        let status = match check.status {
            familiar_ai_daemon::preflight::PreflightStatus::Passed => "passed",
            familiar_ai_daemon::preflight::PreflightStatus::Failed => "failed",
        };
        println!("{status}\t{}\t{}", check.check_id, check.detail);
    }
    if report.is_valid() {
        Ok(())
    } else {
        Err(format!("preflight failed: {}", report.failure_summary()))
    }
}

/// The CLI composition root: read validated configuration and construct the
/// implementation and reviewer agents deterministically.
fn run(prd_path: &std::path::Path) -> Result<(), familiar_ai_daemon::run::RunError> {
    use familiar_ai_daemon::run::RunError;
    let paths = AppPaths::resolve().map_err(|e| RunError::Config(e.to_string()))?;
    let current = std::env::current_dir().map_err(RunError::CurrentDirectory)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|e| RunError::Config(e.to_string()))?;
    let _ownership = familiar_ai_daemon::worker_lock::WorkerLock::acquire_repository(
        &paths.runtime_dir,
        &repository.key,
    )
    .map_err(|e| {
        RunError::Config(format!(
            "cannot acquire mutating orchestrator ownership: {e}"
        ))
    })?;
    let config = Config::load(Some(&paths.config_dir.join("config.toml")))
        .map_err(|e| RunError::Config(e.to_string()))?;
    let (implementation_entry, reviewer_entry) =
        resolved_agent_entries(&config).map_err(RunError::Config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    let remediation = build_agent(&resolved_remediation_entry(&config).map_err(RunError::Config)?);
    let result = execute_with_config(
        prd_path,
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        },
        &config,
        &paths,
    );
    handle_attached_review(
        result,
        &repository.worktree,
        &config,
        &paths,
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        },
    )
}

fn handle_attached_review(
    mut result: Result<
        familiar_ai_daemon::run::RunWorkflowResult,
        familiar_ai_daemon::run::RunError,
    >,
    worktree: &std::path::Path,
    config: &Config,
    paths: &AppPaths,
    agents: &AgentSet<'_>,
) -> Result<(), familiar_ai_daemon::run::RunError> {
    loop {
        match result {
            Ok(_) => return Ok(()),
            Err(familiar_ai_daemon::run::RunError::HumanReviewRequired {
                result: implementation,
                cycle,
                prd_id,
            }) => {
                eprintln!("HumanReviewRequired prd={prd_id}");
                eprintln!(
                    "stop_reasons={}",
                    serde_json::to_string(&cycle.stop_reasons).unwrap_or_else(|_| "[]".into())
                );
                if let Some(review) = &cycle.review_result {
                    for finding in &review.findings {
                        eprintln!(
                            "finding {} {:?}: {}",
                            finding.finding_id, finding.severity, finding.title
                        );
                    }
                }
                for finding in cycle
                    .scope_evaluations
                    .iter()
                    .flat_map(|evaluation| &evaluation.findings)
                {
                    eprintln!("scope_finding {}: {}", finding.rule_id, finding.rule_detail);
                }
                if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
                    eprintln!("non-interactive input: preserving checkpoint");
                    return Err(familiar_ai_daemon::run::RunError::HumanReviewRequired {
                        result: implementation,
                        cycle,
                        prd_id,
                    });
                }
                // Keystrokes pressed during the long silent phases would
                // otherwise be consumed as the choice; drop anything buffered
                // before asking.
                #[cfg(unix)]
                unsafe {
                    libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
                }
                eprint!("Choose [r]etry remediation, [a]ccept reviewed risk, or [p]reserve checkpoint: ");
                let _ = io::stderr().flush();
                let mut choice = String::new();
                if io::stdin().read_line(&mut choice).unwrap_or(0) == 0 {
                    eprintln!("EOF: preserving checkpoint");
                    return Err(familiar_ai_daemon::run::RunError::HumanReviewRequired {
                        result: implementation,
                        cycle,
                        prd_id,
                    });
                }
                match choice.trim().to_ascii_lowercase().as_str() {
                    "r" | "retry" => {
                        result = familiar_ai_daemon::run::resume_implemented_checkpoint(
                            worktree, &prd_id, agents, config, paths,
                        );
                    }
                    "a" | "accept" | "accept-risk" => {
                        eprint!("Actor accepting this exact risk (human:<identity>): ");
                        let _ = io::stderr().flush();
                        let mut actor = String::new();
                        if io::stdin().read_line(&mut actor).unwrap_or(0) == 0
                            || actor.trim().is_empty()
                        {
                            eprintln!("missing actor: preserving checkpoint");
                            return Err(familiar_ai_daemon::run::RunError::HumanReviewRequired {
                                result: implementation,
                                cycle,
                                prd_id,
                            });
                        }
                        familiar_ai_daemon::run::accept_review_risk(
                            worktree,
                            &prd_id,
                            actor.trim(),
                            &cycle,
                            config,
                            paths,
                        )?;
                        return Ok(());
                    }
                    "p" | "preserve" => {
                        return Err(familiar_ai_daemon::run::RunError::HumanReviewRequired {
                            result: implementation,
                            cycle,
                            prd_id,
                        })
                    }
                    _ => {
                        eprintln!("unknown choice: preserving checkpoint");
                        return Err(familiar_ai_daemon::run::RunError::HumanReviewRequired {
                            result: implementation,
                            cycle,
                            prd_id,
                        });
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Composition root for the unattended driver: same agent construction as
/// `run`, plus a warrant that flags may only tighten.
fn drive_command(
    max_prds: Option<u64>,
    max_cost_microusd: Option<u64>,
    max_duration_ms: Option<u64>,
    max_parallel_components: Option<usize>,
    worktree_root: Option<PathBuf>,
) -> Result<DriveSummary, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let mut config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    if let Some(value) = max_parallel_components {
        if value == 0 {
            return Err("--max-parallel-components must be positive".into());
        }
        config.driver.max_parallel_components = value;
    }
    if let Some(value) = worktree_root {
        config.driver.worktree_root = value.to_string_lossy().into_owned();
    }
    let (implementation_entry, reviewer_entry) = resolved_agent_entries(&config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    let remediation = build_agent(&resolved_remediation_entry(&config)?);
    let warrant = DriveWarrant::from_config(&config).tightened_by(
        max_prds,
        max_cost_microusd,
        max_duration_ms,
    );
    let summary = drive(
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        },
        &config,
        &paths,
        warrant,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "session={} termination={} attempted={} completed={} known_cost_microusd={}",
        summary.session_id,
        summary.termination.as_str(),
        summary.attempted,
        summary.completed,
        summary.known_cost_microusd
    );
    Ok(summary)
}

/// Read-only: renders recorded rows and constructs no agents.
fn report_command(session_id: Option<&str>) -> Result<(), String> {
    let db = database()?;
    let rendered =
        familiar_ai_daemon::report::render(&db, session_id).map_err(|e| e.to_string())?;
    print!("{rendered}");
    Ok(())
}

/// Read-only: prints one JSON object over durable execution-era state,
/// scoped to the repository resolved from the current directory exactly as
/// every other backlog-aware command resolves it. Shares its query
/// implementation with the dashboard's `/stewardship/*` endpoints.
fn stewardship_command(command: StewardshipCommand) -> Result<(), String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let db = database()?;
    let value = match command {
        StewardshipCommand::Backlog {
            status,
            cursor,
            limit,
        } => familiar_ai_daemon::stewardship::list_backlog(
            &db,
            &repository,
            status.as_deref(),
            cursor.as_deref(),
            limit,
        ),
        StewardshipCommand::Sessions { cursor, limit } => {
            familiar_ai_daemon::stewardship::list_sessions(
                &db,
                &repository,
                cursor.as_deref(),
                limit,
            )
        }
        StewardshipCommand::Attempts {
            session_id,
            cursor,
            limit,
        } => familiar_ai_daemon::stewardship::list_attempts(
            &db,
            &repository,
            &session_id,
            cursor,
            limit,
        ),
        StewardshipCommand::Checkpoints { cursor, limit } => {
            familiar_ai_daemon::stewardship::list_checkpoints(
                &db,
                &repository,
                cursor.as_deref(),
                limit,
            )
        }
        StewardshipCommand::Recovery { cursor, limit } => {
            familiar_ai_daemon::stewardship::list_recovery_events(&db, &repository, cursor, limit)
        }
        StewardshipCommand::Delivery { cursor, limit } => {
            familiar_ai_daemon::stewardship::list_delivery_decisions(
                &db,
                &repository,
                cursor.as_deref(),
                limit,
            )
        }
        StewardshipCommand::Budget { session_id } => {
            familiar_ai_daemon::stewardship::get_budget(&db, &repository, &session_id)
        }
        StewardshipCommand::Review { session_id } => {
            familiar_ai_daemon::stewardship::list_review_findings(&db, &repository, &session_id)
        }
        StewardshipCommand::Gates { limit } => {
            familiar_ai_daemon::stewardship::list_pending_human_gates(&db, &repository, limit)
        }
    }
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn next() -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    // Resolve Git before opening or migrating storage, preserving the domain's
    // required operation order for invalid working directories.
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let repository_config = config.repository(&repository.worktree);
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| e.to_string())?;
    if discovered.is_empty() {
        return Err("backlog is empty".into());
    }
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    let mut db = database()?;
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&repository, &discovered)
        .map_err(|e| e.to_string())?;
    let manifest = load_manifest(&repository, &discovered).map_err(|e| e.to_string())?;
    let applied = SqliteBootstrapRepository::new(db.conn_mut())
        .apply(&repository, &discovered, manifest.as_ref())
        .map_err(|e| e.to_string())?;
    if let BootstrapApplyResult::Applied(run) = applied {
        eprintln!(
            "historical backlog bootstrap applied: run={} items={} manifest={}",
            run.run_id, run.item_count, run.canonical_hash
        );
    }
    let store = SqliteBacklogRepository::new(db.conn_mut());
    let mut manager = BacklogManager::new(
        ProfiledFilesystemBacklogDiscovery {
            layout: repository_config.layout(),
        },
        store,
    );
    let selected = manager.next(&cwd).map_err(|e| e.to_string())?;
    println!(
        "{}\t{}\t{}\t{}",
        selected.id,
        selected.path,
        selected.status.as_str(),
        selected.title
    );
    Ok(())
}

fn plan(command: Option<PlanCommand>, design_docs: &[PathBuf]) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    paths.ensure_dirs().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&root)
        .map_err(|e| e.to_string())?;
    let mut db = database()?;
    let limits = config.planner.as_ref().ok_or("[planner] is required")?;
    match command {
        None => {
            let agent = build_agent(&limits.agent);
            let (id, summary) = generate_plan(
                &repository.worktree,
                design_docs,
                &config,
                &paths,
                &db,
                agent.as_ref(),
            )?;
            print_summary(&id, &summary);
        }
        Some(PlanCommand::Approve { batch_id, actor }) => {
            if !design_docs.is_empty() {
                return Err("design documents are not accepted by plan approve".into());
            }
            let summary = approve_plan(
                &repository.worktree,
                &batch_id,
                &actor,
                limits,
                &repository,
                &mut db,
            )?;
            print_summary(&batch_id, &summary);
        }
        Some(PlanCommand::Reject {
            batch_id,
            actor,
            reason,
        }) => {
            if !design_docs.is_empty() {
                return Err("design documents are not accepted by plan reject".into());
            }
            reject_plan(
                &repository.worktree,
                &batch_id,
                &actor,
                &reason,
                &repository,
                &mut db,
            )?;
            println!("Batch {batch_id} rejected");
        }
    }
    Ok(())
}

fn backlog(command: BacklogCommand) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let repository_config = config.repository(&repository.worktree);
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| e.to_string())?;
    if discovered.is_empty() {
        return Err("backlog is empty".into());
    }
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    if matches!(command, BacklogCommand::MetadataCheck) {
        let mut legacy = Vec::new();
        for prd in &discovered {
            if prd.metadata.contract_version == Some(1) {
                println!("{}: structured-v1", prd.path);
            } else {
                println!("{}: legacy: add familiar_ai_prd v1 front matter", prd.path);
                legacy.push(prd.path.to_string());
            }
        }
        if !legacy.is_empty() {
            return Err(format!(
                "{} legacy PRD(s) require migration under policy={}: {}",
                legacy.len(),
                repository_config.prd_metadata_policy,
                legacy.join(", ")
            ));
        }
        return Ok(());
    }
    let mut db = database()?;
    match command {
        BacklogCommand::MetadataCheck => unreachable!("handled before storage is opened"),
        BacklogCommand::Bootstrap {
            command: BootstrapCommand::Status,
        } => {
            let manifest = load_manifest(&repository, &discovered).map_err(|e| e.to_string())?;
            let report = SqliteBootstrapRepository::new(db.conn_mut())
                .status(&repository, manifest.as_ref())
                .map_err(|e| e.to_string())?;
            println!(
                "state={} repository={} run={} manifest={} items={}",
                report.state,
                report.repository_key,
                report.run_id.as_deref().unwrap_or("-"),
                report.canonical_hash.as_deref().unwrap_or("-"),
                report.item_count
            );
        }
        BacklogCommand::Bootstrap {
            command:
                BootstrapCommand::Rollback {
                    run_id,
                    reason,
                    actor,
                },
        } => {
            SqliteBacklogRepository::new(db.conn_mut())
                .reconcile_and_snapshot(&repository, &discovered)
                .map_err(|e| e.to_string())?;
            let result = SqliteBootstrapRepository::new(db.conn_mut())
                .rollback(&repository, &discovered, &run_id, &actor, &reason)
                .map_err(|e| e.to_string())?;
            println!(
                "rollback={} restored={}",
                result.rollback_run_id, result.item_count
            );
        }
        BacklogCommand::Release {
            prd_path,
            actor,
            reason,
        } => recover_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            BacklogRecoveryAction::Release,
            &actor,
            &reason,
        )?,
        BacklogCommand::Complete {
            prd_path,
            actor,
            reason,
        } => recover_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            BacklogRecoveryAction::ManualCompleteOverride,
            &actor,
            &reason,
        )?,
        BacklogCommand::RecordComplete {
            prd_path,
            actor,
            reason,
        } => record_complete_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            &actor,
            &reason,
        )?,
    }
    Ok(())
}

fn recover_backlog(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    discovered: &[familiar_ai_core::DiscoveredPrd],
    supplied_path: &std::path::Path,
    action: BacklogRecoveryAction,
    actor: &str,
    reason: &str,
) -> Result<(), String> {
    validate_recovery_attribution(action, actor, reason).map_err(|e| e.to_string())?;
    let target =
        resolve_run_prd(repository, discovered, supplied_path).map_err(|e| e.to_string())?;
    let result = SqliteBacklogRepository::new(db.conn_mut())
        .recover(repository, &target, action, actor, reason)
        .map_err(|e| e.to_string())?;
    let actor = actor.trim();
    let reason = reason.trim();
    let label = if action == BacklogRecoveryAction::ManualCompleteOverride {
        " MANUAL OVERRIDE"
    } else {
        ""
    };
    println!(
        "backlog recovery:{label} {} {} in_progress -> {} action={} actor={} reason={}",
        result.prd.id,
        result.prd.path,
        result.status.as_str(),
        action.as_str(),
        escape_output(actor),
        escape_output(reason)
    );
    Ok(())
}

fn record_complete_backlog(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    discovered: &[familiar_ai_core::DiscoveredPrd],
    supplied_path: &std::path::Path,
    actor: &str,
    reason: &str,
) -> Result<(), String> {
    validate_recovery_attribution(BacklogRecoveryAction::RecordedComplete, actor, reason)
        .map_err(|e| e.to_string())?;
    let target =
        resolve_run_prd(repository, discovered, supplied_path).map_err(|e| e.to_string())?;
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(repository, discovered)
        .map_err(|e| e.to_string())?;
    let result = SqliteBacklogRepository::new(db.conn_mut())
        .record_complete(repository, discovered, &target, actor, reason)
        .map_err(|e| e.to_string())?;
    let actor = actor.trim();
    let reason = reason.trim();
    println!(
        "backlog recovery: {} {} pending -> {} action={} actor={} reason={}",
        result.prd.id,
        result.prd.path,
        result.status.as_str(),
        BacklogRecoveryAction::RecordedComplete.as_str(),
        escape_output(actor),
        escape_output(reason)
    );
    Ok(())
}

fn escape_output(value: &str) -> String {
    format!("{value:?}")
}

fn database() -> Result<Database, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    Ok(db)
}
fn history(limit: u8, verbose: bool) -> Result<(), String> {
    let db = database()?;
    let rows = ExecutionHistoryRepository::new(db.conn())
        .recent(limit)
        .map_err(|e| e.to_string())?;
    if rows.is_empty() {
        println!("No execution history.");
        return Ok(());
    }
    for row in rows {
        let duration = row
            .duration_ms
            .map(|v| format!("{v}ms"))
            .unwrap_or_else(|| "—".into());
        let model = row.model.as_deref().unwrap_or("—");
        let status = match (row.exit_code, row.signal) {
            (Some(code), _) => format!("{} ({code})", row.outcome),
            (None, Some(signal)) => format!("{} (signal {signal})", row.outcome),
            _ if row.outcome == "running" => "running/incomplete".into(),
            _ => row.outcome.clone(),
        };
        println!(
            "{}  {}  {}  {}  {}  {}  {}  {}  {}",
            row.execution_id,
            row.started_at,
            duration,
            row.agent,
            model,
            status,
            row.repository,
            row.worktree,
            row.prd_path
        );
        if verbose {
            for (field, reason) in row.unavailable_fields {
                println!("  {field}: — ({reason})");
            }
        }
    }
    Ok(())
}
fn usage() -> Result<(), String> {
    let db = database()?;
    let u = ExecutionHistoryRepository::new(db.conn())
        .usage()
        .map_err(|e| e.to_string())?;
    println!("Executions: {}", u.execution_count);
    println!("Executions with complete usage: {}", u.complete_usage);
    println!("Executions with unknown usage: {}", u.unknown_usage);
    println!("Known input tokens: {}", u.known_input_tokens);
    println!("Known output tokens: {}", u.known_output_tokens);
    println!("Known cached tokens: {}", u.known_cached_tokens);
    if u.cache_measured_input_tokens > 0 {
        println!(
            "Cached input share: {:.2}% ({} measured execution(s))",
            u.known_cached_tokens as f64 * 100.0 / u.cache_measured_input_tokens as f64,
            u.cache_measured_executions
        );
    } else {
        println!("Cached input share: — (no measured input/cache pairs)");
    }
    println!(
        "Cache-unmeasured executions: {}",
        u.cache_unmeasured_executions
    );
    println!(
        "Known cache savings: {} micro-USD ({} execution(s), persisted execution-history pricing)",
        u.known_cache_savings_microusd, u.cache_savings_priced_executions
    );
    println!(
        "Cache-savings attempts without pricing provenance: {}",
        u.cache_savings_unpriced_executions
    );
    println!("Known total tokens: {}", u.known_total_tokens);
    println!("Executions with known cost: {}", u.known_cost_executions);
    println!(
        "Executions with unknown cost: {}",
        u.unknown_cost_executions
    );
    println!(
        "Known estimated cost: {} micro-USD (${:.6})",
        u.known_cost_microusd,
        u.known_cost_microusd as f64 / 1_000_000.0
    );
    Ok(())
}
fn fail(error: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {error}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_commands() {
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "history", "--limit", "100"])
                .unwrap()
                .command,
            Command::History { limit: 100, .. }
        ));
        assert!(Cli::try_parse_from(["familiar-ai", "history", "--limit", "0"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "usage"])
                .unwrap()
                .command,
            Command::Usage
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "next"])
                .unwrap()
                .command,
            Command::Next
        ));
        assert!(Cli::try_parse_from(["familiar-ai", "next", "PRD-1.md"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "backlog", "metadata-check"])
                .unwrap()
                .command,
            Command::Backlog {
                command: BacklogCommand::MetadataCheck
            }
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "familiar-ai",
                "backlog",
                "complete",
                "docs/prds/PRD-012.md",
                "--actor",
                "human:alice",
                "--reason",
                "manual acceptance"
            ])
            .unwrap()
            .command,
            Command::Backlog {
                command: BacklogCommand::Complete { .. }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "familiar-ai",
                "backlog",
                "record-complete",
                "docs/prds/PRD-014.md",
                "--actor",
                "human:trollboy",
                "--reason",
                "implemented, reviewed, and merged before this database existed"
            ])
            .unwrap()
            .command,
            Command::Backlog {
                command: BacklogCommand::RecordComplete { .. }
            }
        ));
    }
}
