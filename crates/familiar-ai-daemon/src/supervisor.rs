//! Installation and observability for the native per-user supervisor.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use familiar_ai_core::config::WorkerConfig;
use familiar_ai_core::AppPaths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Launchd,
    Systemd,
}

#[derive(Debug)]
pub struct Spec {
    pub backend: Backend,
    pub label: String,
    pub definition: PathBuf,
    pub rendered: String,
}

#[derive(Debug)]
pub struct Status {
    pub backend: Backend,
    pub definition: PathBuf,
    pub installed: bool,
    pub supervisor_state: String,
    pub blockers: Vec<String>,
}

pub fn detect() -> Result<Backend, String> {
    #[cfg(target_os = "macos")]
    {
        return Ok(Backend::Launchd);
    }
    #[cfg(target_os = "linux")]
    {
        return Ok(Backend::Systemd);
    }
    #[allow(unreachable_code)]
    Err(format!(
        "unsupported worker platform: {}",
        std::env::consts::OS
    ))
}

pub fn spec(
    executable: &Path,
    repository: &Path,
    paths: &AppPaths,
    config: &WorkerConfig,
) -> Result<Spec, String> {
    let backend = detect()?; // platform gate precedes all filesystem work/claims
    config.validate()?;
    if !executable.is_absolute() {
        return Err("worker executable must be an absolute path".into());
    }
    if !repository.is_absolute() {
        return Err("worker repository must be an absolute path".into());
    }
    let path = std::env::var("PATH")
        .map_err(|_| "PATH is required for the audited worker environment".to_owned())?;
    let stdout = paths.log_dir.join(format!("{}.stdout.log", config.label));
    let stderr = paths.log_dir.join(format!("{}.stderr.log", config.label));
    let (definition, rendered) = match backend {
        Backend::Launchd => {
            let home = home_dir()?;
            let definition = home
                .join("Library/LaunchAgents")
                .join(format!("{}.plist", config.label));
            let rendered = crate::launchd::plist(
                &config.label,
                executable,
                repository,
                &stdout,
                &stderr,
                &path,
                config.restart_throttle_secs,
                config.max_prds_per_run,
            )?;
            (definition, rendered)
        }
        Backend::Systemd => {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or(home_dir()?.join(".config"));
            let definition = base
                .join("systemd/user")
                .join(format!("{}.service", config.label));
            let rendered = crate::systemd::unit(
                &config.label,
                executable,
                repository,
                &stdout,
                &stderr,
                &path,
                config.restart_throttle_secs,
                config.max_prds_per_run,
            )?;
            (definition, rendered)
        }
    };
    Ok(Spec {
        backend,
        label: config.label.clone(),
        definition,
        rendered,
    })
}

pub fn validate(spec: &Spec, repository: &Path) -> Result<(), Vec<String>> {
    let mut blockers = Vec::new();
    if !repository.is_dir() {
        blockers.push(format!(
            "repository does not exist or is not a directory: {}",
            repository.display()
        ));
    }
    if let Some(parent) = spec.definition.parent() {
        if parent.exists() && !parent.is_dir() {
            blockers.push(format!(
                "supervisor definition parent is not a directory: {}",
                parent.display()
            ));
        }
    }
    if spec.rendered.is_empty() {
        blockers.push("supervisor definition rendered empty".into());
    }
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(blockers)
    }
}

pub fn install(spec: &Spec, repository: &Path, log_dir: &Path) -> Result<bool, String> {
    validate(spec, repository).map_err(|v| v.join("; "))?;
    fs::create_dir_all(log_dir).map_err(|e| format!("cannot create log directory: {e}"))?;
    fs::create_dir_all(spec.definition.parent().expect("definition has parent"))
        .map_err(|e| format!("cannot create supervisor directory: {e}"))?;
    let prior = fs::read_to_string(&spec.definition).ok();
    let changed = prior.as_deref() != Some(&spec.rendered);
    if changed && prior.is_some() {
        deactivate(spec)?;
    }
    if changed {
        fs::write(&spec.definition, &spec.rendered)
            .map_err(|e| format!("cannot write {}: {e}", spec.definition.display()))?;
    }
    activate(spec)?;
    Ok(changed)
}

pub fn uninstall(spec: &Spec) -> Result<bool, String> {
    deactivate(spec)?;
    match fs::remove_file(&spec.definition) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot remove {}: {e}", spec.definition.display())),
    }
}

pub fn status(spec: &Spec, repository: &Path) -> Status {
    let mut blockers = validate(spec, repository).err().unwrap_or_default();
    let installed = spec.definition.is_file();
    if !installed {
        blockers.push(format!(
            "supervisor definition is not installed: {}",
            spec.definition.display()
        ));
    }
    let supervisor_state = query(spec).unwrap_or_else(|e| {
        blockers.push(e);
        "unavailable".into()
    });
    Status {
        backend: spec.backend,
        definition: spec.definition.clone(),
        installed,
        supervisor_state,
        blockers,
    }
}

fn activate(spec: &Spec) -> Result<(), String> {
    match spec.backend {
        Backend::Launchd => {
            let domain = format!("gui/{}", unsafe { libc::getuid() });
            let result = run(
                "launchctl",
                &["bootstrap", &domain, &spec.definition.display().to_string()],
            );
            if result.is_err() && query(spec).is_err() {
                result?;
            }
            run(
                "launchctl",
                &["kickstart", &format!("{domain}/{}", spec.label)],
            )
        }
        Backend::Systemd => {
            run("systemctl", &["--user", "daemon-reload"])?;
            run(
                "systemctl",
                &[
                    "--user",
                    "enable",
                    "--now",
                    &format!("{}.service", spec.label),
                ],
            )
        }
    }
}

fn deactivate(spec: &Spec) -> Result<(), String> {
    if !spec.definition.exists() {
        return Ok(());
    }
    match spec.backend {
        Backend::Launchd => run(
            "launchctl",
            &[
                "bootout",
                &format!("gui/{}/{}", unsafe { libc::getuid() }, spec.label),
            ],
        ),
        Backend::Systemd => {
            run(
                "systemctl",
                &[
                    "--user",
                    "disable",
                    "--now",
                    &format!("{}.service", spec.label),
                ],
            )?;
            run("systemctl", &["--user", "daemon-reload"])
        }
    }
}

fn query(spec: &Spec) -> Result<String, String> {
    match spec.backend {
        Backend::Launchd => run_output(
            "launchctl",
            &[
                "print",
                &format!("gui/{}/{}", unsafe { libc::getuid() }, spec.label),
            ],
        ),
        Backend::Systemd => run_output(
            "systemctl",
            &[
                "--user",
                "show",
                &format!("{}.service", spec.label),
                "--property=LoadState,ActiveState,SubState,Result",
                "--no-pager",
            ],
        ),
    }
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    run_output(program, args).map(|_| ())
}
fn run_output(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot execute {program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        Ok(if stdout.is_empty() {
            "ok".into()
        } else {
            stdout
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(format!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            output.status,
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| "HOME must be an absolute path for worker installation".into())
}

/// Harmless durable fixture modeling a supervisor retry. It never invokes an
/// agent or touches Familiar's database.
pub fn run_fixture(root: &Path) -> Result<String, String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let dispatch = root.join("dispatch");
    let recovered = root.join("recovered");
    let report = root.join("report");
    if !dispatch.exists() {
        fs::write(&dispatch, "first dispatch\n").map_err(|e| e.to_string())?;
        return Err("fixture requested failure restart".into());
    }
    if !recovered.exists() {
        fs::write(&recovered, "recovered\n").map_err(|e| e.to_string())?;
    }
    if !report.exists() {
        fs::write(&report, "one report\n").map_err(|e| e.to_string())?;
    }
    Ok("first_dispatch=true failure_restart=true recovery=true reports=1".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_fails_once_recovers_and_reports_once() {
        let temp = tempfile::tempdir().unwrap();
        assert!(run_fixture(temp.path()).is_err());
        assert!(run_fixture(temp.path()).unwrap().contains("reports=1"));
        assert!(run_fixture(temp.path()).unwrap().contains("reports=1"));
        assert_eq!(
            fs::read_to_string(temp.path().join("report")).unwrap(),
            "one report\n"
        );
    }
}
