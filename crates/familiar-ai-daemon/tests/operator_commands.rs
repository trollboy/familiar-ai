//! PRD-084: the operator repairs are shipped commands, not `examples/`.
//!
//! Each writes durable orchestration state or consults the scheduler, so each
//! demands an explicit human actor and a reason, and each refuses while the
//! control-plane claim is live (FAM-BUG-048). These regressions pin both
//! properties per command, and pin that nothing left under `examples/` can
//! write durable state.

use std::fs;
use std::path::Path;
use std::process::Command;

use familiar_ai_core::{BacklogDiscovery, FilesystemBacklogDiscovery};
use tempfile::tempdir;

fn binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push("familiar-ai");
    path
}

/// Runs an operator subcommand in an isolated home so it cannot touch the
/// developer's real claim or database, and returns (stdout+stderr, success).
fn run(args: &[&str], runtime: &Path, claim: Option<&str>) -> (String, bool) {
    let runtime_dir = runtime.join("run");
    // AppPaths puts the claim under an application subdirectory of
    // XDG_RUNTIME_DIR, which is where the daemon writes it.
    fs::create_dir_all(runtime_dir.join("familiar-ai")).unwrap();
    if let Some(body) = claim {
        fs::write(
            runtime_dir.join("familiar-ai").join("control-plane.claim"),
            body,
        )
        .unwrap();
    }
    let output = Command::new(binary())
        .args(args)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_DATA_HOME", runtime.join("data"))
        .env("XDG_CONFIG_HOME", runtime.join("config"))
        .env("XDG_STATE_HOME", runtime.join("state"))
        .env("HOME", runtime.join("home"))
        .current_dir(runtime)
        .output()
        .expect("run familiar-ai");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (text, output.status.success())
}

/// A claim naming this very process, which `WorkerLock::inspect` therefore
/// reads as live — the FAM-BUG-048 condition.
///
/// Liveness is pid plus process start time, so the claim has to carry this
/// process's real start identity: a pid alone would be indistinguishable from
/// a recycled one.
fn live_claim() -> String {
    let start_identity = fs::read_to_string("/proc/self/stat")
        .expect("read /proc/self/stat")
        .split_whitespace()
        .nth(21)
        .expect("start time field")
        .to_owned();
    serde_json::json!({
        "installation_id": "test-installation",
        "owner_nonce": "test-nonce",
        "owner_pid": std::process::id(),
        "process_start_identity": start_identity,
        "boot_identity": null,
        "socket_path": "/tmp/does-not-matter.sock",
        "protocol_version": 1,
        "generation": 1,
    })
    .to_string()
}

const COMMANDS: [&[&str]; 3] = [
    &["operator", "rebind", "PRD-1"],
    &["operator", "set-phase", "cp-1", "implemented"],
    &["operator", "width"],
];

#[test]
fn every_operator_command_demands_a_named_human() {
    for base in COMMANDS {
        let temp = tempdir().unwrap();
        let mut args: Vec<&str> = base.to_vec();
        args.extend(["--actor", "someone", "--reason", "a reason"]);
        let (text, ok) = run(&args, temp.path(), None);
        assert!(!ok, "{base:?} accepted an actor that is not human:<identity>");
        assert!(
            text.contains("human:<identity>"),
            "{base:?} must say what an actor looks like, got: {text}"
        );
    }
}

#[test]
fn every_operator_command_demands_a_reason() {
    for base in COMMANDS {
        let temp = tempdir().unwrap();
        let mut args: Vec<&str> = base.to_vec();
        args.extend(["--actor", "human:tester", "--reason", "   "]);
        let (text, ok) = run(&args, temp.path(), None);
        assert!(!ok, "{base:?} accepted a blank reason");
        assert!(
            text.contains("--reason must not be empty"),
            "{base:?} got: {text}"
        );
    }
}

/// FAM-BUG-048: these write the same rows the drive writes, so running one
/// under a live session risks mutating a candidate the driver is working.
#[test]
fn every_operator_command_refuses_while_the_claim_is_live() {
    for base in COMMANDS {
        let temp = tempdir().unwrap();
        let mut args: Vec<&str> = base.to_vec();
        args.extend(["--actor", "human:tester", "--reason", "a reason"]);
        let (text, ok) = run(&args, temp.path(), Some(&live_claim()));
        assert!(!ok, "{base:?} ran while the control-plane claim was live");
        assert!(
            text.contains("is live"),
            "{base:?} must name the live owner, got: {text}"
        );
    }
}

/// The authority check runs before the refusal, so an operator who typed the
/// wrong actor is told that rather than being told to stop the daemon first.
#[test]
fn authority_is_checked_before_the_claim() {
    let temp = tempdir().unwrap();
    let (text, ok) = run(
        &["operator", "width", "--actor", "nope", "--reason", "r"],
        temp.path(),
        Some(&live_claim()),
    );
    assert!(!ok);
    assert!(text.contains("human:<identity>"), "got: {text}");
}

/// PRD-084 deletes the example scripts rather than leaving a second, unaudited
/// way to write the same rows. Anything left there must be read-only.
#[test]
fn no_example_writes_durable_state() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    if !examples.exists() {
        return;
    }
    let writers = [
        "record_rebind(",
        ".put(",
        ".transition(",
        "decide_scope(",
        "INSERT INTO",
        "UPDATE ",
    ];
    for entry in fs::read_dir(&examples).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let body = fs::read_to_string(&path).unwrap();
        for marker in writers {
            assert!(
                !body.contains(marker),
                "{} reaches a write API ({marker}); operator repairs belong in \
                 `familiar-ai operator`, where they are audited and refused \
                 under a live claim",
                path.display()
            );
        }
    }
}

/// PRD-084 AC5: an authored width has disagreed with the drive's own
/// admission before (FAM-BUG-010). The command must ask the scheduler, not
/// reimplement it, so both must answer identically for one backlog.
#[test]
fn the_width_command_and_the_drive_agree_on_one_backlog() {
    let repo = tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
    // Width is computed from scope disjointness, so each PRD needs a real
    // Expected Files contract. Two touch the same file and therefore cannot
    // run together; the third is disjoint from both.
    for (n, file) in [(1, "crates/a/src/one.rs"), (2, "crates/a/src/one.rs"), (3, "crates/b/src/three.rs")] {
        fs::write(
            repo.path().join(format!("docs/prds/PRD-{n}.md")),
            format!("# PRD-{n}: Fixture {n}\n\n## Expected Files\n\n- `{file}`\n"),
        )
        .unwrap();
    }

    let identity = FilesystemBacklogDiscovery.resolve(repo.path()).unwrap();
    let discovered = FilesystemBacklogDiscovery.discover(&identity).unwrap();
    let expected = familiar_ai_daemon::drive::achievable_width(&identity.worktree, &discovered)
        .expect("the drive computes a width for this backlog");

    let (text, ok) = run(
        &[
            "operator",
            "width",
            "--actor",
            "human:tester",
            "--reason",
            "comparing against the drive",
        ],
        repo.path(),
        None,
    );
    assert!(ok, "width should run with no claim held: {text}");
    assert!(
        text.contains(&format!(
            "graph_width={} achievable_width={}",
            expected.graph_width, expected.achievable_width
        )),
        "the command must report the scheduler's own numbers ({}, {}), got: {text}",
        expected.graph_width,
        expected.achievable_width
    );
}
