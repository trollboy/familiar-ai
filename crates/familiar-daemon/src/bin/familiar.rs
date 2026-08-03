use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use familiar_agent::CodexAgent;
use familiar_core::{
    load_manifest, validate_graph, AppPaths, BacklogDiscovery, BacklogManager, BacklogStatusStore,
    BootstrapApplyResult, Config, FilesystemBacklogDiscovery,
};
use familiar_daemon::run::execute;
use familiar_storage::{
    Database, ExecutionHistoryRepository, SqliteBacklogRepository, SqliteBootstrapRepository,
};

#[derive(Debug, Parser)]
#[command(name = "familiar", about = "Familiar command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Select the next eligible repository PRD without executing it.
    Next,
    /// Execute a repository PRD with the locally installed Codex CLI.
    Run { prd_path: PathBuf },
    /// List recent standalone executions.
    History {
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u8).range(1..=100))]
        limit: u8,
        #[arg(long)]
        verbose: bool,
    },
    /// Summarize known standalone execution usage and cost.
    Usage,
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
        Command::Run { prd_path } => match execute(&prd_path, &CodexAgent::new("codex")) {
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => {
                let code = error.exit_code();
                eprintln!("error: {error}");
                code.and_then(|value| u8::try_from(value).ok())
                    .map_or(ExitCode::FAILURE, ExitCode::from)
            }
        },
        Command::History { limit, verbose } => match history(limit, verbose) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Usage => match usage() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
        Command::Backlog { command } => match backlog(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        },
    }
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
    }
    Ok(())
}

fn database() -> Result<Database, String> {
    let paths = AppPaths::new();
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
            Cli::try_parse_from(["familiar", "history", "--limit", "100"])
                .unwrap()
                .command,
            Command::History { limit: 100, .. }
        ));
        assert!(Cli::try_parse_from(["familiar", "history", "--limit", "0"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["familiar", "usage"]).unwrap().command,
            Command::Usage
        ));
        assert!(matches!(
            Cli::try_parse_from(["familiar", "next"]).unwrap().command,
            Command::Next
        ));
        assert!(Cli::try_parse_from(["familiar", "next", "PRD-1.md"]).is_err());
    }
}
