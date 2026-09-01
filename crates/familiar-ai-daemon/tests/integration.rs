use std::io::Read;
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;

fn daemon_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("familiar-ai-daemon");
    path
}

#[test]
fn daemon_starts_and_stops_on_sigterm() {
    let tmp = tempdir().unwrap();
    let pid_path = tmp.path().join("test.pid");

    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[daemon]
heartbeat_interval_secs = 1
pid_file = "{pid}"
socket_path = "{sock}"

[tray]
enabled = false

[database]
path = "{db}"

[logging]
level = "info"
format = "json"
"#,
            pid = pid_path.display(),
            sock = tmp.path().join("test.sock").display(),
            db = tmp.path().join("test.db").display(),
        ),
    )
    .unwrap();

    let bin = daemon_bin();
    assert!(bin.exists(), "daemon binary not found at {}", bin.display());

    // Start daemon with stderr piped so we can read logs. FAM-BUG-030: the
    // daemon runs in its OWN process group so that any helper it spawns (the
    // tray on macOS default-feature builds) can be killed with it — a leaked
    // grandchild inheriting a pipe otherwise blocks the unbounded stderr read
    // below, or holds cargo's pipe open after every test has passed.
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(&bin)
        .args(["--config", config_path.to_str().unwrap()])
        .env("XDG_RUNTIME_DIR", tmp.path().join("runtime"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("HOME", tmp.path().join("home"))
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("failed to start daemon");

    // Take stderr handle immediately to avoid holding child borrow
    let mut stderr_handle = child.stderr.take().expect("stderr not piped");

    // Wait for PID file to appear
    for _ in 0..50 {
        if pid_path.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(pid_path.exists(), "PID file was not created");

    // Verify PID file contains the child's PID
    let pid_contents = std::fs::read_to_string(&pid_path).unwrap();
    let file_pid: u32 = pid_contents.trim().parse().unwrap();
    assert_eq!(file_pid, child.id());

    // Wait for at least one heartbeat
    std::thread::sleep(Duration::from_secs(2));

    // Send SIGTERM to the daemon's whole process group.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }

    // Wait for exit without allowing a signal-handling regression to hang the
    // entire workspace test suite indefinitely.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("failed to poll daemon") {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            panic!("daemon did not exit within 10 seconds of SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "daemon exited with error: {status}");

    // PID file should be cleaned up
    assert!(
        !pid_path.exists(),
        "PID file was not cleaned up after shutdown"
    );

    // Kill anything left in the daemon's process group BEFORE the stderr
    // read: with every writer provably dead, read_to_string is guaranteed to
    // reach EOF instead of blocking on a leaked helper's inherited pipe.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    // Read stderr (JSON format, so each line is a JSON object)
    let mut stderr_output = String::new();
    stderr_handle.read_to_string(&mut stderr_output).ok();

    assert!(
        stderr_output.contains("daemon starting"),
        "missing 'daemon starting' in logs:\n{stderr_output}"
    );
    assert!(
        stderr_output.contains("heartbeat"),
        "missing 'heartbeat' in logs:\n{stderr_output}"
    );
    assert!(
        stderr_output.contains("familiar-ai-daemon stopped"),
        "missing 'stopped' in logs:\n{stderr_output}"
    );
}

#[test]
fn daemon_detects_already_running() {
    let tmp = tempdir().unwrap();
    let pid_path = tmp.path().join("test.pid");

    // Write our own PID to simulate an already-running daemon
    std::fs::write(&pid_path, format!("{}", std::process::id())).unwrap();

    let config_path = tmp.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[daemon]
pid_file = "{pid}"
socket_path = "{sock}"

[database]
path = "{db}"
"#,
            pid = pid_path.display(),
            sock = tmp.path().join("test.sock").display(),
            db = tmp.path().join("test.db").display(),
        ),
    )
    .unwrap();

    let bin = daemon_bin();
    assert!(bin.exists(), "daemon binary not found at {}", bin.display());

    let output = Command::new(&bin)
        .args(["--config", config_path.to_str().unwrap()])
        .env("XDG_RUNTIME_DIR", tmp.path().join("runtime"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("HOME", tmp.path().join("home"))
        .output()
        .expect("failed to run daemon");

    assert!(!output.status.success(), "daemon should have failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already running"),
        "expected 'already running' in error: {stderr}"
    );
}
