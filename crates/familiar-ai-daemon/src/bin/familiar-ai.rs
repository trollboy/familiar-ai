use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use familiar_ai_core::{
    load_manifest, resolve_run_prd, validate_graph, validate_recovery_attribution, AppPaths,
    BacklogDiscovery, BacklogManager, BacklogRecoveryAction, BacklogStatusStore,
    BootstrapApplyResult, Config, FilesystemBacklogDiscovery,
};
use familiar_ai_daemon::drive::{drive, DriveWarrant};
use familiar_ai_daemon::run::{build_agent, execute_with_config, resolved_agent_entries, AgentSet};
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
    /// Select the next eligible repository PRD without executing it.
    Next,
    /// Execute a repository PRD with the configured coding agent.
    Run { prd_path: PathBuf },
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
    /// Inspect or roll back the historical backlog bootstrap.
    Backlog {
        #[command(subcommand)]
        command: BacklogCommand,
    },
}

#[derive(Debug, Subcommand)]
enum BacklogCommand {
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
        Command::Next => match next() {
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
        Command::Drive {
            max_prds,
            max_cost_microusd,
            max_duration_ms,
        } => match drive_command(max_prds, max_cost_microusd, max_duration_ms) {
            Ok(()) => ExitCode::SUCCESS,
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
        Command::Backlog { command } => match backlog(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
    }
}

/// The CLI composition root: read validated configuration and construct the
/// implementation and reviewer agents deterministically.
fn run(prd_path: &std::path::Path) -> Result<(), familiar_ai_daemon::run::RunError> {
    use familiar_ai_daemon::run::RunError;
    let paths = AppPaths::resolve().map_err(|e| RunError::Config(e.to_string()))?;
    let config = Config::load(Some(&paths.config_dir.join("config.toml")))
        .map_err(|e| RunError::Config(e.to_string()))?;
    let (implementation_entry, reviewer_entry) =
        resolved_agent_entries(&config).map_err(RunError::Config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    execute_with_config(
        prd_path,
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
        },
        &config,
        &paths,
    )
    .map(|_| ())
}

/// Composition root for the unattended driver: same agent construction as
/// `run`, plus a warrant that flags may only tighten.
fn drive_command(
    max_prds: Option<u64>,
    max_cost_microusd: Option<u64>,
    max_duration_ms: Option<u64>,
) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let (implementation_entry, reviewer_entry) = resolved_agent_entries(&config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    let warrant = DriveWarrant::from_config(&config).tightened_by(
        max_prds,
        max_cost_microusd,
        max_duration_ms,
    );
    let summary = drive(
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
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
    Ok(())
}

/// Read-only: renders recorded rows and constructs no agents.
fn report_command(session_id: Option<&str>) -> Result<(), String> {
    let db = database()?;
    let rendered =
        familiar_ai_daemon::report::render(&db, session_id).map_err(|e| e.to_string())?;
    print!("{rendered}");
    Ok(())
}

fn next() -> Result<(), String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    // Resolve Git before opening or migrating storage, preserving the domain's
    // required operation order for invalid working directories.
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let discovered = FilesystemBacklogDiscovery
        .discover(&repository)
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
    let mut manager = BacklogManager::new(FilesystemBacklogDiscovery, store);
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

fn backlog(command: BacklogCommand) -> Result<(), String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let discovered = FilesystemBacklogDiscovery
        .discover(&repository)
        .map_err(|e| e.to_string())?;
    if discovered.is_empty() {
        return Err("backlog is empty".into());
    }
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    let mut db = database()?;
    match command {
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
    }
}
