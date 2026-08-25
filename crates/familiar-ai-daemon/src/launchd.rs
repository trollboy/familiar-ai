//! Deterministic launchd worker definition: restart only after failure, write
//! durable logs, and run one finite configured warrant.

use std::path::Path;

pub fn plist(
    label: &str,
    executable: &Path,
    repository: &Path,
    stdout_log: &Path,
    stderr_log: &Path,
    toolchain_path: &str,
) -> Result<String, String> {
    if label.trim().is_empty()
        || !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
    {
        return Err("launchd label must contain only letters, digits, '.', '-', or '_'".into());
    }
    for (name, path) in [
        ("executable", executable),
        ("repository", repository),
        ("stdout log", stdout_log),
        ("stderr log", stderr_log),
    ] {
        if !path.is_absolute() {
            return Err(format!("launchd {name} path must be absolute"));
        }
    }
    if toolchain_path.trim().is_empty() || toolchain_path.contains('\0') {
        return Err("launchd toolchain PATH must be non-empty and contain no NUL bytes".into());
    }
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>worker</string>
    <string>run</string>
    <string>{}</string>
    <string>--max-prds</string>
    <string>1</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Background</string>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>{}</string></dict>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        xml(label),
        xml(&executable.display().to_string()),
        xml(&repository.display().to_string()),
        xml(toolchain_path),
        xml(&stdout_log.display().to_string()),
        xml(&stderr_log.display().to_string()),
    ))
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_is_logged_and_failure_restart_only() {
        let rendered = plist(
            "com.example.familiar.fixture",
            Path::new("/opt/familiar-ai"),
            Path::new("/tmp/repo&fixture"),
            Path::new("/tmp/out.log"),
            Path::new("/tmp/err.log"),
            "/opt/homebrew/bin:/usr/bin:/bin",
        )
        .unwrap();
        assert!(rendered.contains("<key>SuccessfulExit</key><false/>"));
        assert!(rendered.contains("<string>worker</string>"));
        assert!(rendered.contains("/tmp/repo&amp;fixture"));
        assert!(
            rendered.contains("<key>PATH</key><string>/opt/homebrew/bin:/usr/bin:/bin</string>")
        );
    }

    #[test]
    fn relative_paths_and_unsafe_labels_are_rejected() {
        assert!(plist(
            "bad label",
            Path::new("familiar-ai"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/out"),
            Path::new("/tmp/err"),
            "/usr/bin:/bin",
        )
        .is_err());
        assert!(plist(
            "com.example.familiar",
            Path::new("/opt/familiar-ai"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/out"),
            Path::new("/tmp/err"),
            "",
        )
        .is_err());
    }
}
