use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use familiar_ai_core::{
    validate_graph, AppPaths, BacklogDiscovery, BacklogManager, BacklogStatusStore,
    FilesystemBacklogDiscovery, ProfiledFilesystemBacklogDiscovery,
};
use familiar_ai_storage::{DriverRepository, OrchestrationRepository, SqliteBacklogRepository};

use familiar_ai_daemon::cli::accounting::AccountingCommand;
use familiar_ai_daemon::cli::backlog::BacklogCommand;
use familiar_ai_daemon::cli::batch_review::BatchReviewCommand;
use familiar_ai_daemon::cli::billing::BillingCommand;
use familiar_ai_daemon::cli::control::ControlCommand;
use familiar_ai_daemon::cli::desktop::DesktopCommand;
use familiar_ai_daemon::cli::gate::GateCommand;
use familiar_ai_daemon::cli::onboard::OnboardCommand;
use familiar_ai_daemon::cli::operator::OperatorCommand;
use familiar_ai_daemon::cli::plan::PlanCommand;
use familiar_ai_daemon::cli::shared::{database, effective_repository_config};
use familiar_ai_daemon::cli::stewardship::StewardshipCommand;
use familiar_ai_daemon::cli::worker::WorkerCommand;

/// PRD-090: a small set of daily verbs at the top, everything else reachable
/// under a declared administrative namespace. See
/// [`familiar_ai_daemon::cli::shared::TOP_LEVEL_COMMANDS`] for the exact
/// declared set and its ceiling, and
/// [`familiar_ai_daemon::cli::shared::RELOCATED_COMMAND_ALIASES`] for every
/// previous top-level name this reorganisation kept working.
#[derive(Debug, Parser)]
#[command(name = "familiar-ai", about = "Familiar command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    // -- Daily verbs -----------------------------------------------------
    /// Select the next eligible repository PRD without executing it; run it
    /// with `familiar-ai run <path>`.
    Next,
    /// Execute a repository PRD with the configured coding agent. A scope
    /// pause is decided with `familiar-ai approve`.
    Run { prd_path: PathBuf },
    /// Execute eligible backlog PRDs unattended until the backlog is empty,
    /// nothing is eligible, or the budget warrant is exhausted. Flags may
    /// only tighten the configured warrant, never loosen it. See what
    /// happened with `familiar-ai report`.
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
    /// Continue one durable partial, or inspect/schedule all durable
    /// partials. Any pending scope finding needs `familiar-ai approve`
    /// first; a reviewed partial ships with `familiar-ai deliver`.
    Resume {
        /// PRD identifier (for example PRD-123), or `all`.
        prd: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Render one unattended driver session: what got built, what stopped
    /// and why, what it cost, and what needs `familiar-ai approve`.
    /// Defaults to the most recent session.
    Report { session_id: Option<String> },
    /// Decide one hash-bound pending scope finding, by ordinal or PRD id
    /// when run with no arguments on a terminal, or by the flags below.
    /// Continue the paused work afterward with `familiar-ai resume <prd>`.
    Approve {
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
    /// Publish, check, merge, deploy to staging, and smoke-test one reviewed
    /// worktree under the configured finite delivery policy -- completes the
    /// workflow.
    Deliver {
        ownership_record: PathBuf,
        /// Resolve and execute the repository-bound environment role.
        #[arg(long)]
        to: Option<String>,
    },

    // -- Administrative namespaces ----------------------------------------
    /// Providers, models, artifacts, compression, and local model residency.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Reconciliation-aware cost reporting, cached billing, and usage.
    Accounting {
        #[command(subcommand)]
        command: AccountingNamespaceCommand,
    },
    /// Read-only queries over durable execution-era state: backlog,
    /// sessions, attempts, checkpoints, recovery, delivery, budgets, review,
    /// gates, reconciliation, workers, repository status, preflight, and
    /// standalone-execution history.
    Stewardship {
        #[command(subcommand)]
        command: StewardshipNamespaceCommand,
    },
    /// Draft or decide a human-reviewed PRD proposal batch, and the backlog
    /// and policy administration around it: onboarding, backlog bootstrap
    /// and recovery, and batch-tier review configuration.
    Plan {
        #[command(subcommand)]
        command: Option<PlanNamespaceCommand>,
        /// Design documents supplied to the configured planner agent.
        design_docs: Vec<PathBuf>,
    },
    /// Operational control and repair: detached executions, the native
    /// worker daemon, operator checkpoint repairs, the merge gate, and
    /// review waivers.
    Ops {
        #[command(subcommand)]
        command: OpsCommand,
    },

    // -- Hidden legacy aliases ---------------------------------------------
    // Every command below moved under an administrative namespace above.
    // Each keeps its previous top-level invocation working unchanged; the
    // notice naming its new form is printed in `main` before parsing, from
    // `familiar_ai_daemon::cli::shared::RELOCATED_COMMAND_ALIASES`, so it
    // fires even for a bare `--help`.
    #[command(hide = true)]
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
    #[command(hide = true)]
    Billing {
        #[command(subcommand)]
        command: BillingCommand,
    },
    #[command(hide = true)]
    Compress {
        #[command(subcommand)]
        command: CompressCommand,
    },
    #[command(hide = true)]
    ModelResidency {
        #[command(subcommand)]
        command: ModelResidencyCommand,
    },
    #[command(hide = true)]
    Status {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
    #[command(hide = true)]
    Preflight,
    #[command(hide = true)]
    History {
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u8).range(1..=100))]
        limit: u8,
        #[arg(long)]
        verbose: bool,
    },
    #[command(hide = true)]
    Usage {
        #[arg(long, requires = "end")]
        start: Option<String>,
        #[arg(long, requires = "start")]
        end: Option<String>,
        #[arg(long, default_value = "day")]
        bucket: String,
        #[arg(long, value_delimiter = ',')]
        group_by: Vec<String>,
        #[arg(long = "filter")]
        filters: Vec<String>,
        #[arg(long)]
        dense: bool,
    },
    #[command(hide = true)]
    Onboard {
        #[command(subcommand)]
        command: OnboardCommand,
    },
    #[command(hide = true)]
    Backlog {
        #[command(subcommand)]
        command: BacklogCommand,
    },
    #[command(hide = true)]
    BatchReview {
        #[command(subcommand)]
        command: BatchReviewCommand,
    },
    #[command(hide = true)]
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    #[command(hide = true)]
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
    #[command(hide = true)]
    Operator {
        #[command(subcommand)]
        command: OperatorCommand,
    },
    #[command(hide = true)]
    Gate {
        #[command(subcommand)]
        command: GateCommand,
    },
    #[command(hide = true)]
    Waive {
        #[arg(long)]
        cycle_id: String,
        #[arg(long)]
        finding_id: String,
        /// Mandatory explicit human authority, e.g. human:trollboy.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason.
        #[arg(long)]
        reason: String,
    },
}

/// The word after `familiar-ai invoked` finds its top-level name here first;
/// see the individual leaf commands' own doc comments (`billing status`,
/// `usage`, ...) for what moved where.
#[derive(Debug, Subcommand)]
enum AccountingNamespaceCommand {
    #[command(flatten)]
    Report(AccountingCommand),
    /// Inspect cached authoritative billing, explicitly collect it, or
    /// reconcile it against local estimates.
    Billing {
        #[command(subcommand)]
        command: BillingCommand,
    },
    /// Query cached local accounting. With no range, preserves the legacy
    /// summary.
    Usage {
        #[arg(long, requires = "end")]
        start: Option<String>,
        #[arg(long, requires = "start")]
        end: Option<String>,
        #[arg(long, default_value = "day")]
        bucket: String,
        #[arg(long, value_delimiter = ',')]
        group_by: Vec<String>,
        #[arg(long = "filter")]
        filters: Vec<String>,
        #[arg(long)]
        dense: bool,
    },
}

#[derive(Debug, Subcommand)]
enum StewardshipNamespaceCommand {
    #[command(flatten)]
    Query(StewardshipCommand),
    /// Show repository project-configuration approval and binding state.
    Status {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
    /// Validate prerequisites without claiming a PRD or invoking a model.
    Preflight,
    /// List recent standalone executions.
    History {
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u8).range(1..=100))]
        limit: u8,
        #[arg(long)]
        verbose: bool,
    },
    /// Compute repository- and host-scoped delivery north-star metrics.
    Metrics {
        #[arg(long)]
        host: String,
        #[arg(long)]
        repository_key: String,
        #[arg(long)]
        current_start: String,
        #[arg(long)]
        current_end: String,
        #[arg(long)]
        previous_start: String,
        #[arg(long)]
        previous_end: String,
        #[arg(long = "required-host")]
        required_hosts: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
enum PlanNamespaceCommand {
    #[command(flatten)]
    Batch(PlanCommand),
    /// Discover and approve repository-owned policy without claiming work.
    Onboard {
        #[command(subcommand)]
        command: OnboardCommand,
    },
    /// Inspect or roll back the historical backlog bootstrap, and record
    /// human-attributed recovery decisions.
    Backlog {
        #[command(subcommand)]
        command: BacklogCommand,
    },
    /// Configure and observe PRD-071 batch-tier independent review.
    BatchReview {
        #[command(subcommand)]
        command: BatchReviewCommand,
    },
}

#[derive(Debug, Subcommand)]
enum OpsCommand {
    /// Submit and observe daemon-owned detached executions.
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Install and operate a bounded native-supervised worker.
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
    /// Install and operate the cross-platform Familiar desktop and daemon.
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
    /// Operator repairs: rebind a checkpoint, transition its phase, or ask
    /// the scheduler its achievable width. Each requires an explicit human
    /// actor and reason, and each refuses while a driver holds the claim.
    Operator {
        #[command(subcommand)]
        command: OperatorCommand,
    },
    /// Ask whether a commit was actually verified, refuse a merge that was
    /// not, or record an override. Only green is a pass (PRD-099).
    Gate {
        #[command(subcommand)]
        command: GateCommand,
    },
    /// Attach a durable human waiver to one open blocking review finding
    /// (required by completion-evidence for terminal reviews). Waivers are
    /// substance-keyed and survive reviewer finding-id rotation.
    Waive {
        #[arg(long)]
        cycle_id: String,
        #[arg(long)]
        finding_id: String,
        /// Mandatory explicit human authority, e.g. human:trollboy.
        #[arg(long)]
        actor: String,
        /// Mandatory non-empty audit reason.
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
enum CompressCommand {
    OutputEnable {
        stage: String,
        #[arg(default_value = "compact")]
        register: String,
        #[arg(long)]
        actor: String,
    },
    InputEnable {
        provider: String,
        #[arg(default_value = "native-rle")]
        transform: String,
        #[arg(long)]
        actor: String,
    },
    /// With --lane, auditably label subsequent observations; without it,
    /// report only measured paired ledger values. Default-on promotion
    /// requires a recorded experiment result.
    Experiment {
        label: String,
        #[arg(long)]
        lane: Option<String>,
        #[arg(long, requires = "lane")]
        actor: Option<String>,
    },
}

/// PRD-073 warm local model residency. Residency is off until an operator
/// enables it here, and every mutation is an audited configuration decision.
#[derive(Debug, Subcommand)]
enum ModelResidencyCommand {
    /// Hold one local worker's model artifact resident between executions.
    Enable {
        /// Resident key: how this resident is named in configuration and in
        /// every durable residency record.
        key: String,
        /// The `worker_registry.workers` entry to serve. It must declare a
        /// PRD-063 local profile and a PRD-062 model artifact.
        #[arg(long)]
        worker: String,
        /// The serving command, one `--launch` per argv element. Required:
        /// Familiar never guesses how a local runtime is started, and the
        /// argv is executed directly rather than through a shell.
        #[arg(long = "launch", required = true)]
        launch: Vec<String>,
        /// This resident's declared memory footprint. Required whenever a
        /// memory ceiling is configured.
        #[arg(long)]
        memory_mb: Option<u64>,
        /// How long a freshly launched server may take to answer.
        #[arg(long, default_value_t = 60)]
        ready_timeout_secs: u64,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Stop holding a resident. The daemon stops the server and records it.
    Disable {
        key: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Show residency configuration and recorded lifecycle events.
    Status {
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Migrate legacy configuration sections to supported replacements.
    Migrate {
        #[command(subcommand)]
        command: ConfigMigrateCommand,
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
    /// Configure native compression or report a measured paired experiment.
    Compress {
        #[command(subcommand)]
        command: CompressCommand,
    },
    /// Hold configured local model artifacts loaded between executions.
    ModelResidency {
        #[command(subcommand)]
        command: ModelResidencyCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ArtifactCommand {
    /// Probe and register externally prepared identity-bearing files.
    Register {
        alias: String,
        root: PathBuf,
        #[arg(long = "file", required = true)]
        files: Vec<PathBuf>,
        /// JSON object of identity-bearing configuration.
        #[arg(long, default_value = "{}")]
        identity: String,
        /// JSON provenance record; omitted fields remain explicitly unknown.
        #[arg(long, default_value = "{}")]
        provenance: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long = "adapter")]
        adapters: Vec<String>,
        #[arg(long)]
        merged: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Record a legacy/runtime-only alias as degraded and unverified.
    RegisterAlias {
        alias: String,
        runtime_alias: String,
        #[arg(long)]
        actor: Option<String>,
    },
    List,
    Show {
        alias: String,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigMigrateCommand {
    /// Losslessly migrate [agents] to the worker registry.
    Agents {
        #[arg(long)]
        actor: Option<String>,
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
        mode: Option<String>,
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
    /// Attach an auditable operator-declared cost estimate to an enabled worker.
    CostBasis {
        worker: String,
        #[arg(long)]
        estimate_microusd: u64,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
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

fn main() -> ExitCode {
    // PRD-090: printed from raw argv, before clap parses anything, so the
    // notice fires even for `familiar-ai <old-name> --help` -- an alias must
    // be provably reachable, and `--help` is the safest possible probe of
    // that.
    if let Some(invoked) = std::env::args().nth(1) {
        if let Some(notice) = familiar_ai_daemon::cli::shared::relocation_notice(&invoked) {
            eprintln!("{notice}");
        }
    }
    let cli = Cli::parse();
    match cli.command {
        None => front_door(),
        Some(command) => dispatch(command),
    }
}

/// Bare `familiar-ai`: the repository's current state and the single next
/// runnable command, rather than a help dump.
fn front_door() -> ExitCode {
    match front_door_report() {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => fail(error),
    }
}

fn front_door_report() -> Result<String, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let config = effective_repository_config(&paths, &cwd)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let mut db = database()?;

    let pending_decision_command = OrchestrationRepository::new(db.conn())
        .pending_scope_decisions(&repository.key)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .map(|decision| {
            format!(
                "familiar-ai approve --finding-hash {} --candidate-hash {} --approve --actor human:<identity> --reason \"<why>\"",
                decision.finding_hash, decision.candidate_hash
            )
        });

    let stopped_session_command = DriverRepository::new(db.conn())
        .latest_session()
        .map_err(|e| e.to_string())?
        .and_then(|session| {
            let reason = session.termination_reason.as_deref()?;
            familiar_ai_daemon::cli::shared::termination_needs_attention(reason)
                .then(|| format!("familiar-ai report {}", session.session_id))
        });

    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
    let layout = repository_config.layout();
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &layout)
        .map_err(|e| e.to_string())?;
    // PRD-103 (f1-front-door-misreports-broken-backlog): a backlog whose
    // graph does not validate, or whose selection fails for a reason other
    // than "nothing is eligible", is not the same state as an empty or
    // fully-scheduled backlog. Collapsing both into `None` told an operator
    // "nothing to do" for a repository that in fact has PRDs it cannot
    // schedule -- exactly the case where they most need to be told
    // something is wrong, not reassured. `familiar_ai_core::BacklogError::
    // NoEligiblePrd` is the one selection error that IS a legitimate "no
    // work right now" outcome (every PRD is completed, blocked, or waiting
    // on a dependency), so it alone still folds to `None`.
    let eligible_work: Result<Option<String>, String> = if discovered.is_empty() {
        Ok(None)
    } else if let Err(error) = validate_graph(&discovered) {
        Err(error.to_string())
    } else {
        SqliteBacklogRepository::new(db.conn_mut())
            .reconcile_and_snapshot(&repository, &discovered)
            .map_err(|e| e.to_string())?;
        let store = SqliteBacklogRepository::new(db.conn_mut());
        let mut manager = BacklogManager::new(ProfiledFilesystemBacklogDiscovery { layout }, store);
        match manager.next(&cwd) {
            Ok(selected) => Ok(Some(format!("familiar-ai run {}", selected.path))),
            Err(familiar_ai_core::BacklogError::NoEligiblePrd(_)) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    };

    // A pending decision or a stopped session is still the right thing to
    // surface even when the backlog is also broken -- neither depends on
    // the backlog validating. Only when neither is present does a broken
    // backlog need to be reported in place of "nothing to do".
    if pending_decision_command.is_none() && stopped_session_command.is_none() {
        if let Err(error) = &eligible_work {
            return Ok(format!(
                "repository: {}\nstate: backlog unschedulable\nerror: {error}\nnext: familiar-ai next\n",
                repository.key
            ));
        }
    }
    let eligible_work_command = eligible_work.unwrap_or(None);

    let state = familiar_ai_daemon::cli::shared::resolve_front_door_state(
        pending_decision_command,
        stopped_session_command,
        eligible_work_command,
    );
    Ok(format!(
        "repository: {}\nstate: {}\nnext: {}\n",
        repository.key,
        state.label(),
        state.next_command()
    ))
}

fn dispatch(command: Command) -> ExitCode {
    match command {
        Command::Next => match familiar_ai_daemon::cli::next::next() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Run { prd_path } => match familiar_ai_daemon::cli::run::run(&prd_path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let code = error.exit_code();
                eprintln!("error: {error}");
                code.and_then(|value| u8::try_from(value).ok())
                    .map_or(ExitCode::FAILURE, ExitCode::from)
            }
        },
        Command::Drive {
            max_prds,
            max_cost_microusd,
            max_duration_ms,
            max_parallel_components,
            worktree_root,
            prd,
        } => match familiar_ai_daemon::cli::drive::drive_command(
            max_prds,
            max_cost_microusd,
            max_duration_ms,
            max_parallel_components,
            worktree_root,
            prd,
        ) {
            // A crash-like zero-work stop (preflight failure, lost worker,
            // storage failure) must be visible to wrapping scripts; only
            // deliberate policy/budget stops exit 0.
            Ok(summary) if summary.termination.worker_should_restart() => fail(format!(
                "session {} terminated abnormally: {}",
                summary.session_id,
                summary.termination.as_str()
            )),
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Resume { prd, dry_run } => {
            match familiar_ai_daemon::cli::resume::resume_command(&prd, dry_run) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Report { session_id } => {
            match familiar_ai_daemon::cli::report::report_command(session_id.as_deref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Approve {
            finding_hash,
            candidate_hash,
            approve,
            reject,
            actor,
            reason,
        }
        | Command::ScopeDecisions {
            finding_hash,
            candidate_hash,
            approve,
            reject,
            actor,
            reason,
        } => match familiar_ai_daemon::cli::scope_decisions::scope_decisions(
            finding_hash,
            candidate_hash,
            approve,
            reject,
            actor,
            reason,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Deliver {
            ownership_record,
            to,
        } => match familiar_ai_daemon::cli::deliver::deliver_command(
            &ownership_record,
            to.as_deref(),
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Config { command } => match config_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Accounting { command } => {
            let result = match command {
                AccountingNamespaceCommand::Report(command) => {
                    familiar_ai_daemon::cli::accounting::accounting_command(command)
                }
                AccountingNamespaceCommand::Billing { command } => {
                    familiar_ai_daemon::cli::billing::billing_command(command)
                }
                AccountingNamespaceCommand::Usage {
                    start,
                    end,
                    bucket,
                    group_by,
                    filters,
                    dense,
                } => familiar_ai_daemon::cli::usage::usage(
                    start.as_deref(),
                    end.as_deref(),
                    &bucket,
                    group_by,
                    filters,
                    dense,
                ),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Stewardship { command } => {
            let result = match command {
                StewardshipNamespaceCommand::Query(command) => {
                    familiar_ai_daemon::cli::stewardship::stewardship_command(command)
                }
                StewardshipNamespaceCommand::Status { repository } => {
                    familiar_ai_daemon::config_cli::execute(
                        familiar_ai_daemon::config_cli::ConfigAction::Status { repository },
                    )
                }
                StewardshipNamespaceCommand::Preflight => {
                    familiar_ai_daemon::cli::preflight::preflight_command()
                }
                StewardshipNamespaceCommand::History { limit, verbose } => {
                    familiar_ai_daemon::cli::history::history(limit, verbose)
                }
                StewardshipNamespaceCommand::Metrics {
                    host,
                    repository_key,
                    current_start,
                    current_end,
                    previous_start,
                    previous_end,
                    required_hosts,
                } => familiar_ai_daemon::cli::metrics::command(
                    familiar_ai_daemon::cli::metrics::MetricsRequest {
                        host,
                        repository_key,
                        current_start,
                        current_end,
                        previous_start,
                        previous_end,
                        required_hosts,
                    },
                ),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Plan {
            command,
            design_docs,
        } => {
            let result = match command {
                None => familiar_ai_daemon::cli::plan::plan(None, &design_docs),
                Some(PlanNamespaceCommand::Batch(command)) => {
                    familiar_ai_daemon::cli::plan::plan(Some(command), &design_docs)
                }
                Some(PlanNamespaceCommand::Onboard { command }) => {
                    familiar_ai_daemon::cli::onboard::onboard(command)
                }
                Some(PlanNamespaceCommand::Backlog { command }) => {
                    familiar_ai_daemon::cli::backlog::backlog(command)
                }
                Some(PlanNamespaceCommand::BatchReview { command }) => {
                    familiar_ai_daemon::cli::batch_review::batch_review_command(command)
                }
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Ops { command } => {
            let result = match command {
                OpsCommand::Control { command } => {
                    familiar_ai_daemon::cli::control::control_command(command)
                }
                OpsCommand::Worker { command } => {
                    familiar_ai_daemon::cli::worker::worker_command(command)
                }
                OpsCommand::Desktop { command } => {
                    familiar_ai_daemon::cli::desktop::desktop(command)
                }
                OpsCommand::Operator { command } => {
                    familiar_ai_daemon::cli::operator::operator(command)
                }
                OpsCommand::Gate { command } => familiar_ai_daemon::cli::gate::gate(command),
                OpsCommand::Waive {
                    cycle_id,
                    finding_id,
                    actor,
                    reason,
                } => familiar_ai_daemon::cli::waive::waive(cycle_id, finding_id, actor, reason),
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }

        // -- Hidden legacy aliases: identical behaviour, unchanged fields --
        Command::Billing { command } => {
            match familiar_ai_daemon::cli::billing::billing_command(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Compress { command } => {
            let result = match command {
                CompressCommand::OutputEnable {
                    stage,
                    register,
                    actor,
                } => familiar_ai_daemon::compress_cli::configure_output(&stage, &register, &actor),
                CompressCommand::InputEnable {
                    provider,
                    transform,
                    actor,
                } => {
                    familiar_ai_daemon::compress_cli::configure_input(&provider, &transform, &actor)
                }
                CompressCommand::Experiment { label, lane, actor } => match lane {
                    Some(lane) => familiar_ai_daemon::compress_cli::configure_experiment(
                        &label,
                        &lane,
                        actor.as_deref().expect("clap requires actor with lane"),
                    ),
                    None => familiar_ai_daemon::compress_cli::experiment(&label),
                },
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::ModelResidency { command } => {
            match familiar_ai_daemon::cli::model_residency::execute(model_residency_action(command))
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Status { repository } => match familiar_ai_daemon::config_cli::execute(
            familiar_ai_daemon::config_cli::ConfigAction::Status { repository },
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Preflight => match familiar_ai_daemon::cli::preflight::preflight_command() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::History { limit, verbose } => {
            match familiar_ai_daemon::cli::history::history(limit, verbose) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Usage {
            start,
            end,
            bucket,
            group_by,
            filters,
            dense,
        } => match familiar_ai_daemon::cli::usage::usage(
            start.as_deref(),
            end.as_deref(),
            &bucket,
            group_by,
            filters,
            dense,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Onboard { command } => match familiar_ai_daemon::cli::onboard::onboard(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Backlog { command } => match familiar_ai_daemon::cli::backlog::backlog(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::BatchReview { command } => {
            match familiar_ai_daemon::cli::batch_review::batch_review_command(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Control { command } => {
            match familiar_ai_daemon::cli::control::control_command(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Worker { command } => {
            match familiar_ai_daemon::cli::worker::worker_command(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Operator { command } => {
            match familiar_ai_daemon::cli::operator::operator(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Gate { command } => match familiar_ai_daemon::cli::gate::gate(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Waive {
            cycle_id,
            finding_id,
            actor,
            reason,
        } => match familiar_ai_daemon::cli::waive::waive(cycle_id, finding_id, actor, reason) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
    }
}

fn model_residency_action(
    command: ModelResidencyCommand,
) -> familiar_ai_daemon::cli::model_residency::ResidencyAction {
    match command {
        ModelResidencyCommand::Enable {
            key,
            worker,
            launch,
            memory_mb,
            ready_timeout_secs,
            actor,
        } => familiar_ai_daemon::cli::model_residency::ResidencyAction::Enable {
            key,
            worker,
            launch,
            memory_mb,
            ready_timeout_secs,
            actor,
        },
        ModelResidencyCommand::Disable { key, actor } => {
            familiar_ai_daemon::cli::model_residency::ResidencyAction::Disable { key, actor }
        }
        ModelResidencyCommand::Status { limit } => {
            familiar_ai_daemon::cli::model_residency::ResidencyAction::Status { limit }
        }
    }
}

fn config_command(command: ConfigCommand) -> Result<(), String> {
    use familiar_ai_daemon::config_cli::{execute, ConfigAction};
    if let ConfigCommand::Compress { command } = command {
        return match command {
            CompressCommand::OutputEnable {
                stage,
                register,
                actor,
            } => familiar_ai_daemon::compress_cli::configure_output(&stage, &register, &actor),
            CompressCommand::InputEnable {
                provider,
                transform,
                actor,
            } => familiar_ai_daemon::compress_cli::configure_input(&provider, &transform, &actor),
            CompressCommand::Experiment { label, lane, actor } => match lane {
                Some(lane) => familiar_ai_daemon::compress_cli::configure_experiment(
                    &label,
                    &lane,
                    actor.as_deref().expect("clap requires actor with lane"),
                ),
                None => familiar_ai_daemon::compress_cli::experiment(&label),
            },
        };
    }
    if let ConfigCommand::ModelResidency { command } = command {
        return familiar_ai_daemon::cli::model_residency::execute(model_residency_action(command));
    }
    let action = match command {
        ConfigCommand::Provider { command } => match command {
            ProviderCommand::Add {
                name,
                kind,
                mode,
                host,
                auth,
                via,
                recipe,
                actor,
            } => ConfigAction::ProviderAdd {
                name,
                kind,
                mode,
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
            ModelCommand::CostBasis {
                worker,
                estimate_microusd,
                actor,
                reason,
            } => ConfigAction::ModelCostBasis {
                worker,
                estimate_microusd,
                actor,
                reason,
            },
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
        ConfigCommand::Artifact { command } => match command {
            ArtifactCommand::Register {
                alias,
                root,
                files,
                identity,
                provenance,
                base,
                adapters,
                merged,
                actor,
            } => ConfigAction::ArtifactRegister {
                alias,
                root,
                files,
                identity,
                provenance,
                base,
                adapters,
                merged,
                actor,
            },
            ArtifactCommand::RegisterAlias {
                alias,
                runtime_alias,
                actor,
            } => ConfigAction::ArtifactRegisterAlias {
                alias,
                runtime_alias,
                actor,
            },
            ArtifactCommand::List => ConfigAction::ArtifactList,
            ArtifactCommand::Show { alias } => ConfigAction::ArtifactShow { alias },
        },
        ConfigCommand::Migrate { command } => match command {
            ConfigMigrateCommand::Agents { actor } => ConfigAction::MigrateAgents { actor },
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
        ConfigCommand::Compress { .. } | ConfigCommand::ModelResidency { .. } => {
            unreachable!("handled above")
        }
    };
    execute(action)
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
            assert_eq!(
                familiar_ai_daemon::drive::parse_prd_flag(value).unwrap(),
                PrdId::new(65),
                "{value}"
            );
        }
        assert_eq!(
            familiar_ai_daemon::drive::parse_prd_flag("PRD-65a").unwrap(),
            PrdId::with_suffix(65, Some('a'))
        );
        for value in ["", "PRD-", "sixty-five", "PRD-65A", "65.1", "PRD-65 66"] {
            assert!(
                familiar_ai_daemon::drive::parse_prd_flag(value).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn drive_accepts_repeatable_prd_allowlist_flags() {
        let Some(Command::Drive { prd, .. }) = Cli::try_parse_from([
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
            Some(Command::History { limit: 100, .. })
        ));
        assert!(Cli::try_parse_from(["familiar-ai", "history", "--limit", "0"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "usage"])
                .unwrap()
                .command,
            Some(Command::Usage { .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "next"])
                .unwrap()
                .command,
            Some(Command::Next)
        ));
        assert!(Cli::try_parse_from(["familiar-ai"])
            .unwrap()
            .command
            .is_none());
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "onboard", "propose"])
                .unwrap()
                .command,
            Some(Command::Onboard {
                command: OnboardCommand::Propose { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "plan", "onboard", "propose"])
                .unwrap()
                .command,
            Some(Command::Plan {
                command: Some(PlanNamespaceCommand::Onboard {
                    command: OnboardCommand::Propose { .. }
                }),
                ..
            })
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
            Some(Command::Onboard {
                command: OnboardCommand::Approve { .. }
            })
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
            Some(Command::Backlog {
                command: BacklogCommand::MetadataCheck { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "backlog", "metadata-check", "--advisory"])
                .unwrap()
                .command,
            Some(Command::Backlog {
                command: BacklogCommand::MetadataCheck { advisory: true, .. }
            })
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
            Some(Command::Backlog {
                command: BacklogCommand::Complete { .. }
            })
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
            Some(Command::Backlog {
                command: BacklogCommand::RecordComplete { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "familiar-ai",
                "approve",
                "--finding-hash",
                "h",
                "--candidate-hash",
                "c",
                "--approve",
                "--actor",
                "human:alice",
                "--reason",
                "looks fine"
            ])
            .unwrap()
            .command,
            Some(Command::Approve { approve: true, .. })
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "accounting", "billing", "status"])
                .unwrap()
                .command,
            Some(Command::Accounting {
                command: AccountingNamespaceCommand::Billing {
                    command: BillingCommand::Status
                }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "accounting", "month-to-date"])
                .unwrap()
                .command,
            Some(Command::Accounting {
                command: AccountingNamespaceCommand::Report(AccountingCommand::MonthToDate)
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "stewardship", "status"])
                .unwrap()
                .command,
            Some(Command::Stewardship {
                command: StewardshipNamespaceCommand::Status { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "familiar-ai",
                "ops",
                "waive",
                "--cycle-id",
                "c",
                "--finding-id",
                "f",
                "--actor",
                "human:a",
                "--reason",
                "r"
            ])
            .unwrap()
            .command,
            Some(Command::Ops {
                command: OpsCommand::Waive { .. }
            })
        ));
        // Every relocated top-level name still parses on its own, unchanged.
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "billing", "status"])
                .unwrap()
                .command,
            Some(Command::Billing {
                command: BillingCommand::Status
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar-ai", "status"])
                .unwrap()
                .command,
            Some(Command::Status { .. })
        ));
    }
}
