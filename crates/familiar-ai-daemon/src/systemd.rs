//! Deterministic systemd user service definition.

use std::path::Path;

#[allow(clippy::too_many_arguments)] // native definition has these explicit audited fields
pub fn unit(
    label: &str,
    executable: &Path,
    repository: &Path,
    stdout_log: &Path,
    stderr_log: &Path,
    toolchain_path: &str,
    restart_throttle_secs: u64,
    max_prds: u64,
) -> Result<String, String> {
    if label.is_empty()
        || !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
    {
        return Err("systemd label must contain only letters, digits, '.', '-', or '_'".into());
    }
    for (name, path) in [
        ("executable", executable),
        ("repository", repository),
        ("stdout log", stdout_log),
        ("stderr log", stderr_log),
    ] {
        if !path.is_absolute() {
            return Err(format!("systemd {name} path must be absolute"));
        }
        if path.to_string_lossy().contains('\n') {
            return Err(format!("systemd {name} path must not contain newlines"));
        }
    }
    if toolchain_path.trim().is_empty() || toolchain_path.contains(['\0', '\n']) {
        return Err(
            "systemd toolchain PATH must be non-empty and contain no NUL or newline bytes".into(),
        );
    }
    if restart_throttle_secs == 0 || max_prds == 0 {
        return Err("systemd restart throttle and max PRDs must be positive".into());
    }
    Ok(format!("[Unit]\nDescription=Familiar persistent worker ({label})\nStartLimitIntervalSec=300\nStartLimitBurst=5\n\n[Service]\nType=oneshot\nExecStart={} worker run {} --max-prds {}\nEnvironment=\"PATH={}\"\nRestart=on-failure\nRestartSec={}\nStandardOutput=append:{}\nStandardError=append:{}\n\n[Install]\nWantedBy=default.target\n", escape(executable), escape(repository), max_prds, environment_escape(toolchain_path), restart_throttle_secs, specifier_escape(&stdout_log.display().to_string()), specifier_escape(&stderr_log.display().to_string())))
}

fn escape(path: &Path) -> String {
    specifier_escape(&path.display().to_string()).replace(' ', "\\x20")
}
fn specifier_escape(value: &str) -> String {
    value.replace('%', "%%")
}
fn environment_escape(value: &str) -> String {
    specifier_escape(value)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_is_bounded_logged_and_throttled() {
        let rendered = unit(
            "ai.familiar.worker",
            Path::new("/opt/familiar ai"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/out"),
            Path::new("/tmp/err"),
            "/usr/bin:/bin",
            12,
            1,
        )
        .unwrap();
        assert!(rendered.contains("Restart=on-failure\nRestartSec=12"));
        assert!(rendered.contains("StartLimitBurst=5"));
        assert!(rendered.contains("--max-prds 1"));
        assert!(rendered.contains("/opt/familiar\\x20ai"));
    }
}
