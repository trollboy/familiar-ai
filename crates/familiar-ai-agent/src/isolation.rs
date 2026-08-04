//! Shared subprocess mechanics used by every adapter: denied-read-path
//! isolation, the line-oriented output stream loop, and the timeout watchdog.

use std::io::{self, BufRead, BufReader, Write};
use std::process::Command;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

use crate::AgentExecutionError;

pub(crate) fn isolated_command(
    executable: &str,
    denied_read_path: Option<&std::path::Path>,
) -> Result<Command, AgentExecutionError> {
    let Some(denied) = denied_read_path else {
        return Ok(Command::new(executable));
    };
    #[cfg(target_os = "macos")]
    {
        let canonical = denied
            .canonicalize()
            .map_err(|source| AgentExecutionError::Launch {
                executable: executable.to_owned(),
                source: Box::new(source),
                result: Box::default(),
            })?;
        let escaped = canonical
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let profile =
            format!("(version 1) (allow default) (deny file-read* (subpath \"{escaped}\"))");
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command.args(["-p", &profile, executable]);
        Ok(command)
    }
    #[cfg(target_os = "linux")]
    {
        let canonical = denied
            .canonicalize()
            .map_err(|source| AgentExecutionError::Launch {
                executable: executable.to_owned(),
                source: Box::new(source),
                result: Box::default(),
            })?;
        probe_linux_sandbox(&canonical).map_err(|source| AgentExecutionError::Launch {
            executable: executable.to_owned(),
            source: Box::new(source),
            result: Box::default(),
        })?;
        let mut command = Command::new(bwrap_program());
        command.args(bwrap_wrapper_argv(&canonical));
        command.arg(executable);
        Ok(command)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = denied;
        Err(AgentExecutionError::Launch {
            executable: executable.to_owned(),
            source: Box::new(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "isolated review filesystem is unavailable on this platform",
            )),
            result: Box::default(),
        })
    }
}

#[cfg(target_os = "linux")]
const BWRAP_EXECUTABLE: &str = "bwrap";

/// Serializes tests that exercise isolation, because the probe-failure test
/// overrides the bwrap program for the duration of its run.
#[cfg(all(target_os = "linux", test))]
pub(crate) static ISOLATION_TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(target_os = "linux", test))]
fn bwrap_program() -> std::ffi::OsString {
    std::env::var_os("FAMILIAR_AI_TEST_BWRAP_OVERRIDE").unwrap_or_else(|| BWRAP_EXECUTABLE.into())
}

#[cfg(all(target_os = "linux", not(test)))]
fn bwrap_program() -> std::ffi::OsString {
    BWRAP_EXECUTABLE.into()
}

/// The pinned bubblewrap wrapper: bind the host filesystem through unchanged,
/// mask the denied tree with an empty tmpfs, and tie the sandbox to the
/// watchdog's process-group kill. Shared by the probe and the real launch so
/// the proven sandbox and the executed sandbox cannot diverge.
#[cfg(target_os = "linux")]
fn bwrap_wrapper_argv(canonical_denied: &std::path::Path) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    vec![
        OsString::from("--dev-bind"),
        OsString::from("/"),
        OsString::from("/"),
        OsString::from("--tmpfs"),
        canonical_denied.as_os_str().to_owned(),
        OsString::from("--die-with-parent"),
        OsString::from("--"),
    ]
}

/// Prove the sandbox can be created before any agent process is spawned.
/// A missing `bwrap`, blocked namespaces, or any non-zero probe exit fails
/// the launch closed with the probe's diagnostic.
#[cfg(target_os = "linux")]
fn probe_linux_sandbox(canonical_denied: &std::path::Path) -> std::io::Result<()> {
    use std::process::Stdio;
    let output = Command::new(bwrap_program())
        .args(bwrap_wrapper_argv(canonical_denied))
        .arg("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| {
            std::io::Error::new(
                source.kind(),
                format!("sandbox probe cannot launch {BWRAP_EXECUTABLE}: {source}"),
            )
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "sandbox probe failed (status {:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ))
    }
}

/// Test support: whether this environment can actually create the sandbox.
/// Production code never consults this; it probes per launch.
#[cfg(all(target_os = "linux", test))]
pub(crate) fn linux_sandbox_available() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    probe_linux_sandbox(temp.path()).is_ok()
}

/// Per-line decision made by an adapter's stream parser.
pub(crate) enum StreamAction {
    /// Human-readable text derived from a recognized event.
    Text(String),
    /// Forward the raw line verbatim; the adapter cannot classify it.
    Forward,
    /// Recognized but produces no output.
    Silent,
}

pub(crate) fn stream_lines(
    reader: impl io::Read,
    output: &mut dyn Write,
    mut parse: impl FnMut(&str) -> StreamAction,
) -> io::Result<()> {
    for line in BufReader::new(reader).lines() {
        let line = line?;
        match parse(&line) {
            StreamAction::Text(text) => {
                writeln!(output, "{text}")?;
                output.flush()?;
            }
            StreamAction::Forward => {
                writeln!(output, "{line}")?;
                output.flush()?;
            }
            StreamAction::Silent => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) type Watchdog = (
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
    Arc<AtomicBool>,
);

/// Kill the child's whole process group at the deadline. The child must have
/// been spawned with `process_group(0)`.
#[cfg(unix)]
pub(crate) fn spawn_watchdog(child_id: u32, timeout_ms: Option<u64>) -> Option<Watchdog> {
    timeout_ms.map(|timeout| {
        let timed_out = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&timed_out);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            if done_rx
                .recv_timeout(Duration::from_millis(timeout))
                .is_err()
            {
                flag.store(true, Ordering::Release);
                unsafe {
                    libc::kill(-(child_id as i32), libc::SIGKILL);
                }
            }
        });
        (done_tx, handle, timed_out)
    })
}

#[cfg(unix)]
pub(crate) fn finish_watchdog(watchdog: Option<Watchdog>) -> bool {
    if let Some((done, handle, flag)) = watchdog {
        let _ = done.send(());
        let _ = handle.join();
        flag.load(Ordering::Acquire)
    } else {
        false
    }
}

#[cfg(all(target_os = "linux", test))]
mod tests {
    use super::*;
    use crate::{CodingAgent, ExecutionRequest};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn wrapper_argv_is_pinned_and_contains_only_the_canonical_path() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().canonicalize().unwrap();
        let argv = bwrap_wrapper_argv(&canonical);
        assert_eq!(
            argv,
            vec![
                std::ffi::OsString::from("--dev-bind"),
                "/".into(),
                "/".into(),
                "--tmpfs".into(),
                canonical.as_os_str().to_owned(),
                "--die-with-parent".into(),
                "--".into(),
            ]
        );
    }

    #[test]
    fn probe_failure_fails_closed_before_the_agent_ever_runs() {
        let _guard = ISOLATION_TEST_ENV.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let ran_marker = temp.path().join("agent-ran");
        let agent_exe = temp.path().join("codex");
        fs::write(
            &agent_exe,
            format!("#!/bin/sh\ntouch {}\nexit 0\n", ran_marker.display()),
        )
        .unwrap();
        let mut permissions = fs::metadata(&agent_exe).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&agent_exe, permissions).unwrap();
        let denied = temp.path().join("denied");
        fs::create_dir_all(&denied).unwrap();

        std::env::set_var(
            "FAMILIAR_AI_TEST_BWRAP_OVERRIDE",
            temp.path().join("no-such-bwrap"),
        );
        let result = crate::CodexAgent::new(agent_exe.to_string_lossy()).execute(
            ExecutionRequest {
                working_directory: temp.path(),
                denied_read_path: Some(&denied),
                prompt: "review",
                filesystem: crate::FilesystemPolicy::ReadOnly,
                model: None,
                timeout_ms: Some(1_000),
            },
            &mut Vec::new(),
        );
        std::env::remove_var("FAMILIAR_AI_TEST_BWRAP_OVERRIDE");
        assert!(matches!(result, Err(AgentExecutionError::Launch { .. })));
        assert!(
            !ran_marker.exists(),
            "the agent executable must never run without a proven sandbox"
        );
    }
}
