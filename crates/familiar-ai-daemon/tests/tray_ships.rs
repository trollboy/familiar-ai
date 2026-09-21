//! The tray is not optional and not legacy.
//!
//! It shipped in PRD-004 and was then dark for months while three defects
//! stacked up behind each other — a stale `daemon_run` call, a SIGTERM path
//! that left the process alive, an icon assertion pinned to a size the asset
//! had outgrown. The mechanism was not that anyone decided to stop shipping
//! it; it was that `crates/familiar-ai-tray` sat in the workspace `exclude`
//! list from the initial commit with no recorded reason, while being a
//! default-on dependency of the daemon. So it always compiled, and no
//! `--workspace` command ever tested, linted or formatted it. Nothing was
//! asking its 66 tests whether they still passed.
//!
//! That exclusion was removed on 2026-09-17. These assertions exist so it
//! cannot come back by accident, and so "the tray stopped shipping" has to be
//! a deliberate, reviewed change rather than a line nobody noticed.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must exist: {e}", path.display()))
}

/// Strip `#` comments so prose explaining the history cannot satisfy or break
/// an assertion about the directives.
fn directives(body: &str) -> String {
    body.lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_tray_is_a_workspace_member_and_is_not_excluded() {
    let manifest = directives(&read("Cargo.toml"));

    // Check the members array specifically. Asserting the path appears
    // anywhere in the file would be satisfied by an `exclude` entry too,
    // which is the exact regression this test exists to catch.
    let members = manifest
        .split_once("members")
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(list, _)| list.to_string())
        .expect("the workspace manifest must declare a members array");
    assert!(
        members.contains("crates/familiar-ai-tray"),
        "familiar-ai-tray must be a workspace member, or nothing tests, lints \
         or formats it. members = {members}"
    );

    // The exclude list is where it spent its whole life until 2026-09-17.
    let excluded = manifest
        .split_once("exclude")
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(list, _)| list.to_string())
        .unwrap_or_default();
    assert!(
        !excluded.contains("familiar-ai-tray"),
        "familiar-ai-tray must never return to the workspace exclude list: {excluded}"
    );
}

#[test]
fn the_tray_is_built_by_default_rather_than_behind_an_opt_in() {
    let manifest = directives(&read("crates/familiar-ai-daemon/Cargo.toml"));

    assert!(
        manifest.contains("default = [\"tray\"]"),
        "the tray must stay in the daemon's default feature set — the owner's \
         directive is that a missing systray icon is a failure case, not a \
         deferred feature, so putting it further behind a flag is not a fix"
    );
    assert!(
        manifest.contains("familiar-ai-tray = { path = \"../familiar-ai-tray\""),
        "the daemon must still depend on the tray crate"
    );
}

#[test]
fn the_tray_does_not_carry_its_own_lockfile_again() {
    // A per-crate Cargo.lock is what a crate outside the workspace has. Its
    // reappearance means the crate has drifted back out, and it is also how a
    // nested 1.4GB target/ directory got into a review diff once already.
    assert!(
        !repo_root()
            .join("crates/familiar-ai-tray/Cargo.lock")
            .exists(),
        "familiar-ai-tray must share the workspace lockfile, not carry its own"
    );
}

#[test]
fn gtk_is_a_linux_only_direct_dependency() {
    let manifest = directives(&read("crates/familiar-ai-tray/Cargo.toml"));
    let (common, linux) = manifest
        .split_once("[target.'cfg(target_os = \"linux\")'.dependencies]")
        .expect("the tray manifest must have an explicit Linux dependency section");

    assert!(
        common.contains("muda = { version = \"0.17\", default-features = false }"),
        "the cross-platform menu dependency must not enable GTK globally"
    );
    assert!(
        !common
            .lines()
            .any(|line| line.trim_start().starts_with("gtk =")),
        "GTK must not be a common dependency"
    );
    assert!(linux
        .lines()
        .any(|line| line.trim_start().starts_with("gtk =")));
    assert!(linux
        .contains("muda = { version = \"0.17\", default-features = false, features = [\"gtk\"] }"));
}

#[test]
fn the_trays_tests_are_reachable_from_a_workspace_run() {
    // The regression that matters most is not any single assertion in the tray
    // — it is that the tray has assertions at all and that a workspace command
    // reaches them. 66 of them existed for months without ever being run.
    let sources = [
        "crates/familiar-ai-tray/src/view.rs",
        "crates/familiar-ai-tray/src/menu.rs",
        "crates/familiar-ai-tray/src/data.rs",
    ];
    let total: usize = sources
        .iter()
        .map(|path| read(path).matches("#[test]").count())
        .sum();
    assert!(
        total >= 30,
        "the tray's own test coverage has collapsed to {total} tests in its core \
         modules, which is how it went dark the first time"
    );
}
