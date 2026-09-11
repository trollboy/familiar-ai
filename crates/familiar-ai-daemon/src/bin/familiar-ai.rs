use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli;

use cli::backlog::BacklogCommand;
use cli::billing::BillingCommand;
use cli::compress::CompressCommand;
use cli::config::ConfigCommand;
use cli::control::ControlCommand;
use cli::onboard::OnboardCommand;
use cli::plan::PlanCommand;
use cli::stewardship::StewardshipCommand;
use cli::worker::WorkerCommand;

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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Control { command } => match cli::control::control_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Compress { command } => match cli::compress::compress_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Config { command } => match cli::config::config_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Billing { command } => match cli::billing::billing_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Status { repository } => match familiar_ai_daemon::config_cli::execute(
            familiar_ai_daemon::config_cli::ConfigAction::Status { repository },
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Preflight => match cli::preflight::preflight_command() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Next => match cli::next::next() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Plan {
            command,
            design_docs,
        } => match cli::plan::plan(command, &design_docs) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Onboard { command } => match cli::onboard::onboard(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Run { prd_path } => match cli::run::run(&prd_path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let code = error.exit_code();
                eprintln!("error: {error}");
                code.and_then(|value| u8::try_from(value).ok())
                    .map_or(ExitCode::FAILURE, ExitCode::from)
            }
        },
        Command::Resume { prd, dry_run } => match cli::resume::resume_command(&prd, dry_run) {
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
        } => match cli::scope_decisions::scope_decisions(
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
        } => match cli::drive::drive_command(
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
        Command::History { limit, verbose } => match cli::history::history(limit, verbose) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Usage {
            start,
            end,
            bucket,
            group_by,
            filters,
            dense,
        } => match cli::usage::usage(
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
            match cli::report::report_command(session_id.as_deref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => fail(error),
            }
        }
        Command::Deliver {
            ownership_record,
            to,
        } => match cli::deliver::deliver_command(&ownership_record, to.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Worker { command } => match cli::worker::worker_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Stewardship { command } => match cli::stewardship::stewardship_command(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Backlog { command } => match cli::backlog::backlog(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
    }
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
