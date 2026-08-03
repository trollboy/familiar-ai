use std::process::Command;
use std::time::SystemTime;

fn main() {
    // Git SHA
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        if output.status.success() {
            let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo::rustc-env=FAMILIAR_GIT_SHA={sha}");
        }
    }

    // Build date (no chrono in build scripts — use date command or manual formatting)
    if let Ok(output) = Command::new("date").arg("+%Y-%m-%d").output() {
        if output.status.success() {
            let date = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo::rustc-env=FAMILIAR_BUILD_DATE={date}");
        }
    } else {
        // Fallback: use SystemTime epoch math
        let secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let days = secs / 86400;
        println!("cargo::rustc-env=FAMILIAR_BUILD_DATE=epoch-day-{days}");
    }

    // Rust version
    if let Ok(output) = Command::new("rustc").arg("--version").output() {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // "rustc 1.78.0 (..." -> "1.78.0"
            if let Some(v) = version.split_whitespace().nth(1) {
                println!("cargo::rustc-env=FAMILIAR_RUST_VERSION={v}");
            }
        }
    }

    println!("cargo::rerun-if-changed=.git/HEAD");
}
