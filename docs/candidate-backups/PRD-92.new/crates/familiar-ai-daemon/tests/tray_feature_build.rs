//! PRD-092 f2: the previous version of this file had one test that accepted
//! *either* a successful `--features tray` build *or* the named
//! missing-dependency diagnostic as passing. In the tester image used by
//! `tests-green-crates` (no `libgtk-3-dev` / `libxdo-dev` installed), the
//! build always takes the diagnostic branch, so the success branch — and
//! with it every line in `familiar-ai-daemon/src/main.rs` gated on
//! `#[cfg(feature = "tray")]` — was never compiled by anything. Splitting
//! into two tests fixes that quietly-degenerate coverage:
//!
//! - `tray_feature_build_names_missing_dependency_when_libxdo_absent` forces
//!   the missing-dependency branch deterministically (via
//!   `familiar_ai_tray::sysdeps::SEARCH_DIRS_OVERRIDE_ENV`, rather than
//!   relying on the ambient host happening to lack the library) and asserts
//!   the named diagnostic. This one always runs.
//! - `tray_feature_build_succeeds_with_system_dependencies_present` asserts
//!   build *success* unconditionally — it does not fall back to accepting
//!   the diagnostic — so it fails loudly rather than silently passing if run
//!   on a host without the tray system dependencies. The tester image now
//!   installs `libgtk-3-dev`/`libxdo-dev` (see the Dockerfile `tester`
//!   stage), so this runs unconditionally and gives real executed coverage
//!   of the `#[cfg(feature = "tray")]` code in `main.rs`.
//!
//! Both build invocations use an isolated `CARGO_TARGET_DIR` so cargo's
//! build-script fingerprint cache for one test can never be shared with the
//! other test or with a previous build in this crate's normal `target/`.

use std::path::Path;
use std::process::Command;

// Mirrors `familiar_ai_tray::sysdeps::SEARCH_DIRS_OVERRIDE_ENV`. Kept as a
// literal rather than a dependency on `familiar-ai-tray`: that crate is only
// ever meant to be compiled in as an optional dependency behind the `tray`
// feature this test builds out-of-process — an unconditional dev-dependency
// here would force it (and its `gtk`/`libxdo` native deps) into every
// `cargo test -p familiar-ai-daemon` run, including the required
// `tests-green-crates` check, which builds `--no-default-features`.
const SEARCH_DIRS_OVERRIDE_ENV: &str = "FAMILIAR_AI_TRAY_XDO_SEARCH_DIRS_OVERRIDE";

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn tray_feature_build_command(target_dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(workspace_root())
        .env("CARGO_TARGET_DIR", target_dir)
        .args([
            "build",
            "-p",
            "familiar-ai-daemon",
            "--bin",
            "familiar-ai-daemon",
            "--features",
            "tray",
        ]);
    cmd
}

#[test]
#[cfg(target_os = "linux")]
fn tray_feature_build_names_missing_dependency_when_libxdo_absent() {
    let empty_dir = tempfile::tempdir().expect("create empty search-dir override");
    let target_dir = tempfile::tempdir().expect("create isolated CARGO_TARGET_DIR for this build");

    let output = tray_feature_build_command(target_dir.path())
        .env(SEARCH_DIRS_OVERRIDE_ENV, empty_dir.path())
        .output()
        .expect("failed to invoke cargo for the tray-feature build");

    assert!(
        !output.status.success(),
        "tray-feature build unexpectedly succeeded with an empty forced search-dir override"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("requires the system library") && stderr.contains("libxdo-dev"),
        "tray-feature build failed for a reason other than the named missing-dependency \
         diagnostic — stderr:\n{stderr}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn tray_feature_build_succeeds_with_system_dependencies_present() {
    let target_dir = tempfile::tempdir().expect("create isolated CARGO_TARGET_DIR for this build");

    let output = tray_feature_build_command(target_dir.path())
        .output()
        .expect("failed to invoke cargo for the tray-feature build");

    assert!(
        output.status.success(),
        "tray-feature build failed — this test requires libgtk-3-dev and libxdo-dev on the \
         host (installed in the Dockerfile tester stage). stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
