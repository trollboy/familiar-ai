use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use familiar_ai_core::onboarding;
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
    /// Manage provider endpoints and enabled models without handling credentials.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Show repository project-configuration approval and binding state.
    Status {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
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
    /// List or decide one hash-bound pending scope finding.
    ScopeDecisions {
        #[arg(long)]
        finding_hash: Option<String>,
        #[arg(long)]
        candidate_hash: Option<String>,
        #[arg(long, conflicts_with = "reject")]
        approve: bool,
        #[arg(long, conflicts_with = "approve")]
        reject: bool,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        reason: Option<String>,
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
        /// Approved PRD identifier (repeatable, e.g. --prd PRD-065). When
        /// given, the session may select ONLY these PRDs; selection can never
        /// escape the recorded set.
        #[arg(long = "prd")]
        prd: Vec<String>,
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
    Deliver {
        ownership_record: PathBuf,
        /// Resolve and execute the repository-bound environment role.
        #[arg(long)]
        to: Option<String>,
    },
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
    /// Discover and approve repository-owned policy without claiming work.
    Onboard {
        #[command(subcommand)]
        command: OnboardCommand,
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
enum ConfigCommand {
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// Show durable configuration mutation decisions.
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Approve or revoke the exact current familiar.toml snapshot.
    Project {
        #[command(subcommand)]
        command: ProjectConfigCommand,
    },
    /// Show the approval-aware three-layer configuration with provenance.
    Show {
        #[arg(long)]
        effective: bool,
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectConfigCommand {
    Approve {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        actor: String,
    },
    Revoke {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        actor: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    Add {
        name: String,
        #[arg(long, default_value = "inference")]
        kind: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        via: Option<String>,
        #[arg(long)]
        recipe: Option<String>,
        #[arg(long)]
        actor: Option<String>,
    },
    Remove {
        name: String,
        #[arg(long)]
        actor: Option<String>,
    },
    Verify {
        name: String,
        #[arg(long)]
        actor: Option<String>,
    },
    List {
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Bind a declared environment name to a machine-local provider.
    Bind {
        role: String,
        provider: String,
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        actor: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    Enable {
        model: String,
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        capabilities: Vec<String>,
        #[arg(long)]
        actor: Option<String>,
    },
    Disable {
        model: String,
        #[arg(long)]
        actor: Option<String>,
    },
    List,
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
enum OnboardCommand {
    /// Write untrusted discovery proposals. Grants no authority.
    Propose {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long, default_value = "onboarding-proposal.toml")]
        output: PathBuf,
    },
    /// Convert an explicit deterministic answers file into attributed policy.
    Approve {
        proposal: PathBuf,
        #[arg(long)]
        answers: PathBuf,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        repositories_dir: Option<PathBuf>,
    },
    /// Validate one policy snapshot without storage, a PRD claim, or a model.
    Validate { policy: PathBuf },
    /// Run the harmless onboarding boundary fixture.
    Fixture { policy: PathBuf },
}

#[derive(Debug, Subcommand)]
enum BacklogCommand {
    /// Deterministically check structured PRD metadata migration state. Never writes PRDs.
    MetadataCheck {
        /// Report legacy migration debt and fail only on structured-v1 diagnostics.
        #[arg(long, conflicts_with = "strict")]
        advisory: bool,
        /// Fail on legacy migration debt as well as structured-v1 diagnostics (default).
        #[arg(long, conflicts_with = "advisory")]
        strict: bool,
    },
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
    /// Complete one reviewed retained checkpoint in a single transaction,
    /// binding the approved candidate hash and the resulting commit. Accepts
    /// an entry left pending (after a release) or in_progress.
    ApproveAndComplete {
        prd_path: PathBuf,
        /// Mandatory explicit human authority in the form human:<identity>.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason for the approval.
        #[arg(long)]
        reason: String,
        /// The commit the approved candidate landed as. Defaults to the
        /// repository's current HEAD.
        #[arg(long)]
        commit: Option<String>,
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
        Command::Config { command } => match config_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Status { repository } => match familiar_ai_daemon::config_cli::execute(
            familiar_ai_daemon::config_cli::ConfigAction::Status { repository },
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
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
        Command::Onboard { command } => match onboard(command) {
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
        Command::ScopeDecisions {
            finding_hash,
            candidate_hash,
            approve,
            reject,
            actor,
            reason,
        } => match scope_decisions(finding_hash, candidate_hash, approve, reject, actor, reason) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Drive {
            max_prds,
            max_cost_microusd,
            max_duration_ms,
            max_parallel_components,
            worktree_root,
            prd,
        } => match drive_command(
            max_prds,
            max_cost_microusd,
            max_duration_ms,
            max_parallel_components,
            worktree_root,
            prd,
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
        Command::Deliver {
            ownership_record,
            to,
        } => match deliver_command(&ownership_record, to.as_deref()) {
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

fn config_command(command: ConfigCommand) -> Result<(), String> {
    use familiar_ai_daemon::config_cli::{execute, ConfigAction};
    let action = match command {
        ConfigCommand::Provider { command } => match command {
            ProviderCommand::Add {
                name,
                kind,
                host,
                auth,
                via,
                recipe,
                actor,
            } => ConfigAction::ProviderAdd {
                name,
                kind,
                host,
                auth,
                via,
                recipe,
                actor,
            },
            ProviderCommand::Remove { name, actor } => ConfigAction::ProviderRemove { name, actor },
            ProviderCommand::Verify { name, actor } => ConfigAction::ProviderVerify { name, actor },
            ProviderCommand::List { refresh, actor } => {
                ConfigAction::ProviderList { refresh, actor }
            }
            ProviderCommand::Bind {
                role,
                provider,
                repository,
                actor,
            } => ConfigAction::ProviderBind {
                repository,
                role,
                provider,
                actor,
            },
        },
        ConfigCommand::Model { command } => match command {
            ModelCommand::Enable {
                model,
                capabilities,
                actor,
            } => ConfigAction::ModelEnable {
                model,
                capabilities,
                actor,
            },
            ModelCommand::Disable { model, actor } => ConfigAction::ModelDisable { model, actor },
            ModelCommand::List => ConfigAction::ModelList,
        },
        ConfigCommand::History { limit } => ConfigAction::History { limit },
        ConfigCommand::Project { command } => match command {
            ProjectConfigCommand::Approve { repository, actor } => {
                ConfigAction::ProjectApprove { repository, actor }
            }
            ProjectConfigCommand::Revoke { repository, actor } => {
                ConfigAction::ProjectRevoke { repository, actor }
            }
        },
        ConfigCommand::Show {
            effective,
            repository,
        } => {
            if !effective {
                return Err("config show currently requires --effective".into());
            }
            ConfigAction::ShowEffective { repository }
        }
    };
    execute(action)
}

fn onboard(command: OnboardCommand) -> Result<(), String> {
    match command {
        OnboardCommand::Propose { repository, output } => {
            let proposal = onboarding::propose(&repository)?;
            let encoded = onboarding::encode_proposal(&proposal)?;
            std::fs::write(&output, encoded)
                .map_err(|e| format!("cannot write proposal {}: {e}", output.display()))?;
            println!("proposal={} authority_granted=false", output.display());
        }
        OnboardCommand::Approve {
            proposal,
            answers,
            actor,
            repositories_dir,
        } => {
            let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
            let main = paths.config_dir.join("config.toml");
            let config = Config::load(Some(&main)).map_err(|e| e.to_string())?;
            let directory = repositories_dir.unwrap_or_else(|| {
                let configured = config
                    .repositories_dir
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("repositories"));
                if configured.is_absolute() {
                    configured
                } else {
                    paths.config_dir.join(configured)
                }
            });
            let (hash, encoded) = onboarding::approve(&proposal, &answers, &actor)?;
            std::fs::create_dir_all(&directory)
                .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;
            let attribution = onboarding::encoded_policy_attribution(&encoded)?;
            let repository = &attribution.repository;
            let name = format!("{}.toml", onboarding::sha256(repository.as_bytes()));
            let target = directory.join(name);
            let diff = match std::fs::read_to_string(&target) {
                Ok(current) if current == encoded => "unchanged",
                Ok(current) => {
                    let prior_hash = onboarding::encoded_policy_attribution(&current)
                        .ok()
                        .map(|value| value.content_sha256);
                    if prior_hash.as_deref() == Some(hash.as_str()) {
                        "attribution-changed"
                    } else {
                        "policy-content-changed"
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => "new",
                Err(error) => return Err(format!("cannot inspect {}: {error}", target.display())),
            };
            let pending = target.with_extension("toml.pending");
            std::fs::write(&pending, encoded)
                .map_err(|e| format!("cannot write {}: {e}", pending.display()))?;
            if let Err(error) = onboarding::validate_policy(&pending) {
                let _ = std::fs::remove_file(&pending);
                return Err(error);
            }
            std::fs::rename(&pending, &target)
                .map_err(|e| format!("cannot install {}: {e}", target.display()))?;
            println!(
                "policy={} diff={} actor={} sha256={}",
                target.display(),
                diff,
                actor,
                hash
            );
        }
        OnboardCommand::Validate { policy } => {
            let attribution = onboarding::validate_policy(&policy)?;
            println!(
                "valid actor={} sha256={}",
                attribution.actor, attribution.content_sha256
            );
        }
        OnboardCommand::Fixture { policy } => println!("{}", onboarding::safe_fixture(&policy)?),
    }
    Ok(())
}

fn resume_command(prd: &str, dry_run: bool) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
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
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
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
    // FAM-BUG-016: completion authority is the durable backlog status, not
    // file location — a completed PRD whose document still sits in the active
    // directory (the wave-2 shape) must satisfy dependencies here.
    let mut completed = discovered
        .iter()
        .filter(|entry| entry.location == familiar_ai_core::PrdLocation::Archived)
        .map(|entry| entry.id.to_string())
        .collect::<BTreeSet<_>>();
    completed.extend(
        familiar_ai_storage::OrchestrationRepository::new(db.conn())
            .terminal_prds(&repository.key)
            .map_err(|e| e.to_string())?,
    );
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

fn scope_decisions(
    finding_hash: Option<String>,
    candidate_hash: Option<String>,
    approve: bool,
    reject: bool,
    actor: Option<String>,
    reason: Option<String>,
) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|e| e.to_string())?;
    let mut db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    let repo = familiar_ai_storage::OrchestrationRepository::new(db.conn());
    let pending = repo
        .pending_scope_decisions(&repository.key)
        .map_err(|e| e.to_string())?;
    if finding_hash.is_none() {
        for item in &pending {
            println!(
                "{}",
                serde_json::to_string(item).map_err(|e| e.to_string())?
            );
        }
        if !pending.is_empty() && io::stdin().is_terminal() && io::stderr().is_terminal() {
            eprint!("Decide finding hash (blank to preserve): ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut hash = String::new();
            io::stdin()
                .read_line(&mut hash)
                .map_err(|e| e.to_string())?;
            let hash = hash.trim();
            if hash.is_empty() {
                return Ok(());
            }
            let item = pending
                .iter()
                .find(|p| p.finding_hash == hash)
                .ok_or_else(|| "pending finding hash not found".to_string())?;
            eprint!("Approve or reject [a/r]: ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut choice = String::new();
            io::stdin()
                .read_line(&mut choice)
                .map_err(|e| e.to_string())?;
            eprint!("Actor (human:<identity>): ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut who = String::new();
            io::stdin().read_line(&mut who).map_err(|e| e.to_string())?;
            let checkpoint = repo
                .decide_scope(
                    &repository.key,
                    hash,
                    &item.candidate_hash,
                    choice.trim().eq_ignore_ascii_case("a"),
                    who.trim(),
                    "interactive scope decision",
                )
                .map_err(|e| e.to_string())?;
            continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
        }
        return Ok(());
    }
    if approve == reject {
        return Err("supply exactly one of --approve or --reject".into());
    }
    let actor = actor.ok_or_else(|| "--actor is required for a decision".to_string())?;
    if !actor.starts_with("human:") {
        return Err("--actor must be human:<identity>".into());
    }
    let checkpoint = repo
        .decide_scope(
            &repository.key,
            &finding_hash.unwrap(),
            &candidate_hash.ok_or_else(|| "--candidate-hash is required".to_string())?,
            approve,
            &actor,
            &reason
                .filter(|r| !r.trim().is_empty())
                .ok_or_else(|| "--reason is required".to_string())?,
        )
        .map_err(|e| e.to_string())?;
    continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
    Ok(())
}

fn continue_scope_decision(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    config: &Config,
    checkpoint_id: &str,
) -> Result<(), String> {
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .all(&repository.key)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
        .ok_or_else(|| format!("checkpoint {checkpoint_id} disappeared"))?;
    if checkpoint.phase != "reviewed" {
        return Ok(());
    }
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|error| error.to_string())?;
    let target = FilesystemBacklogDiscovery
        .discover_with_layout(repository, &repository_config.layout())
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|target| target.id.to_string() == checkpoint.prd_id)
        .ok_or_else(|| format!("{} is no longer discoverable", checkpoint.prd_id))?;
    familiar_ai_daemon::drive::continue_scope_approved_candidate(db, repository, &target, config)
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
            let summary = drive_command(Some(max_prds), None, None, None, None, Vec::new())?;
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
    let config = effective_repository_config(&paths, &repository)?;
    let spec =
        familiar_ai_daemon::supervisor::spec(&executable, &repository, &paths, &config.worker)?;
    Ok((spec, repository, paths))
}

fn deliver_command(ownership_record: &std::path::Path, to: Option<&str>) -> Result<(), String> {
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
    let config = effective_repository_config(&paths, &repository.worktree)?;
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
    let policy = repository_config.delivery_policy()?;
    if let Some(role) = to {
        let ownership: familiar_ai_daemon::worktree::WorktreeOwnership = serde_json::from_slice(
            &std::fs::read(ownership_record)
                .map_err(|e| format!("cannot read ownership record: {e}"))?,
        )
        .map_err(|e| format!("invalid ownership record: {e}"))?;
        if ownership.state != "ready_for_delivery" {
            return Err(format!(
                "worktree is not reviewed and ready for delivery (state={})",
                ownership.state
            ));
        }
        let discovered = FilesystemBacklogDiscovery
            .discover_with_layout(&repository, &repository_config.layout())
            .map_err(|e| e.to_string())?;
        let prd = discovered
            .iter()
            .find(|p| p.id.to_string() == ownership.prd_id)
            .ok_or_else(|| {
                format!(
                    "{} is not present in the repository backlog",
                    ownership.prd_id
                )
            })?;
        let revision = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&ownership.worktree)
            .output()
            .map_err(|e| format!("cannot identify delivery revision: {e}"))?;
        if !revision.status.success() {
            return Err("cannot identify delivery revision".into());
        }
        let revision = String::from_utf8_lossy(&revision.stdout).trim().to_owned();
        let db_path = config.database.resolve_path(&paths.data_dir);
        let db = Database::open(&db_path).map_err(|e| e.to_string())?;
        db.run_migrations().map_err(|e| e.to_string())?;
        let result = familiar_ai_daemon::delivery::deliver_to_with(
            &config,
            policy,
            role,
            &repository.key,
            &ownership.session_id,
            &ownership.prd_id,
            &revision,
            &prd.metadata.external_gates,
            db.conn(),
            &ownership.worktree,
            &familiar_ai_daemon::delivery::ProcessRunner::new(policy.command_timeout_ms),
        )?;
        println!(
            "delivery_session={} prd={} role={} target={} revision={} smoke=passed",
            ownership.session_id, ownership.prd_id, result.role, result.target, result.revision
        );
        return Ok(());
    }
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
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
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
            familiar_ai_daemon::preflight::PreflightStatus::EnvironmentDenied => {
                "environment_denied"
            }
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
    let config =
        effective_repository_config(&paths, &repository.worktree).map_err(RunError::Config)?;
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
/// Parse an approved-PRD flag value: `PRD-65`, `prd-065`, `65`, or any of
/// those with a single trailing lowercase suffix letter.
fn parse_prd_flag(value: &str) -> Result<familiar_ai_core::PrdId, String> {
    let trimmed = value.trim();
    let body = trimmed
        .strip_prefix("PRD-")
        .or_else(|| trimmed.strip_prefix("prd-"))
        .unwrap_or(trimmed);
    let (digits, suffix) = match body.strip_suffix(|c: char| c.is_ascii_lowercase()) {
        Some(prefix) => (prefix, body.chars().last()),
        None => (body, None),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "invalid --prd value '{value}': expected PRD-<number> with an optional lowercase suffix"
        ));
    }
    let number: u64 = digits
        .parse()
        .map_err(|_| format!("invalid --prd value '{value}': number out of range"))?;
    Ok(familiar_ai_core::PrdId::with_suffix(number, suffix))
}

fn drive_command(
    max_prds: Option<u64>,
    max_cost_microusd: Option<u64>,
    max_duration_ms: Option<u64>,
    max_parallel_components: Option<usize>,
    worktree_root: Option<PathBuf>,
    prd_flags: Vec<String>,
) -> Result<DriveSummary, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut config = effective_repository_config(&paths, &current)?;
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
    let mut warrant = DriveWarrant::from_config(&config).tightened_by(
        max_prds,
        max_cost_microusd,
        max_duration_ms,
    );
    if !prd_flags.is_empty() {
        let allowlist = prd_flags
            .iter()
            .map(|value| parse_prd_flag(value))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        warrant = warrant.with_prd_allowlist(allowlist)?;
    }
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
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let config = effective_repository_config(&paths, &cwd)?;
    // Resolve Git before opening or migrating storage, preserving the domain's
    // required operation order for invalid working directories.
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
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
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &root)?;
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
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let config = effective_repository_config(&paths, &cwd)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
    let metadata_mode = match &command {
        BacklogCommand::MetadataCheck { advisory: true, .. } => {
            Some(familiar_ai_core::MetadataCheckMode::Advisory)
        }
        BacklogCommand::MetadataCheck { .. } => Some(familiar_ai_core::MetadataCheckMode::Strict),
        _ => None,
    };
    if let Some(mode) = metadata_mode {
        println!("metadata-check mode={}", mode.as_str());
    }
    let mut layout = repository_config.layout();
    if metadata_mode.is_some() {
        // The checker owns strict/advisory exit semantics. Discovery remains
        // fail-closed for every malformed structured-v1 document in both modes.
        layout.metadata_policy = familiar_ai_core::PrdMetadataPolicy::Incremental;
    }
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &layout)
        .map_err(|e| e.to_string())?;
    if discovered.is_empty() {
        return Err("backlog is empty".into());
    }
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    if let Some(mode) = metadata_mode {
        let mut legacy = Vec::new();
        for prd in &discovered {
            if prd.metadata.contract_version == Some(1) {
                println!("{}: structured-v1", prd.path);
            } else {
                println!("{}: legacy: add familiar_ai_prd v1 front matter", prd.path);
                legacy.push(prd.path.to_string());
            }
        }
        println!(
            "metadata-check mode={} structured_v1={} legacy={}",
            mode.as_str(),
            discovered.len() - legacy.len(),
            legacy.len()
        );
        if !legacy.is_empty() && mode.legacy_is_failure() {
            return Err(format!(
                "metadata-check mode={}: {} legacy PRD(s) require migration under policy={}: {}",
                mode.as_str(),
                legacy.len(),
                repository_config.prd_metadata_policy,
                legacy.join(", ")
            ));
        }
        return Ok(());
    }
    let mut db = database()?;
    match command {
        BacklogCommand::MetadataCheck { .. } => {
            unreachable!("handled before storage is opened")
        }
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
        BacklogCommand::ApproveAndComplete {
            prd_path,
            actor,
            reason,
            commit,
        } => approve_and_complete_backlog(
            &mut db,
            &repository,
            &discovered,
            &prd_path,
            &actor,
            &reason,
            commit.as_deref(),
        )?,
    }
    Ok(())
}

/// PRD-065: complete a reviewed retained checkpoint transactionally, binding
/// the approved candidate hash and the resulting commit. Errors print only
/// commands valid for the entry's current persisted state.
fn approve_and_complete_backlog(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    discovered: &[familiar_ai_core::DiscoveredPrd],
    supplied_path: &std::path::Path,
    actor: &str,
    reason: &str,
    commit: Option<&str>,
) -> Result<(), String> {
    validate_recovery_attribution(BacklogRecoveryAction::ApproveAndComplete, actor, reason)
        .map_err(|e| e.to_string())?;
    let target =
        resolve_run_prd(repository, discovered, supplied_path).map_err(|e| e.to_string())?;
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .get(&repository.key, &target.id.to_string())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "no durable checkpoint exists for {}; approve-and-complete acts only on reviewed checkpoints.\nvalid next commands: familiar-ai run {} (fresh attempt), familiar-ai resume {} (inspect partials)",
                target.id,
                target.path,
                target.id
            )
        })?;
    // The checkpoint worktree must still hold the exact approved candidate:
    // that validation is what makes the commit-containment proof below a bind
    // to the APPROVED content rather than to whatever sits on disk (F1).
    let candidate = familiar_ai_daemon::resume::one(db, &repository.key, &target.id.to_string())?;
    if !candidate.valid {
        return Err(format!(
            "checkpoint for {} is not valid ({}); the worktree no longer holds the approved candidate, so no commit can be provably bound to it.\nvalid next commands: familiar-ai resume {} (inspect), familiar-ai run {} (fresh attempt)",
            target.id,
            candidate.reason.as_deref().unwrap_or("invalid_checkpoint"),
            target.id,
            target.path
        ));
    }
    // The default commit is the MAIN worktree's HEAD (where an approved
    // candidate is merged), never the checkpoint worktree's HEAD, which still
    // sits at the base revision.
    let commit = match commit {
        Some(value) => value.to_owned(),
        None => {
            let output = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repository.worktree)
                .output()
                .map_err(|e| format!("cannot resolve HEAD: {e}"))?;
            if !output.status.success() {
                return Err(format!(
                    "cannot resolve HEAD: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
    };
    // Resolve the commit to a real object and prove it contains the candidate
    // byte for byte. A default of HEAD taken before the candidate was merged
    // fails here instead of durably binding completion to the base revision.
    let commit = familiar_ai_daemon::resume::verify_commit_contains_candidate(
        &candidate.worktree,
        &commit,
        &candidate.changed_files,
    )?;
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(repository, discovered)
        .map_err(|e| e.to_string())?;
    let result = SqliteBacklogRepository::new(db.conn_mut())
        .approve_and_complete(
            repository,
            &target,
            actor,
            reason,
            &checkpoint.diff_hash,
            &commit,
        )
        .map_err(|error| {
            format!(
                "{error}\n{}",
                backlog_next_commands(db, repository, &target)
            )
        })?;
    println!(
        "backlog approve-and-complete: {} {} -> {} approved_hash={} commit={} actor={} reason={}",
        result.prd.id,
        result.prd.path,
        result.status.as_str(),
        checkpoint.diff_hash,
        commit,
        escape_output(actor.trim()),
        escape_output(reason.trim())
    );
    Ok(())
}

/// The commands valid for one backlog entry's current persisted state.
fn backlog_next_commands(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    target: &familiar_ai_core::DiscoveredPrd,
) -> String {
    let status: Option<String> = familiar_ai_storage::list_backlog_entries(
        db.conn(),
        &repository.key,
        None,
        None,
        usize::MAX,
    )
    .ok()
    .and_then(|rows| {
        rows.into_iter()
            .find(|row| row.prd_path == target.path.as_str())
            .map(|row| row.status)
    });
    let id = &target.id;
    let path = &target.path;
    match status.as_deref() {
        Some("completed") => format!("valid next commands: none — {id} is already completed"),
        Some("blocked") => format!(
            "valid next commands: familiar-ai backlog release {path} --actor ... --reason ..."
        ),
        Some("pending") | Some("in_progress") => format!(
            "valid next commands: familiar-ai backlog approve-and-complete {path} --actor human:<identity> --reason ... (reviewed checkpoint), familiar-ai resume {id} (continue the partial), familiar-ai run {path} (fresh attempt)"
        ),
        _ => format!("valid next commands: familiar-ai run {path} (entry is not tracked yet)"),
    }
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

fn effective_repository_config(
    paths: &AppPaths,
    repository: &std::path::Path,
) -> Result<Config, String> {
    familiar_ai_daemon::config_cli::effective_config_for_repository(
        &familiar_ai_daemon::config_cli::ConfigContext {
            config_path: paths.config_dir.join("config.toml"),
            data_dir: paths.data_dir.clone(),
        },
        repository,
    )
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
    let ledger = familiar_ai_storage::AccountingRepository::new(db.conn())
        .usage()
        .map_err(|e| e.to_string())?;
    println!("Ledger observations: {}", ledger.observations);
    println!(
        "Ledger observations with unknown usage: {}",
        ledger.unknown_observations
    );
    println!(
        "Ledger uncached input tokens: {}",
        ledger.uncached_input_tokens
    );
    println!("Ledger cache-read tokens: {}", ledger.cache_read_tokens);
    println!("Ledger cache-write tokens: {}", ledger.cache_write_tokens);
    println!("Ledger output tokens: {}", ledger.output_tokens);
    println!(
        "Ledger reasoning-output tokens: {}",
        ledger.reasoning_output_tokens
    );
    println!("Ledger local-estimate nanoUSD: {}", ledger.known_nanousd);
    println!(
        "Ledger provenance vendor-reported={} configured-rate={} known-zero={}",
        ledger.vendor_reported_estimates,
        ledger.configured_rate_estimates,
        ledger.known_zero_estimates
    );
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
    fn prd_flags_parse_canonically_and_reject_garbage() {
        use familiar_ai_core::PrdId;
        for value in ["PRD-65", "prd-65", "65", "PRD-065"] {
            assert_eq!(parse_prd_flag(value).unwrap(), PrdId::new(65), "{value}");
        }
        assert_eq!(
            parse_prd_flag("PRD-65a").unwrap(),
            PrdId::with_suffix(65, Some('a'))
        );
        for value in ["", "PRD-", "sixty-five", "PRD-65A", "65.1", "PRD-65 66"] {
            assert!(parse_prd_flag(value).is_err(), "{value}");
        }
    }

    #[test]
    fn drive_accepts_repeatable_prd_allowlist_flags() {
        let Command::Drive { prd, .. } = Cli::try_parse_from([
            "familiar-ai",
            "drive",
            "--max-prds",
            "1",
            "--prd",
            "PRD-065",
            "--prd",
            "PRD-041",
        ])
        .unwrap()
        .command
        else {
            panic!("expected drive command");
        };
        assert_eq!(prd, vec!["PRD-065", "PRD-041"]);
    }

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
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "onboard", "propose"])
                .unwrap()
                .command,
            Command::Onboard {
                command: OnboardCommand::Propose { .. }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "familiar-ai",
                "onboard",
                "approve",
                "proposal.toml",
                "--answers",
                "answers.toml",
                "--actor",
                "human:alice"
            ])
            .unwrap()
            .command,
            Command::Onboard {
                command: OnboardCommand::Approve { .. }
            }
        ));
        assert!(Cli::try_parse_from([
            "familiar-ai",
            "onboard",
            "approve",
            "proposal.toml",
            "--actor",
            "human:alice"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["familiar-ai", "next", "PRD-1.md"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "backlog", "metadata-check"])
                .unwrap()
                .command,
            Command::Backlog {
                command: BacklogCommand::MetadataCheck { .. }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "backlog", "metadata-check", "--advisory"])
                .unwrap()
                .command,
            Command::Backlog {
                command: BacklogCommand::MetadataCheck { advisory: true, .. }
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
