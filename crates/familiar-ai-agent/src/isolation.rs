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
        // Built before fork: unsupported kernels and enumeration failures stop
        // the launch here, before any agent process exists.
        let ruleset =
            build_landlock_ruleset(std::path::Path::new("/"), &canonical).map_err(|source| {
                AgentExecutionError::Launch {
                    executable: executable.to_owned(),
                    source: Box::new(source),
                    result: Box::default(),
                }
            })?;
        let mut command = Command::new(executable);
        let mut ruleset = Some(ruleset);
        // SAFETY: the closure runs between fork and exec and performs only
        // Landlock/prctl syscalls on a ruleset prepared above; it allocates
        // nothing. Returning Err aborts the spawn, so the agent can never come
        // into existence unsandboxed.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(move || {
                let ruleset = ruleset
                    .take()
                    .ok_or_else(|| io::Error::other("landlock ruleset consumed before exec"))?;
                restrict_self_fully(ruleset).inspect_err(|error| {
                    // The pre-exec error channel can only carry a raw errno, so
                    // a non-OS error would reach the parent as a meaningless
                    // EINVAL. Report the real reason on inherited stderr.
                    let message = format!("familiar-ai: landlock isolation failed: {error}\n");
                    libc::write(2, message.as_ptr().cast(), message.len());
                })
            });
        }
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

/// Serializes tests that exercise isolation, because the forced-unsupported
/// test sets a process-wide override for the duration of its run.
#[cfg(all(target_os = "linux", test))]
pub(crate) static ISOLATION_TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(target_os = "linux", test))]
fn landlock_forced_unsupported() -> bool {
    std::env::var_os("FAMILIAR_AI_TEST_LANDLOCK_UNSUPPORTED").is_some()
}

#[cfg(all(target_os = "linux", not(test)))]
fn landlock_forced_unsupported() -> bool {
    false
}

/// Every path the sandboxed child may still reach: walking from `root` toward
/// the denied path, each level contributes its entries except the next
/// ancestor on the path. The denied tree and its ancestors are granted
/// nothing, so denial is by omission — Landlock is allowlist-only.
/// `root` is a parameter so tests can pin enumeration over a fixture tree.
#[cfg(target_os = "linux")]
fn landlock_grant_paths(
    root: &std::path::Path,
    canonical_denied: &std::path::Path,
) -> io::Result<Vec<std::path::PathBuf>> {
    let relative = canonical_denied.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "denied path {} is not beneath {}",
                canonical_denied.display(),
                root.display()
            ),
        )
    })?;
    let mut granted = Vec::new();
    let mut level = root.to_path_buf();
    for component in relative.components() {
        let next = level.join(component.as_os_str());
        for entry in std::fs::read_dir(&level)? {
            let path = entry?.path();
            if path != next {
                granted.push(path);
            }
        }
        level = next;
    }
    granted.sort();
    Ok(granted)
}

/// Build the read/execute allowlist for everything outside the denied tree.
/// Pure pre-fork work: an unsupported kernel or a failed enumeration stops the
/// launch before any agent process exists.
#[cfg(target_os = "linux")]
fn build_landlock_ruleset(
    root: &std::path::Path,
    canonical_denied: &std::path::Path,
) -> io::Result<landlock::RulesetCreated> {
    use landlock::{AccessFs, PathBeneath, PathFd, RulesetAttr, RulesetCreatedAttr, ABI};
    if landlock_forced_unsupported() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "landlock forced unsupported for testing",
        ));
    }
    let abi = ABI::V1;
    let read_access = AccessFs::from_read(abi);
    let mut ruleset = landlock::Ruleset::default()
        .handle_access(read_access)
        .map_err(landlock_error)?
        .create()
        .map_err(landlock_error)?;
    let file_access = AccessFs::ReadFile | AccessFs::Execute;
    for path in landlock_grant_paths(root, canonical_denied)? {
        // A path that cannot be opened or stat'd (dangling symlink, denied
        // permission) is simply not granted: that narrows the child's access,
        // never widens it, so an unrelated broken entry must not fail review.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(fd) = PathFd::new(&path) else {
            continue;
        };
        // Directory-only rights (ReadDir) are invalid on a regular file: the
        // kernel rejects such a rule and best-effort compatibility would
        // downgrade the whole ruleset to partially enforced.
        let access = if metadata.is_dir() {
            read_access
        } else {
            file_access
        };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, access))
            .map_err(landlock_error)?;
    }
    Ok(ruleset)
}

#[cfg(target_os = "linux")]
fn landlock_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("landlock isolation unavailable: {error}"),
    )
}

/// Apply the ruleset to the current (post-fork, pre-exec) process. Anything
/// short of full enforcement is an error, so a partially-restricted agent can
/// never run.
#[cfg(target_os = "linux")]
fn restrict_self_fully(ruleset: landlock::RulesetCreated) -> io::Result<()> {
    use landlock::RulesetStatus;
    let status = ruleset.restrict_self().map_err(landlock_error)?;
    match status.ruleset {
        RulesetStatus::FullyEnforced => Ok(()),
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("landlock ruleset not fully enforced: {other:?}"),
        )),
    }
}

/// Test support: whether this kernel can enforce the sandbox at all.
/// Production code never consults this; every launch builds and enforces.
#[cfg(all(target_os = "linux", test))]
pub(crate) fn linux_sandbox_available() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(canonical) = temp.path().canonicalize() else {
        return false;
    };
    build_landlock_ruleset(std::path::Path::new("/"), &canonical).is_ok()
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
    fn grant_enumeration_omits_the_denied_tree_and_its_ancestors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        // root/{keep-a, keep-b, mid/{keep-c, repo/{secret}}}
        let mid = root.join("mid");
        let repo = mid.join("repo");
        fs::create_dir_all(repo.join("inner")).unwrap();
        fs::create_dir_all(mid.join("keep-c")).unwrap();
        fs::create_dir_all(root.join("keep-a")).unwrap();
        fs::write(root.join("keep-b"), "file").unwrap();

        let granted = landlock_grant_paths(&root, &repo).unwrap();
        assert_eq!(
            granted,
            vec![root.join("keep-a"), root.join("keep-b"), mid.join("keep-c")]
        );
        // Neither the denied tree nor any ancestor is granted: denial is by
        // omission, and ancestors are therefore not listable by the child.
        assert!(!granted.contains(&repo));
        assert!(!granted.contains(&mid));
        assert!(!granted.iter().any(|path| path.starts_with(&repo)));
    }

    #[test]
    fn enumeration_rejects_a_denied_path_outside_the_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let outside = std::path::Path::new("/elsewhere/repo");
        assert!(landlock_grant_paths(&root, outside).is_err());
    }

    #[test]
    fn unsupported_landlock_fails_closed_before_the_agent_ever_runs() {
        let _guard = ISOLATION_TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
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

        std::env::set_var("FAMILIAR_AI_TEST_LANDLOCK_UNSUPPORTED", "1");
        let result = crate::CodexAgent::new(agent_exe.to_string_lossy()).execute(
            ExecutionRequest {
                working_directory: temp.path(),
                denied_read_path: Some(&denied),
                prompt: "review",
                prompt_cache_key: None,
                filesystem: crate::FilesystemPolicy::ReadOnly,
                model: None,
                timeout_ms: Some(1_000),
                budget: crate::ExecutionBudget::default(),
            },
            &mut Vec::new(),
        );
        std::env::remove_var("FAMILIAR_AI_TEST_LANDLOCK_UNSUPPORTED");
        assert!(matches!(result, Err(AgentExecutionError::Launch { .. })));
        assert!(
            !ran_marker.exists(),
            "the agent executable must never run without a proven sandbox"
        );
    }
}
