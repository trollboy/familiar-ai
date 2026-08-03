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
    path.push("familiar-daemon");
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

    // Start daemon with stderr piped so we can read logs
    let mut child = Command::new(&bin)
        .args(["--config", config_path.to_str().unwrap()])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
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

    // Send SIGTERM
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }

    // Wait for exit
    let status = child.wait().expect("failed to wait on daemon");
    assert!(status.success(), "daemon exited with error: {status}");

    // PID file should be cleaned up
    assert!(
        !pid_path.exists(),
        "PID file was not cleaned up after shutdown"
    );

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
        stderr_output.contains("familiar-daemon stopped"),
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
        .output()
        .expect("failed to run daemon");

    assert!(!output.status.success(), "daemon should have failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already running"),
        "expected 'already running' in error: {stderr}"
    );
}
