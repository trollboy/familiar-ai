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
    #[cfg(not(target_os = "macos"))]
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
