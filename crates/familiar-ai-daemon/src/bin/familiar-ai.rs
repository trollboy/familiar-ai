use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use familiar_ai_daemon::cli::backlog::BacklogCommand;
use familiar_ai_daemon::cli::billing::BillingCommand;
use familiar_ai_daemon::cli::control::ControlCommand;
use familiar_ai_daemon::cli::onboard::OnboardCommand;
use familiar_ai_daemon::cli::plan::PlanCommand;
use familiar_ai_daemon::cli::stewardship::StewardshipCommand;
use familiar_ai_daemon::cli::worker::WorkerCommand;

#[derive(Debug, Parser)]
#[command(name = "familiar-ai", about = "Familiar command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Submit and observe daemon-owned detached executions.
    Control {
        #[command(subcommand)]
        command: ControlCommand,
    },
    /// Configure native compression or report a measured paired experiment.
    Compress {
        #[command(subcommand)]
        command: CompressCommand,
    },
    /// Manage provider endpoints and enabled models without handling credentials.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect cached authoritative billing or explicitly collect it.
    Billing {
        #[command(subcommand)]
        command: BillingCommand,
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
    /// Query cached local accounting. With no range, preserves the legacy summary.
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
    let cli = Cli::parse();
    match cli.command {
        Command::Control { command } => {
            match familiar_ai_daemon::cli::control::control_command(command) {
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
        Command::Config { command } => match config_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Billing { command } => {
            match familiar_ai_daemon::cli::billing::billing_command(command) {
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
        Command::Next => match familiar_ai_daemon::cli::next::next() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Plan {
            command,
            design_docs,
        } => match familiar_ai_daemon::cli::plan::plan(command, &design_docs) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Onboard { command } => match familiar_ai_daemon::cli::onboard::onboard(command) {
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
        Command::Resume { prd, dry_run } => {
            match familiar_ai_daemon::cli::resume::resume_command(&prd, dry_run) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::ScopeDecisions {
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
        Command::Report { session_id } => {
            match familiar_ai_daemon::cli::report::report_command(session_id.as_deref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
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
        Command::Worker { command } => {
            match familiar_ai_daemon::cli::worker::worker_command(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Stewardship { command } => {
            match familiar_ai_daemon::cli::stewardship::stewardship_command(command) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Backlog { command } => match familiar_ai_daemon::cli::backlog::backlog(command) {
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
            Command::Usage { .. }
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
