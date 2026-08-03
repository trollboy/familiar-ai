use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};

use clap::{Parser, Subcommand};
use familiar_core::{AppPaths, Config};
use familiar_daemon::run::execute;
use familiar_storage::{Database, ExecutionHistoryRepository};

#[derive(Debug, Parser)]
#[command(name = "familiar", about = "Familiar command-line interface")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { prd_path } => match execute(&prd_path, "codex") {
            Ok(status) => exit_code(status),
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
    }
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
fn exit_code(status: ExitStatus) -> ExitCode {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map(ExitCode::from)
        .unwrap_or(ExitCode::FAILURE)
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
    }
}
