//! `familiar-ai worker` — install and operate a bounded native-supervised
//! worker.

use std::path::PathBuf;

use clap::Subcommand;
use familiar_ai_core::AppPaths;

use super::drive::drive_command;
use super::report::report_command;
use super::shared::effective_repository_config;

#[derive(Debug, Subcommand)]
pub enum WorkerCommand {
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

pub fn worker_command(command: WorkerCommand) -> Result<(), String> {
    match command {
        WorkerCommand::Install { repository } => {
            let (spec, repository, paths) = worker_spec(&repository)?;
            let changed = crate::supervisor::install(&spec, &repository, &paths.log_dir)?;
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
            let removed = crate::supervisor::uninstall(&spec)?;
            println!(
                "uninstalled={} removed={} durable_history=preserved",
                spec.label, removed
            );
            Ok(())
        }
        WorkerCommand::Status { repository } => {
            let (spec, repository, _) = worker_spec(&repository)?;
            let status = crate::supervisor::status(&spec, &repository);
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
            crate::supervisor::validate(&spec, &repository).map_err(|v| v.join("; "))?;
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
            let backend = crate::supervisor::detect()?;
            let root = std::env::temp_dir()
                .join(format!("familiar-ai-worker-fixture-{}", std::process::id()));
            let first = crate::supervisor::run_fixture(&root);
            if first.is_ok() {
                return Err("fixture did not request its failure restart".into());
            }
            let result = crate::supervisor::run_fixture(&root)?;
            let again = crate::supervisor::run_fixture(&root)?;
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
            let rendered = crate::launchd::plist(
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
) -> Result<(crate::supervisor::Spec, PathBuf, AppPaths), String> {
    // Platform detection must happen before canonicalization/config loading so
    // unsupported platforms fail before any work-like activity.
    crate::supervisor::detect()?;
    let executable = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let repository = repository
        .canonicalize()
        .map_err(|e| format!("cannot resolve repository {}: {e}", repository.display()))?;
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &repository)?;
    let spec = crate::supervisor::spec(&executable, &repository, &paths, &config.worker)?;
    Ok((spec, repository, paths))
}
