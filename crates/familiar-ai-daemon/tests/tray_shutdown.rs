//! The tray build must honour SIGTERM.
//!
//! `integration.rs` already covers daemon shutdown, but it sets
//! `[tray] enabled = false`, so it exercises the headless path in every build.
//! The real tray path parks the main thread inside `gtk::main()`, which returns
//! only when something calls `gtk::main_quit()` — and for a long while nothing
//! did. SIGTERM stopped the workers and left the process alive with its icon
//! still in the tray and its PID file still on disk, and no test noticed
//! because the suite builds this crate with `--no-default-features`.
//!
//! This test therefore insists on the combination that was never covered:
//! the `tray` feature compiled in, `[tray] enabled = true`, and a real X
//! server (Xvfb) for GTK to talk to.
#![cfg(all(feature = "tray", target_os = "linux"))]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

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

/// A headless X server for GTK. Killed on drop so a failing assertion cannot
/// leak one into the host's display range.
struct Xvfb {
    child: Child,
    display: String,
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        // SIGTERM, not SIGKILL: an X server removes its socket and lock file
        // on a clean exit and cannot on a kill. Killing it leaked one display
        // number per run out of a pool of twenty, so this test was quietly
        // walking towards a hard "no free X display" failure — which is what
        // it eventually did.
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Whether anything is actually serving this display. The socket file
/// outliving its server is the normal aftermath of a killed X server, and a
/// file alone must not be read as "taken".
fn display_is_live(n: u32) -> bool {
    std::os::unix::net::UnixStream::connect(format!("/tmp/.X11-unix/X{n}")).is_ok()
}

fn start_xvfb() -> Xvfb {
    // Start well above :0 so a developer's real session is never a candidate,
    // and skip any number already claimed rather than fighting for it.
    for n in 90..110 {
        let socket = format!("/tmp/.X11-unix/X{n}");
        if display_is_live(n) {
            continue;
        }
        // Nothing is serving it, so any socket or lock left here is debris
        // from a server that did not exit cleanly. Clear it and reuse the
        // number rather than burning through the pool.
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(format!("/tmp/.X{n}-lock"));
        let child = match Command::new("Xvfb")
            .args([&format!(":{n}"), "-screen", "0", "640x480x24"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            // A missing Xvfb must fail loudly: a test that quietly passes when
            // it cannot run is worse than no test, and this one exists purely
            // because an uncovered path stayed broken.
            Err(e) => panic!(
                "Xvfb is required to exercise the tray shutdown path but could \
                 not be started: {e}. Install it (apt install xvfb) or run this \
                 crate with --no-default-features to skip the tray build."
            ),
        };
        let mut xvfb = Xvfb {
            child,
            display: format!(":{n}"),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if std::path::Path::new(&socket).exists() {
                return xvfb;
            }
            if let Ok(Some(_)) = xvfb.child.try_wait() {
                break; // this display was lost to a race; try the next one
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = xvfb.child.kill();
    }
    panic!("no free X display in :90-:109 for Xvfb");
}

#[test]
fn tray_build_exits_on_sigterm() {
    let xvfb = start_xvfb();

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
enabled = true

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

    use std::os::unix::process::CommandExt;
    let mut child = Command::new(&bin)
        .args(["--config", config_path.to_str().unwrap()])
        .env("DISPLAY", &xvfb.display)
        .env("XDG_RUNTIME_DIR", tmp.path().join("runtime"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("HOME", tmp.path().join("home"))
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("failed to start daemon");

    let mut stderr_handle = child.stderr.take().expect("stderr not piped");

    // Same generous budget as integration.rs: a cold start applies every
    // migration, and this one also waits on GTK and an X connection.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pid_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if !pid_path.exists() {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.wait();
        let mut reason = String::new();
        use std::io::Read as _;
        let _ = stderr_handle.read_to_string(&mut reason);
        panic!("PID file was not created; daemon stderr:\n{reason}");
    }

    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("failed to poll daemon") {
            break status;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            let mut reason = String::new();
            use std::io::Read as _;
            let _ = stderr_handle.read_to_string(&mut reason);
            panic!(
                "tray build did not exit within 10 seconds of SIGTERM — the GTK \
                 main loop is not being quit on shutdown; daemon stderr:\n{reason}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "daemon exited with error: {status}");
    assert!(
        !pid_path.exists(),
        "PID file was not cleaned up after shutdown"
    );

    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}
