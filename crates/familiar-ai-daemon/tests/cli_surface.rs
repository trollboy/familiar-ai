//! PRD-090: the CLI surface design pass.
//!
//! A small set of daily verbs at the top, everything else reachable under a
//! declared administrative namespace, a no-argument front door that answers
//! "what do I do next", every relocated command still working under its
//! previous name, and a help text that names the next step in the workflow
//! at every level. These regressions pin all five.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use familiar_ai_daemon::cli::shared::{
    relocation_notice, RELOCATED_COMMAND_ALIASES, TOP_LEVEL_COMMANDS, TOP_LEVEL_COMMAND_CEILING,
};
use familiar_ai_storage::{
    CheckpointRepository, Database, DriverRepository, ExecutionCheckpoint, OrchestrationRepository,
};
use tempfile::tempdir;

fn git_repo() -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap();
    assert!(status.success());
    fs::create_dir_all(temp.path().join("docs/prds")).unwrap();
    temp
}

fn command(repo: &Path, database: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_familiar-ai"));
    command
        .current_dir(repo)
        .args(args)
        .env("FAMILIAR_AI_DATABASE__PATH", database)
        .env(
            "XDG_CONFIG_HOME",
            std::env::temp_dir().join("familiar-ai-tests-no-config"),
        )
        .env(
            "XDG_RUNTIME_DIR",
            database.parent().unwrap().join("xdg-runtime"),
        );
    command
}

fn run(repo: &Path, database: &Path, args: &[&str]) -> Output {
    command(repo, database, args).output().unwrap()
}

fn text(output: &Output) -> String {
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    combined
}

/// Stdout alone, excluding stderr -- needed wherever stderr would legitimately
/// differ between two invocations being compared (the relocation notice, for
/// instance, which only ever fires for the old name).
fn stdout_only(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every line under a `--help` listing's `Commands:` header, up to the next
/// blank line -- clap always renders one subcommand per line, name first.
fn listed_commands(help_text: &str) -> Vec<String> {
    help_text
        .lines()
        .skip_while(|line| *line != "Commands:")
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// The body of a named `--help` section (`Commands:`, `Arguments:`,
/// `Options:`), the same shape as [`listed_commands`] but for any header --
/// used to compare the argument surface of a leaf command rather than its
/// subcommand listing.
fn section_lines(help_text: &str, header: &str) -> Vec<String> {
    help_text
        .lines()
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn assert_lists_leaves(repo: &Path, database: &Path, path: &[&str], leaves: &[&str]) {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    let output = run(repo, database, &args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let listed = listed_commands(&stdout);
    for leaf in leaves {
        assert!(
            listed.iter().any(|name| name == leaf),
            "expected {path:?} --help to list '{leaf}'; got {listed:?}\n{stdout}"
        );
    }
}

// ---------------------------------------------------------------------
// AC2: top-level commands are limited to the daily verbs and a small
// declared set of administrative namespaces, checked against a ceiling.
// ---------------------------------------------------------------------

#[test]
fn declared_top_level_commands_stay_under_their_ceiling() {
    assert!(TOP_LEVEL_COMMANDS.len() <= TOP_LEVEL_COMMAND_CEILING);
}

#[test]
fn bare_help_lists_exactly_the_declared_top_level_commands() {
    let repo = git_repo();
    let database = repo.path().join("state/help.db");
    let output = run(repo.path(), &database, &["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut listed: Vec<String> = listed_commands(&stdout)
        .into_iter()
        .filter(|name| name != "help")
        .collect();
    listed.sort();
    let mut declared: Vec<String> = TOP_LEVEL_COMMANDS.iter().map(|s| s.to_string()).collect();
    declared.sort();
    assert_eq!(listed, declared, "{stdout}");

    // None of the sixteen relocated names may appear at the top level.
    for (old, _) in RELOCATED_COMMAND_ALIASES {
        assert!(
            !listed.iter().any(|name| name == old),
            "relocated command '{old}' must not be listed at the top level"
        );
    }
}

// ---------------------------------------------------------------------
// AC3: every relocated command keeps its previous invocation working as a
// recorded alias that succeeds and prints the new form.
// ---------------------------------------------------------------------

#[test]
fn every_relocated_command_alias_succeeds_and_names_its_new_form() {
    let repo = git_repo();
    let database = repo.path().join("state/aliases.db");
    for (old, new) in RELOCATED_COMMAND_ALIASES {
        // `--help` is side-effect free for every one of these, so this
        // exercises the real alias path rather than a fixture stand-in.
        let output = run(repo.path(), &database, &[old, "--help"]);
        assert!(
            output.status.success(),
            "familiar-ai {old} --help must succeed: {}",
            text(&output)
        );
        let expected = relocation_notice(old).expect("declared alias");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&expected),
            "familiar-ai {old} --help must note it is now '{new}': {stderr}"
        );
    }
}

#[test]
fn relocation_notice_fires_even_for_a_bare_invocation_not_just_help() {
    let repo = git_repo();
    let database = repo.path().join("state/aliases-bare.db");
    // `status` and `preflight` are read-only against an empty repository, so
    // they can run all the way through rather than stopping at `--help`.
    let output = run(repo.path(), &database, &["preflight"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'familiar-ai preflight' is now 'familiar-ai stewardship preflight'"));
}

// ---------------------------------------------------------------------
// PRD-103 f3-alias-regression-does-not-assert-target: the notice above
// proves only that the old name is declared in `RELOCATED_COMMAND_ALIASES`,
// printed from raw argv before clap ever parses anything. It does not prove
// the old name still reaches the same handler as its replacement -- an
// alias wired to the wrong handler, or whose copy of the arguments has
// drifted from the namespaced original, would still pass it. This compares
// the actual subcommand/argument surface `--help` renders for each side.
// ---------------------------------------------------------------------

#[test]
fn every_relocated_alias_resolves_to_the_identical_subcommand_surface_as_its_target() {
    let repo = git_repo();
    let database = repo.path().join("state/alias-targets.db");
    for (old, new) in RELOCATED_COMMAND_ALIASES {
        let old_output = run(repo.path(), &database, &[old, "--help"]);
        assert!(
            old_output.status.success(),
            "familiar-ai {old} --help must succeed: {}",
            text(&old_output)
        );
        let old_help = stdout_only(&old_output);

        let mut new_args: Vec<&str> = new.split(' ').collect();
        new_args.push("--help");
        let new_output = run(repo.path(), &database, &new_args);
        assert!(
            new_output.status.success(),
            "familiar-ai {new} --help must succeed: {}",
            text(&new_output)
        );
        let new_help = stdout_only(&new_output);

        assert_eq!(
            listed_commands(&old_help),
            listed_commands(&new_help),
            "familiar-ai {old} must list the identical subcommands as familiar-ai {new}"
        );
        for header in ["Arguments:", "Options:"] {
            assert_eq!(
                section_lines(&old_help, header),
                section_lines(&new_help, header),
                "familiar-ai {old} --help's {header} section must match familiar-ai {new} --help's -- \
                 an alias wired to the wrong handler, or a copy of its arguments that has drifted from \
                 the namespaced original, would show up here"
            );
        }
    }
}

// ---------------------------------------------------------------------
// AC5: no capability is removed -- every relocated group's leaves are
// reachable at their new namespaced home (and, via the alias test above,
// at their old name too).
// ---------------------------------------------------------------------

#[test]
fn every_relocated_leaf_is_reachable_at_its_new_namespaced_home() {
    let repo = git_repo();
    let database = repo.path().join("state/leaves.db");
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["config"],
            &[
                "provider",
                "model",
                "artifact",
                "migrate",
                "history",
                "project",
                "show",
                "compress",
                "model-residency",
            ],
        ),
        (
            &["config", "compress"],
            &["output-enable", "input-enable", "experiment"],
        ),
        (
            &["config", "model-residency"],
            &["enable", "disable", "status"],
        ),
        (
            &["accounting"],
            &["month-to-date", "prd-cost", "billing", "usage"],
        ),
        (
            &["accounting", "billing"],
            &["status", "collect", "reconcile"],
        ),
        (
            &["stewardship"],
            &[
                "substance",
                "backlog",
                "sessions",
                "attempts",
                "checkpoints",
                "recovery",
                "delivery",
                "budget",
                "review",
                "gates",
                "reconciliation",
                "workers",
                "status",
                "preflight",
                "history",
            ],
        ),
        (
            &["plan"],
            &["approve", "reject", "onboard", "backlog", "batch-review"],
        ),
        (
            &["plan", "onboard"],
            &["propose", "approve", "validate", "fixture"],
        ),
        (
            &["plan", "backlog"],
            &[
                "metadata-check",
                "bootstrap",
                "release",
                "complete",
                "record-complete",
                "approve-and-complete",
            ],
        ),
        (&["plan", "backlog", "bootstrap"], &["status", "rollback"]),
        (&["plan", "batch-review"], &["enable", "disable", "pending"]),
        (
            &["ops"],
            &["control", "worker", "operator", "gate", "waive"],
        ),
        (
            &["ops", "control"],
            &[
                "register",
                "submit",
                "attach",
                "show",
                "project-state",
                "cancel",
            ],
        ),
        (
            &["ops", "worker"],
            &[
                "install",
                "uninstall",
                "status",
                "validate",
                "test",
                "plist",
                "run",
            ],
        ),
        (&["ops", "operator"], &["rebind", "set-phase", "width"]),
        (&["ops", "gate"], &["run", "status", "require", "override"]),
    ];
    for (path, leaves) in cases {
        assert_lists_leaves(repo.path(), &database, path, leaves);
    }
}

// ---------------------------------------------------------------------
// PRD-103 f2-ac5-leaf-set-not-compared: the spot check above never
// enumerates the full reachable leaf set -- it recurses only as deep as the
// levels it names, and `any(|name| name == leaf)` makes an extra or missing
// unlisted leaf invisible. `ops desktop` is exactly such a leaf: it is a
// real group under `OpsCommand` with three leaves of its own, and the case
// list above never mentions it, so it was never checked. This regression
// walks the whole command tree recursively from `--help` and compares the
// complete set of reachable leaf paths against a checked-in inventory, so a
// dropped capability or an unlisted group fails with a diff instead of
// passing unseen.
// ---------------------------------------------------------------------

/// Every leaf command reachable from `familiar-ai --help`, as a full
/// space-separated path (e.g. `"ops desktop status"`). Recomputed by hand
/// against the `Command`/`*NamespaceCommand`/`*Command` enum definitions in
/// `src/bin/familiar-ai.rs` and `src/cli/*.rs` -- this is the "declared
/// inventory" the walk below is checked against, so growing the tree is a
/// deliberate edit here, not a silent pass.
const DECLARED_LEAVES: &[&str] = &[
    // -- Daily verbs ------------------------------------------------------
    "next",
    "run",
    "drive",
    "resume",
    "report",
    "approve",
    "deliver",
    // -- config -------------------------------------------------------
    "config history",
    "config show",
    "config provider add",
    "config provider remove",
    "config provider verify",
    "config provider list",
    "config provider bind",
    "config model enable",
    "config model disable",
    "config model list",
    "config artifact register",
    "config artifact register-alias",
    "config artifact list",
    "config artifact show",
    "config migrate agents",
    "config project approve",
    "config project revoke",
    "config compress output-enable",
    "config compress input-enable",
    "config compress experiment",
    "config model-residency enable",
    "config model-residency disable",
    "config model-residency status",
    // -- accounting ---------------------------------------------------
    "accounting month-to-date",
    "accounting prd-cost",
    "accounting usage",
    "accounting billing status",
    "accounting billing collect",
    "accounting billing reconcile",
    // -- stewardship ----------------------------------------------------
    "stewardship substance",
    "stewardship backlog",
    "stewardship sessions",
    "stewardship attempts",
    "stewardship checkpoints",
    "stewardship recovery",
    "stewardship delivery",
    "stewardship budget",
    "stewardship review",
    "stewardship gates",
    "stewardship reconciliation",
    "stewardship workers",
    "stewardship status",
    "stewardship preflight",
    "stewardship history",
    // -- plan -----------------------------------------------------------
    "plan approve",
    "plan reject",
    "plan onboard propose",
    "plan onboard approve",
    "plan onboard validate",
    "plan onboard fixture",
    "plan backlog metadata-check",
    "plan backlog bootstrap status",
    "plan backlog bootstrap rollback",
    "plan backlog release",
    "plan backlog complete",
    "plan backlog record-complete",
    "plan backlog approve-and-complete",
    "plan batch-review enable",
    "plan batch-review disable",
    "plan batch-review pending",
    // -- ops --------------------------------------------------------------
    "ops waive",
    "ops control register",
    "ops control submit",
    "ops control attach",
    "ops control show",
    "ops control project-state",
    "ops control cancel",
    "ops worker install",
    "ops worker uninstall",
    "ops worker status",
    "ops worker validate",
    "ops worker test",
    "ops worker plist",
    "ops worker run",
    "ops desktop install",
    "ops desktop uninstall",
    "ops desktop status",
    "ops operator rebind",
    "ops operator set-phase",
    "ops operator width",
    "ops gate run",
    "ops gate status",
    "ops gate require",
    "ops gate override",
];

/// Recursively walks `familiar-ai <path> --help`, following every listed
/// subcommand (other than clap's own `help`), and returns every leaf path
/// reached -- a level with no `Commands:` section of its own is a leaf.
fn collect_leaves(repo: &Path, database: &Path, path: &[&str]) -> Vec<String> {
    let mut args: Vec<&str> = path.to_vec();
    args.push("--help");
    let output = run(repo, database, &args);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let children: Vec<String> = listed_commands(&stdout)
        .into_iter()
        .filter(|name| name != "help")
        .collect();
    if children.is_empty() {
        return vec![path.join(" ")];
    }
    let mut leaves = Vec::new();
    for child in children {
        let mut child_path: Vec<&str> = path.to_vec();
        child_path.push(&child);
        leaves.extend(collect_leaves(repo, database, &child_path));
    }
    leaves
}

/// `Ok(())` when both sets are identical; otherwise an error naming exactly
/// what is missing from the reachable tree and what is unexpectedly present
/// in it, so a real regression fails with a diff rather than a bare
/// "assertion failed".
fn leaf_paths_match(actual: &BTreeSet<String>, declared: &BTreeSet<String>) -> Result<(), String> {
    if actual == declared {
        return Ok(());
    }
    let missing: Vec<_> = declared.difference(actual).cloned().collect();
    let unexpected: Vec<_> = actual.difference(declared).cloned().collect();
    Err(format!(
        "leaf set mismatch -- declared but unreachable: {missing:?}; reachable but undeclared: {unexpected:?}"
    ))
}

#[test]
fn full_reachable_leaf_set_matches_the_declared_inventory() {
    let repo = git_repo();
    let database = repo.path().join("state/leaf-inventory.db");
    let actual: BTreeSet<String> = collect_leaves(repo.path(), &database, &[])
        .into_iter()
        .collect();
    let declared: BTreeSet<String> = DECLARED_LEAVES.iter().map(|s| s.to_string()).collect();
    assert_eq!(leaf_paths_match(&actual, &declared), Ok(()));
}

/// Pins that the comparison above actually catches a dropped capability,
/// rather than being vacuously true: with one declared leaf removed from
/// the "reachable" side, the same comparison must report a mismatch.
#[test]
fn leaf_set_comparison_fails_when_a_leaf_is_missing_from_the_tree() {
    let declared: BTreeSet<String> = DECLARED_LEAVES.iter().map(|s| s.to_string()).collect();
    let mut actual = declared.clone();
    assert!(actual.remove("ops desktop status"));
    assert!(leaf_paths_match(&actual, &declared).is_err());
}

// ---------------------------------------------------------------------
// AC4: help at every level names the next command in the workflow, so the
// path from `next` to `run` to a pause to approval to completion is
// traversable from help text alone.
// ---------------------------------------------------------------------

#[test]
fn help_text_traces_the_whole_workflow_from_next_to_completion() {
    let repo = git_repo();
    let database = repo.path().join("state/chain.db");

    let top = text(&run(repo.path(), &database, &["--help"]));
    assert!(listed_commands(&top).iter().any(|name| name == "next"));

    let next_help = text(&run(repo.path(), &database, &["next", "--help"]));
    assert!(next_help.contains("familiar-ai run"), "{next_help}");

    let run_help = text(&run(repo.path(), &database, &["run", "--help"]));
    assert!(run_help.contains("familiar-ai approve"), "{run_help}");

    let approve_help = text(&run(repo.path(), &database, &["approve", "--help"]));
    assert!(
        approve_help.contains("familiar-ai resume"),
        "{approve_help}"
    );

    let resume_help = text(&run(repo.path(), &database, &["resume", "--help"]));
    assert!(resume_help.contains("familiar-ai deliver"), "{resume_help}");

    let deliver_help = text(&run(repo.path(), &database, &["deliver", "--help"]));
    assert!(
        deliver_help.to_lowercase().contains("complete"),
        "{deliver_help}"
    );
}

// ---------------------------------------------------------------------
// AC1: bare `familiar-ai` reports repository state and the single next
// runnable command, pinned per state.
// ---------------------------------------------------------------------

fn front_door(repo: &Path, database: &Path) -> Output {
    run(repo, database, &[])
}

/// Opens and migrates the database directly, exactly as every command's own
/// `database()` helper does, without depending on a prior CLI invocation
/// having touched it first (several commands, like `preflight`, never open
/// the database at all).
fn migrated_database(database: &Path) -> Database {
    fs::create_dir_all(database.parent().unwrap()).unwrap();
    let db = Database::open(database).unwrap();
    db.run_migrations().unwrap();
    db
}

fn repository_key(repo: &tempfile::TempDir) -> String {
    format!("{}/.git", repo.path().canonicalize().unwrap().display())
}

#[test]
fn front_door_reports_nothing_to_do_on_an_empty_backlog() {
    let repo = git_repo();
    let database = repo.path().join("state/nothing.db");
    let output = front_door(repo.path(), &database);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("state: nothing to do"), "{stdout}");
    assert!(stdout.contains("next: familiar-ai next"), "{stdout}");
}

// ---------------------------------------------------------------------
// PRD-103 f1-front-door-misreports-broken-backlog: `front_door_report`
// used to compute no eligible work whenever `validate_graph` failed, and
// swallow every selection error with `.ok()` -- both collapsed into
// `NothingToDo`, so a repository with a structurally broken backlog (here,
// a PRD depending on one that does not exist) was told "nothing to do", a
// false statement of state for exactly the case where an operator most
// needs to be told something is wrong.
// ---------------------------------------------------------------------

#[test]
fn front_door_reports_backlog_unschedulable_instead_of_nothing_to_do_when_the_graph_is_invalid() {
    let repo = git_repo();
    let database = repo.path().join("state/invalid-graph.db");
    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "# PRD-1: One\n\n**Depends on:** PRD-999\n",
    )
    .unwrap();

    let output = front_door(repo.path(), &database);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("state: nothing to do"),
        "a backlog with an unresolvable dependency is not \"nothing to do\": {stdout}"
    );
    assert!(stdout.contains("state: backlog unschedulable"), "{stdout}");
    assert!(stdout.contains("next: familiar-ai next"), "{stdout}");
    assert!(
        stdout.contains("PRD-999"),
        "the report should name the failure, not just gesture at it: {stdout}"
    );
}

#[test]
fn front_door_reports_work_eligible_and_names_the_run_command() {
    let repo = git_repo();
    let database = repo.path().join("state/eligible.db");
    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "# PRD-001: Sample\n",
    )
    .unwrap();
    let output = front_door(repo.path(), &database);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("state: work eligible"), "{stdout}");
    assert!(
        stdout.contains("next: familiar-ai run docs/prds/PRD-001.md"),
        "{stdout}"
    );
}

#[test]
fn front_door_reports_decision_pending_and_names_the_approve_command() {
    let repo = git_repo();
    let database = repo.path().join("state/decision.db");
    let db = migrated_database(&database);
    let key = repository_key(&repo);
    CheckpointRepository::new(db.conn())
        .put(&ExecutionCheckpoint {
            checkpoint_id: "cp-1".into(),
            repository_key: key.clone(),
            prd_id: "PRD-1".into(),
            prd_path: "docs/prds/PRD-001.md".into(),
            execution_id: Some("exec-1".into()),
            phase: "implemented_pending_review".into(),
            base_revision: "deadbeef".into(),
            worktree_path: "/tmp/does-not-matter".into(),
            branch_name: None,
            diff_hash: "sha256:candidate".into(),
            changed_files_json: "[]".into(),
            agent_identity: "claude-code".into(),
            usage_json: "{}".into(),
            test_evidence_json: "{}".into(),
            invalid_reason: None,
        })
        .unwrap();
    OrchestrationRepository::new(db.conn())
        .record_scope_finding(
            &key,
            "cp-1",
            "PRD-1",
            "sha256:candidate",
            "finding-hash-1",
            r#"{"finding_id":"f","change_id":"c","path":"p","old_path":null,"change_kind":"added","file_class":"ordinary_source","decision":"undeclared_scope_expansion","rule_id":"r","rule_source":"built_in","rule_detail":"d","expected_file_match":null,"allowed_path_match":null,"prohibited_rule_match":null,"policy_snapshot_hash":"h"}"#,
        )
        .unwrap();
    drop(db);

    let output = front_door(repo.path(), &database);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("state: decision pending"), "{stdout}");
    assert!(
        stdout.contains("next: familiar-ai approve --finding-hash finding-hash-1 --candidate-hash sha256:candidate --approve"),
        "{stdout}"
    );
}

#[test]
fn front_door_reports_session_stopped_and_names_the_report_command() {
    let repo = git_repo();
    let database = repo.path().join("state/stopped.db");
    let db = migrated_database(&database);
    let key = repository_key(&repo);
    let sessions = DriverRepository::new(db.conn());
    sessions.open_session("sess-1", &key, "{}").unwrap();
    sessions
        .finish_session("sess-1", "preflight_failed")
        .unwrap();
    drop(db);

    let output = front_door(repo.path(), &database);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("state: session stopped"), "{stdout}");
    assert!(
        stdout.contains("next: familiar-ai report sess-1"),
        "{stdout}"
    );
}

#[test]
fn front_door_prefers_a_pending_decision_over_eligible_work() {
    let repo = git_repo();
    let database = repo.path().join("state/priority.db");
    fs::write(
        repo.path().join("docs/prds/PRD-001.md"),
        "# PRD-001: Sample\n",
    )
    .unwrap();
    let _ = run(repo.path(), &database, &["next"]);

    let db = Database::open(&database).unwrap();
    let key = repository_key(&repo);
    CheckpointRepository::new(db.conn())
        .put(&ExecutionCheckpoint {
            checkpoint_id: "cp-1".into(),
            repository_key: key.clone(),
            prd_id: "PRD-1".into(),
            prd_path: "docs/prds/PRD-001.md".into(),
            execution_id: Some("exec-1".into()),
            phase: "implemented_pending_review".into(),
            base_revision: "deadbeef".into(),
            worktree_path: "/tmp/does-not-matter".into(),
            branch_name: None,
            diff_hash: "sha256:candidate".into(),
            changed_files_json: "[]".into(),
            agent_identity: "claude-code".into(),
            usage_json: "{}".into(),
            test_evidence_json: "{}".into(),
            invalid_reason: None,
        })
        .unwrap();
    OrchestrationRepository::new(db.conn())
        .record_scope_finding(
            &key,
            "cp-1",
            "PRD-1",
            "sha256:candidate",
            "finding-hash-1",
            r#"{"finding_id":"f","change_id":"c","path":"p","old_path":null,"change_kind":"added","file_class":"ordinary_source","decision":"undeclared_scope_expansion","rule_id":"r","rule_source":"built_in","rule_detail":"d","expected_file_match":null,"allowed_path_match":null,"prohibited_rule_match":null,"policy_snapshot_hash":"h"}"#,
        )
        .unwrap();
    drop(db);

    let output = front_door(repo.path(), &database);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("state: decision pending"), "{stdout}");
}
