use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};

use clap::{Parser, Subcommand};
use familiar_daemon::run::execute;

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Run { prd_path } => execute(&prd_path, "codex"),
    };

    match result {
        Ok(status) => exit_code(status),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
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
    fn parses_run_prd_path() {
        let cli = Cli::try_parse_from(["familiar", "run", "docs/prds/PRD-003.md"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Run { prd_path } if prd_path == PathBuf::from("docs/prds/PRD-003.md")
        ));
    }

    #[test]
    fn rejects_run_without_prd_path() {
        assert!(Cli::try_parse_from(["familiar", "run"]).is_err());
    }
}
