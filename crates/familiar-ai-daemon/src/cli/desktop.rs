//! Install and inspect the independently supervised daemon and Tauri desktop.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use familiar_ai_core::AppPaths;

#[derive(Debug, Subcommand)]
pub enum DesktopCommand {
    /// Install and start independent per-user daemon and desktop definitions.
    Install {
        #[arg(long)]
        daemon: Option<PathBuf>,
        #[arg(long)]
        desktop: Option<PathBuf>,
    },
    /// Stop and remove both definitions while preserving configuration/history.
    Uninstall {
        #[arg(long)]
        daemon: Option<PathBuf>,
        #[arg(long)]
        desktop: Option<PathBuf>,
    },
    /// Report both native supervisor definitions and their live state.
    Status {
        #[arg(long)]
        daemon: Option<PathBuf>,
        #[arg(long)]
        desktop: Option<PathBuf>,
    },
}

pub fn desktop(command: DesktopCommand) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let (daemon, desktop) = match &command {
        DesktopCommand::Install { daemon, desktop } => {
            executables(daemon.as_deref(), desktop.as_deref(), true)?
        }
        DesktopCommand::Uninstall { daemon, desktop }
        | DesktopCommand::Status { daemon, desktop } => {
            executables(daemon.as_deref(), desktop.as_deref(), false)?
        }
    };
    let spec = crate::supervisor::desktop_install_spec(&daemon, &desktop, &paths)?;
    match command {
        DesktopCommand::Install { .. } => {
            let changed = crate::supervisor::install_desktop(&spec, &paths.log_dir)?;
            for (definition, changed) in spec.definitions.iter().zip(changed) {
                println!(
                    "installed={} changed={} definition={}",
                    definition.label,
                    changed,
                    definition.definition.display()
                );
            }
        }
        DesktopCommand::Uninstall { .. } => {
            let removed = crate::supervisor::uninstall_desktop(&spec)?;
            for (definition, removed) in spec.definitions.iter().rev().zip(removed) {
                println!(
                    "uninstalled={} removed={} durable_history=preserved",
                    definition.label, removed
                );
            }
        }
        DesktopCommand::Status { .. } => {
            let statuses = crate::supervisor::desktop_status(&spec);
            let mut blockers = Vec::new();
            for (definition, status) in spec.definitions.iter().zip(statuses) {
                println!(
                    "service={} backend={:?} installed={} definition={} state={}",
                    definition.label,
                    status.backend,
                    status.installed,
                    status.definition.display(),
                    status.supervisor_state
                );
                blockers.extend(
                    status
                        .blockers
                        .into_iter()
                        .map(|blocker| format!("{}: {blocker}", definition.label)),
                );
            }
            if !blockers.is_empty() {
                for blocker in &blockers {
                    println!("blocker={blocker}");
                }
                return Err(format!("desktop has {} blocker(s)", blockers.len()));
            }
        }
    }
    Ok(())
}

fn executables(
    daemon: Option<&Path>,
    desktop: Option<&Path>,
    require_existing: bool,
) -> Result<(PathBuf, PathBuf), String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let directory = current
        .parent()
        .ok_or_else(|| "current executable has no parent".to_string())?;
    let daemon = daemon
        .map(Path::to_path_buf)
        .unwrap_or_else(|| directory.join("familiar-ai-daemon"));
    let desktop = desktop
        .map(resolve_desktop_app)
        .unwrap_or_else(|| directory.join("familiar-ai-desktop"));
    if require_existing {
        Ok((
            canonical(&daemon, "daemon")?,
            canonical(&desktop, "desktop")?,
        ))
    } else {
        Ok((absolute(daemon)?, absolute(desktop)?))
    }
}

fn resolve_desktop_app(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|extension| extension == "app") {
        path.join("Contents/MacOS/familiar-ai-desktop")
    } else {
        path.to_path_buf()
    }
}

fn canonical(path: &Path, name: &str) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|e| format!("cannot resolve {name} executable {}: {e}", path.display()))
}

fn absolute(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path))
    }
}
