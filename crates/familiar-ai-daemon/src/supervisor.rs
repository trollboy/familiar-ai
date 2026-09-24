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

#[derive(Debug)]
pub struct DesktopInstallSpec {
    pub backend: Backend,
    pub definitions: Vec<Spec>,
    executables: [PathBuf; 2],
}

impl DesktopInstallSpec {
    /// The executables each definition runs, in definition order.
    pub fn executables(&self) -> &[PathBuf; 2] {
        &self.executables
    }
}

/// The daemon and desktop are supervised independently: stopping or crashing
/// the UI cannot stop active work, and restarting the daemon does not require
/// replacing the WebView process.
pub fn desktop_install_spec(
    daemon_executable: &Path,
    desktop_executable: &Path,
    paths: &AppPaths,
) -> Result<DesktopInstallSpec, String> {
    let backend = detect()?;
    for (name, executable) in [
        ("daemon executable", daemon_executable),
        ("desktop executable", desktop_executable),
    ] {
        if !executable.is_absolute() {
            return Err(format!(
                "{name} must be an absolute path: {}",
                executable.display()
            ));
        }
    }
    let toolchain_path = std::env::var("PATH")
        .map_err(|_| "PATH is required for the audited desktop environment".to_string())?;
    let definitions = match backend {
        Backend::Launchd => {
            let base = home_dir()?.join("Library/LaunchAgents");
            vec![
                Spec {
                    backend,
                    label: "com.trollboy.familiar.daemon".into(),
                    definition: base.join("com.trollboy.familiar.daemon.plist"),
                    rendered: desktop_launchd(
                        "com.trollboy.familiar.daemon",
                        daemon_executable,
                        &paths.log_dir.join("daemon.stdout.log"),
                        &paths.log_dir.join("daemon.stderr.log"),
                        &toolchain_path,
                        false,
                    ),
                },
                Spec {
                    backend,
                    label: "com.trollboy.familiar.desktop".into(),
                    definition: base.join("com.trollboy.familiar.desktop.plist"),
                    rendered: desktop_launchd(
                        "com.trollboy.familiar.desktop",
                        desktop_executable,
                        &paths.log_dir.join("desktop.stdout.log"),
                        &paths.log_dir.join("desktop.stderr.log"),
                        &toolchain_path,
                        true,
                    ),
                },
            ]
        }
        Backend::Systemd => {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or(home_dir()?.join(".config"))
                .join("systemd/user");
            vec![
                Spec {
                    backend,
                    label: "familiar-ai-daemon".into(),
                    definition: base.join("familiar-ai-daemon.service"),
                    rendered: desktop_systemd(
                        "Familiar daemon",
                        daemon_executable,
                        &toolchain_path,
                        false,
                    ),
                },
                Spec {
                    backend,
                    label: "familiar-ai-desktop".into(),
                    definition: base.join("familiar-ai-desktop.service"),
                    rendered: desktop_systemd(
                        "Familiar desktop",
                        desktop_executable,
                        &toolchain_path,
                        true,
                    ),
                },
            ]
        }
    };
    Ok(DesktopInstallSpec {
        backend,
        definitions,
        executables: [
            daemon_executable.to_path_buf(),
            desktop_executable.to_path_buf(),
        ],
    })
}

pub fn install_desktop(spec: &DesktopInstallSpec, log_dir: &Path) -> Result<Vec<bool>, String> {
    for (definition, executable) in spec.definitions.iter().zip(&spec.executables) {
        if !executable.is_file() {
            return Err(format!(
                "{} executable does not exist: {}",
                definition.label,
                executable.display()
            ));
        }
    }
    fs::create_dir_all(log_dir).map_err(|e| format!("cannot create log directory: {e}"))?;
    let mut changed = Vec::new();
    for definition in &spec.definitions {
        fs::create_dir_all(definition.definition.parent().expect("definition parent"))
            .map_err(|e| format!("cannot create supervisor directory: {e}"))?;
        let prior = fs::read_to_string(&definition.definition).ok();
        let differs = prior.as_deref() != Some(&definition.rendered);
        if differs && prior.is_some() {
            deactivate(definition)?;
        }
        if differs {
            fs::write(&definition.definition, &definition.rendered)
                .map_err(|e| format!("cannot write {}: {e}", definition.definition.display()))?;
        }
        activate(definition)?;
        changed.push(differs);
    }
    Ok(changed)
}

pub fn uninstall_desktop(spec: &DesktopInstallSpec) -> Result<Vec<bool>, String> {
    let mut removed = Vec::new();
    // UI first, daemon second: no UI reconnect storm while intentional
    // daemon shutdown is in progress.
    for definition in spec.definitions.iter().rev() {
        deactivate(definition)?;
        removed.push(match fs::remove_file(&definition.definition) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(format!(
                    "cannot remove {}: {error}",
                    definition.definition.display()
                ))
            }
        });
    }
    Ok(removed)
}

pub fn desktop_status(spec: &DesktopInstallSpec) -> Vec<Status> {
    spec.definitions
        .iter()
        .zip(&spec.executables)
        .map(|(definition, expected)| {
            let installed = definition.definition.is_file();
            let mut blockers = Vec::new();
            if !installed {
                blockers.push(format!(
                    "definition is not installed: {}",
                    definition.definition.display()
                ));
            } else {
                let program = fs::read_to_string(&definition.definition)
                    .ok()
                    .and_then(|text| installed_program(definition.backend, &text));
                blockers.extend(program_blockers(expected, program.as_deref()));
            }
            let supervisor_state = query(definition).unwrap_or_else(|error| {
                blockers.push(error);
                "unavailable".into()
            });
            Status {
                backend: definition.backend,
                definition: definition.definition.clone(),
                installed,
                supervisor_state,
                blockers,
            }
        })
        .collect()
}

/// The executable an installed definition runs, read back from the file the
/// supervisor loaded rather than from what this CLI would render.
fn installed_program(backend: Backend, definition: &str) -> Option<PathBuf> {
    match backend {
        Backend::Launchd => {
            let rest = definition.split("<key>ProgramArguments</key>").nth(1)?;
            let start = rest.find("<string>")? + "<string>".len();
            let end = rest[start..].find("</string>")? + start;
            Some(PathBuf::from(xml_unescape(&rest[start..end])))
        }
        Backend::Systemd => definition.lines().find_map(|line| {
            let value = line.trim().strip_prefix("ExecStart=")?;
            Some(PathBuf::from(
                value
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .replace("\\x20", " ")
                    .replace("%%", "%"),
            ))
        }),
    }
}

/// FAM-BUG-084: a definition that runs a different binary than the one
/// `install` would write is how a rebuilt desktop stays three days old. The
/// mismatch is a blocker on its own; when both binaries exist and differ, the
/// message says which build the supervisor actually runs.
fn program_blockers(expected: &Path, installed: Option<&Path>) -> Vec<String> {
    let Some(installed) = installed else {
        return vec!["installed definition has no readable program".into()];
    };
    if installed == expected {
        return Vec::new();
    }
    let mut blocker = format!(
        "installed definition runs {} but `ops desktop install` would use {}",
        installed.display(),
        expected.display()
    );
    match (fs::read(installed), fs::read(expected)) {
        (Ok(running), Ok(sibling)) if running != sibling => {
            blocker.push_str("; the two binaries differ, so rebuilding does not change what runs");
        }
        (Err(_), _) => blocker.push_str("; the installed program does not exist"),
        _ => {}
    }
    blocker.push_str(" (re-run `ops desktop install`, or pass --desktop deliberately)");
    vec![blocker]
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn desktop_launchd(
    label: &str,
    executable: &Path,
    stdout: &Path,
    stderr: &Path,
    toolchain_path: &str,
    interactive: bool,
) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{}</string>\n<key>ProgramArguments</key><array><string>{}</string></array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ThrottleInterval</key><integer>10</integer>\n<key>ProcessType</key><string>{}</string>\n<key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
        xml_escape(label),
        xml_escape(&executable.display().to_string()),
        if interactive { "Interactive" } else { "Background" },
        xml_escape(toolchain_path),
        xml_escape(&stdout.display().to_string()),
        xml_escape(&stderr.display().to_string()),
    )
}

fn desktop_systemd(description: &str, executable: &Path, path: &str, graphical: bool) -> String {
    let target = if graphical {
        "graphical-session.target"
    } else {
        "default.target"
    };
    format!(
        "[Unit]\nDescription={description}\nStartLimitIntervalSec=300\nStartLimitBurst=5\n\n[Service]\nType=simple\nExecStart={}\nEnvironment=\"PATH={}\"\nRestart=on-failure\nRestartSec=10\n\n[Install]\nWantedBy={target}\n",
        executable.display().to_string().replace(' ', "\\x20").replace('%', "%%"),
        path.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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

    #[test]
    fn installed_program_is_read_back_from_both_definition_formats() {
        let plist = desktop_launchd(
            "com.example.desktop",
            Path::new("/Users/me/Applications/Familiar.app/Contents/MacOS/familiar-ai-desktop"),
            Path::new("/tmp/desktop.out"),
            Path::new("/tmp/desktop.err"),
            "/usr/bin:/bin",
            true,
        );
        assert_eq!(
            installed_program(Backend::Launchd, &plist).unwrap(),
            Path::new("/Users/me/Applications/Familiar.app/Contents/MacOS/familiar-ai-desktop")
        );
        let unit = desktop_systemd(
            "Familiar desktop",
            Path::new("/home/me/my bin/familiar-ai-desktop"),
            "/usr/bin:/bin",
            true,
        );
        assert_eq!(
            installed_program(Backend::Systemd, &unit).unwrap(),
            Path::new("/home/me/my bin/familiar-ai-desktop")
        );
        assert!(installed_program(Backend::Launchd, "<plist/>").is_none());
    }

    #[test]
    fn stale_desktop_program_is_a_status_blocker() {
        let temp = tempfile::tempdir().unwrap();
        let sibling = temp.path().join("familiar-ai-desktop");
        let bundle = temp
            .path()
            .join("Familiar.app/Contents/MacOS/familiar-ai-desktop");
        fs::create_dir_all(bundle.parent().unwrap()).unwrap();
        fs::write(&sibling, b"new build").unwrap();
        fs::write(&bundle, b"old build").unwrap();

        assert!(program_blockers(&sibling, Some(&sibling)).is_empty());

        let blockers = program_blockers(&sibling, Some(&bundle));
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("Familiar.app"));
        assert!(blockers[0].contains("the two binaries differ"));

        fs::write(&bundle, b"new build").unwrap();
        assert!(!program_blockers(&sibling, Some(&bundle))[0].contains("differ"));

        assert!(
            program_blockers(&sibling, Some(Path::new("/nonexistent/desktop")))[0]
                .contains("does not exist")
        );
        assert!(program_blockers(&sibling, None)[0].contains("no readable program"));
    }

    #[test]
    fn desktop_services_are_independent_and_restart_only_on_failure() {
        let daemon = desktop_launchd(
            "com.example.daemon",
            Path::new("/opt/familiar-ai-daemon"),
            Path::new("/tmp/daemon.out"),
            Path::new("/tmp/daemon.err"),
            "/usr/bin:/bin",
            false,
        );
        let desktop = desktop_launchd(
            "com.example.desktop",
            Path::new("/opt/Familiar.app/Contents/MacOS/familiar-ai-desktop"),
            Path::new("/tmp/desktop.out"),
            Path::new("/tmp/desktop.err"),
            "/usr/bin:/bin",
            true,
        );
        assert!(daemon.contains("<string>Background</string>"));
        assert!(desktop.contains("<string>Interactive</string>"));
        assert!(daemon.contains("<key>SuccessfulExit</key><false/>"));
        assert_ne!(daemon, desktop);

        let linux_daemon = desktop_systemd(
            "Familiar daemon",
            Path::new("/opt/familiar-ai-daemon"),
            "/usr/bin:/bin",
            false,
        );
        let linux_desktop = desktop_systemd(
            "Familiar desktop",
            Path::new("/opt/familiar-ai-desktop"),
            "/usr/bin:/bin",
            true,
        );
        assert!(linux_daemon.contains("WantedBy=default.target"));
        assert!(linux_desktop.contains("WantedBy=graphical-session.target"));
        assert!(linux_desktop.contains("Restart=on-failure"));
    }
}
