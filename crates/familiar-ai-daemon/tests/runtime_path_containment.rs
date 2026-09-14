//! PRD-081 worktree containment for the raw agent runtime.
//!
//! `SandboxedToolExecutor::resolve_within_worktree` is the single chokepoint
//! standing between a model-supplied path and the host filesystem. Its check
//! is purely lexical — it rejects an empty path, an absolute path, and any
//! `..` component — and it never canonicalizes. A lexical check cannot see a
//! symlink, and the runtime offers `RunCommand`, so a model can create one
//! (`ln -s / escape`) and then reach straight through it with a path that is
//! neither absolute nor contains `..`.
//!
//! Both directions are affected. `read-file` returns the contents of any file
//! the daemon user can read, and `apply-edit` *writes* through the link — it
//! answers `Ok("wrote 12 bytes")` while modifying a file outside the worktree
//! entirely. The write side is the severe one: combined with `RunCommand`, a
//! worker can rewrite anything its uid owns.
//!
//! These tests are the oracle for "contained": they are written to fail
//! against the current implementation and to pass only once containment is
//! decided on the *resolved* path rather than the spelled one. Nothing here
//! makes a live or billable call.

use std::fs;
use std::os::unix::fs::symlink;

use familiar_ai_agent::raw_runtime::{
    AuthorityContext, CapabilityId, ExecutionError, ToolExecutor, ValidatedCall,
};
use familiar_ai_core::config::{AgentRuntimeSandboxConfig, TokenDisciplineConfig};
use familiar_ai_daemon::agent_runtime::SandboxedToolExecutor;

fn no_sandbox() -> AgentRuntimeSandboxConfig {
    AgentRuntimeSandboxConfig {
        allowed_commands: vec!["printf".into()],
        network_allowed: false,
        allowed_environment: vec![],
    }
}

fn authority() -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: "exec_1".into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_1".into(),
    }
}

fn call(capability: CapabilityId, call_id: &str, arguments: serde_json::Value) -> ValidatedCall {
    ValidatedCall {
        call_id: call_id.into(),
        capability,
        argument_hash: "hash".into(),
        arguments,
    }
}

fn enabled_executor(worktree_root: std::path::PathBuf) -> SandboxedToolExecutor {
    SandboxedToolExecutor::new(
        worktree_root,
        no_sandbox(),
        2_000,
        1 << 20,
        TokenDisciplineConfig {
            enabled: true,
            targeted_edit_threshold_bytes: 10,
            tool_result_max_lines: 10,
            tool_result_head_lines: 3,
            tool_result_tail_lines: 3,
            file_read_max_lines: 5,
        },
    )
    .unwrap()
}

/// The confirmed HIGH. `escape/secret.txt` is not absolute and holds no `..`
/// component, so the lexical guard admits it; `read_to_string` then follows
/// the symlink out of the worktree entirely.
#[test]
fn read_file_refuses_a_symlink_that_escapes_the_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP SECRET\n").unwrap();
    symlink(outside.path(), temp.path().join("escape")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::ReadFile,
            "c_symlink_read",
            serde_json::json!({ "path": "escape/secret.txt" }),
        ),
        &authority(),
    );

    match result {
        Err(ExecutionError::Failed(_)) => {}
        Ok(outcome) => panic!(
            "symlink escape must be refused, but read-file returned: {}",
            outcome.result_text
        ),
        Err(other) => panic!("expected ExecutionError::Failed, got {other:?}"),
    }
}

/// A symlink pointing at a single file outside the worktree, rather than at a
/// directory — the same escape without a traversable component to inspect.
#[test]
fn read_file_refuses_a_symlink_to_a_single_outside_file() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa");
    fs::write(&secret, "PRIVATE KEY\n").unwrap();
    symlink(&secret, temp.path().join("innocent.txt")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::ReadFile,
            "c_symlink_file",
            serde_json::json!({ "path": "innocent.txt" }),
        ),
        &authority(),
    );

    match result {
        Err(ExecutionError::Failed(_)) => {}
        Ok(outcome) => panic!(
            "symlink to an outside file must be refused, got: {}",
            outcome.result_text
        ),
        Err(other) => panic!("expected ExecutionError::Failed, got {other:?}"),
    }
}

/// The write side of the same hole: containment must not depend on which
/// capability reached the filesystem.
#[test]
fn apply_edit_refuses_to_write_through_an_escaping_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("victim.txt");
    fs::write(&target, "original\n").unwrap();
    symlink(&target, temp.path().join("victim.txt")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::ApplyEdit,
            "c_symlink_write",
            serde_json::json!({
                "path": "victim.txt",
                "change_kind": "whole-file",
                "content": "OVERWRITTEN\n",
            }),
        ),
        &authority(),
    );

    assert!(
        matches!(result, Err(ExecutionError::Failed(_))),
        "apply-edit through an escaping symlink must be refused, got {result:?}"
    );
    // The decisive assertion: refusal is worthless if the write already landed.
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "original\n",
        "a file outside the worktree was modified"
    );
}

/// `victim.txt` is a symlink whose target does not exist yet. The fallback
/// that resolves a not-yet-created leaf must not treat a dangling symlink
/// the same way: `canonicalize` fails with `NotFound` for both, but only a
/// component with no filesystem entry at all is the to-be-created-leaf case.
/// A symlink that already exists, dangling or not, is refused outright.
#[test]
fn apply_edit_refuses_a_dangling_symlink_leaf_pointing_outside() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("autostart.desktop");
    symlink(&target, temp.path().join("victim.txt")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::ApplyEdit,
            "c_dangling_symlink_write",
            serde_json::json!({
                "path": "victim.txt",
                "change_kind": "whole-file",
                "content": "OVERWRITTEN\n",
            }),
        ),
        &authority(),
    );

    assert!(
        matches!(result, Err(ExecutionError::Failed(_))),
        "apply-edit through a dangling symlink must be refused, got {result:?}"
    );
    assert!(
        !target.exists(),
        "a file outside the worktree was created through a dangling symlink"
    );
}

/// Same hole, one level up: the *intermediate* component is the dangling
/// symlink (`link -> /outside/dir-created-later`), not the leaf itself.
#[test]
fn apply_edit_refuses_a_dangling_symlink_intermediate_component() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let not_yet_created = outside.path().join("dir-created-later");
    symlink(&not_yet_created, temp.path().join("link")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::ApplyEdit,
            "c_dangling_symlink_intermediate",
            serde_json::json!({
                "path": "link/new.txt",
                "change_kind": "whole-file",
                "content": "OVERWRITTEN\n",
            }),
        ),
        &authority(),
    );

    assert!(
        matches!(result, Err(ExecutionError::Failed(_))),
        "apply-edit through a dangling symlink intermediate component must be refused, got {result:?}"
    );
    assert!(
        !not_yet_created.exists(),
        "a directory outside the worktree was created through a dangling symlink component"
    );
}

/// The full chain the PRD describes, with no symlink planted by the test
/// harness: the worker creates the link itself through `run-command` and then
/// reaches through it. This is the only test here where the link is made by
/// the same authority that later tries to use it, which is the actual threat
/// model — `ln -s` is reachable whenever it is allowlisted.
#[test]
fn a_worker_cannot_read_through_a_symlink_it_created_with_run_command() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "TOP SECRET\n").unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    executor.sandbox.allowed_commands.push("ln".into());

    let planted = executor
        .execute(
            &call(
                CapabilityId::RunCommand,
                "c_plant",
                serde_json::json!({
                    "argv": ["ln", "-s", outside.path().to_str().unwrap(), "escape"],
                }),
            ),
            &authority(),
        )
        .expect("creating a symlink is an allowed command, not the thing being refused");
    let _ = planted;
    assert!(
        temp.path().join("escape").symlink_metadata().is_ok(),
        "the worker's own run-command should have planted the link"
    );

    let result = executor.execute(
        &call(
            CapabilityId::ReadFile,
            "c_read_planted",
            serde_json::json!({ "path": "escape/secret.txt" }),
        ),
        &authority(),
    );
    match result {
        Err(ExecutionError::Failed(_)) => {}
        Ok(outcome) => panic!(
            "a worker read through a link it planted itself: {}",
            outcome.result_text
        ),
        Err(other) => panic!("expected ExecutionError::Failed, got {other:?}"),
    }

    // And the write direction of the same planted link.
    let result = executor.execute(
        &call(
            CapabilityId::ApplyEdit,
            "c_write_planted",
            serde_json::json!({
                "path": "escape/secret.txt",
                "change_kind": "whole-file",
                "content": "OVERWRITTEN\n",
            }),
        ),
        &authority(),
    );
    assert!(
        matches!(result, Err(ExecutionError::Failed(_))),
        "apply-edit through a self-planted link must be refused, got {result:?}"
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "TOP SECRET\n",
        "a file outside the worktree was modified through a self-planted link"
    );
}

/// Criterion 2 names `search-list` alongside read-file and apply-edit. Its
/// *argument* goes through the chokepoint, so `path: "escape"` is refused —
/// but the recursive walk behind it is a second way to reach the filesystem,
/// and `is_dir()` follows symlinks. Containment has to survive the walk, not
/// only the argument.
#[test]
fn search_list_refuses_an_escaping_subpath_argument() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("id_rsa"), "PRIVATE KEY\n").unwrap();
    symlink(outside.path(), temp.path().join("escape")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::SearchList,
            "c_search_arg",
            serde_json::json!({ "query": "", "path": "escape" }),
        ),
        &authority(),
    );

    assert!(
        matches!(result, Err(ExecutionError::Failed(_))),
        "search-list must refuse an escaping subpath exactly as read-file does, got {result:?}"
    );
}

/// The walk itself. `search-list` with no `path` starts at the worktree root,
/// which is contained — and then descends through `escape` into another
/// directory entirely, handing the model the names of files it can never
/// legitimately reach. Filenames are not file contents, but an SSH key's
/// existence and location is exactly the reconnaissance containment exists to
/// deny.
#[test]
fn search_list_does_not_enumerate_through_an_escaping_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("id_rsa"), "PRIVATE KEY\n").unwrap();
    fs::create_dir(outside.path().join("nested")).unwrap();
    fs::write(outside.path().join("nested/token.txt"), "tok\n").unwrap();
    symlink(outside.path(), temp.path().join("escape")).unwrap();
    fs::write(temp.path().join("inside.txt"), "ok\n").unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let outcome = executor
        .execute(
            &call(
                CapabilityId::SearchList,
                "c_search_walk",
                serde_json::json!({ "query": "" }),
            ),
            &authority(),
        )
        .expect("a search rooted at the worktree root is itself legitimate");

    assert!(
        outcome.result_text.contains("inside.txt"),
        "the contained tree must still be listed: {}",
        outcome.result_text
    );
    assert!(
        !outcome.result_text.contains("id_rsa"),
        "search-list enumerated a file outside the worktree: {}",
        outcome.result_text
    );
    assert!(
        !outcome.result_text.contains("token.txt"),
        "search-list recursed outside the worktree: {}",
        outcome.result_text
    );
}

/// Containment must not be mistaken for refusing every link. A symlink whose
/// target is inside the worktree is usable, so it stays listed; what it must
/// not do is turn the walk into an unbounded one.
#[test]
fn search_list_lists_a_contained_symlink_without_looping_on_a_cycle() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("real.txt"), "ok\n").unwrap();
    symlink(temp.path().join("real.txt"), temp.path().join("link.txt")).unwrap();
    // `loop -> .` is contained, and traversing it would never terminate for a
    // query that matches nothing.
    symlink(temp.path(), temp.path().join("loop")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let outcome = executor
        .execute(
            &call(
                CapabilityId::SearchList,
                "c_search_cycle",
                serde_json::json!({ "query": "zzz-matches-nothing" }),
            ),
            &authority(),
        )
        .expect("a cycle inside the worktree must not hang or crash the walk");
    assert_eq!(outcome.result_text, "");

    let outcome = executor
        .execute(
            &call(
                CapabilityId::SearchList,
                "c_search_contained_link",
                serde_json::json!({ "query": "link.txt" }),
            ),
            &authority(),
        )
        .unwrap();
    assert!(
        outcome.result_text.contains("link.txt"),
        "a symlink resolving inside the worktree stays visible: {}",
        outcome.result_text
    );
}

/// Criterion 2 also names `run-command`'s working directory. Nothing runs
/// here: the refusal must land before the process is spawned.
#[test]
fn run_command_refuses_an_escaping_working_directory() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), temp.path().join("escape")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::RunCommand,
            "c_cwd",
            serde_json::json!({
                "argv": ["printf", "hello"],
                "working_directory": "escape",
            }),
        ),
        &authority(),
    );

    assert!(
        matches!(result, Err(ExecutionError::Failed(_))),
        "run-command must refuse an escaping working directory, got {result:?}"
    );
}

/// The lexical guard closes plain parent traversal today. Containment is
/// about to be rewritten around canonicalization, so pin the existing
/// behaviour to keep the rewrite from reopening it.
#[test]
fn read_file_still_refuses_parent_traversal_and_absolute_paths() {
    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());

    for path in ["../outside.txt", "a/../../outside.txt", "/etc/passwd", ""] {
        let result = executor.execute(
            &call(
                CapabilityId::ReadFile,
                "c_lexical",
                serde_json::json!({ "path": path }),
            ),
            &authority(),
        );
        assert!(
            matches!(result, Err(ExecutionError::Failed(_))),
            "path {path:?} must be refused, got {result:?}"
        );
    }
}

/// PRD-082 defect 4: the executor must hold exactly one form of its root.
/// A configured root that traverses a symlink shares no textual prefix with
/// the resolved paths the capabilities actually open, so any second copy of
/// the root — the stored spelling — disagrees with the one containment is
/// decided against. Two spellings of the same worktree must therefore be
/// indistinguishable to the executor in every root-derived value.
///
/// This is the case the PRD notes does not reproduce under a bare `tempfile`
/// root on Linux (it canonicalizes to itself) and does on macOS, where
/// `TMPDIR` resolves through `/private/var`. Planting the symlink explicitly
/// reproduces it on either.
#[test]
fn a_symlinked_root_spelling_is_indistinguishable_from_its_canonical_one() {
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("inside.txt"), "ok\n").unwrap();
    // `link/` and `real/` are the same directory spelled two ways.
    symlink(&real, temp.path().join("link")).unwrap();

    let canonical = real.canonicalize().unwrap();
    let via_link = enabled_executor(temp.path().join("link"));
    let via_real = enabled_executor(real.clone());

    assert_eq!(
        via_link.worktree_root(),
        canonical,
        "the root was stored in its configured form rather than canonicalized once"
    );
    assert_eq!(via_link.worktree_root(), via_real.worktree_root());
    assert_eq!(
        via_link.tool_output_retention_dir(&authority().execution_id),
        via_real.tool_output_retention_dir(&authority().execution_id),
        "one worktree spelled two ways keyed two retention stores"
    );

    // And the root a path is *made relative to* is that same single form:
    // stripping a resolved path against the configured spelling would fail
    // and leak absolute host paths into an ordinary in-worktree search.
    let mut via_link = via_link;
    let outcome = via_link
        .execute(
            &call(
                CapabilityId::SearchList,
                "c_symlinked_root",
                serde_json::json!({ "query": "inside" }),
            ),
            &authority(),
        )
        .unwrap();
    assert_eq!(
        outcome.result_text, "inside.txt",
        "search-list reported a path measured against a different root form"
    );
}

/// Retention backs the paging handle a bounded result advertises. If the
/// retention write fails, the handle must not be emitted — otherwise the
/// model is handed a pointer to a file that was never created, which is
/// precisely the silent narrowing the runtime claims cannot happen.
///
/// The handle string *looks* like `.familiar/tool-output/<id>.txt`, but it
/// is opaque: the bytes live in daemon-owned storage outside the worktree
/// (`tool_output_retention_dir`), so occupying that name inside the worktree
/// would not make retention fail at all. Retention is blocked where it
/// actually happens — a regular file sitting on the retention directory's
/// own path, which no `create_dir_all` can turn into a directory.
#[test]
fn a_failed_retention_write_never_advertises_an_unresolvable_handle() {
    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());
    let blocker = executor.tool_output_retention_dir(&authority().execution_id);
    fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    let _ = fs::remove_dir_all(&blocker);
    fs::write(&blocker, "blocker").unwrap();
    let script: String = (1..=20).map(|n| format!("line{n}\\n")).collect();
    let outcome = executor.execute(
        &call(
            CapabilityId::RunCommand,
            "c_retention_fail",
            serde_json::json!({ "argv": ["printf", script] }),
        ),
        &authority(),
    );

    match outcome {
        // Failing closed is acceptable.
        Err(ExecutionError::Failed(_)) => {}
        // Succeeding is acceptable only if no handle was advertised.
        Ok(outcome) => assert!(
            !outcome.result_text.contains(".familiar/tool-output/"),
            "a paging handle was advertised for output that was never retained: {}",
            outcome.result_text
        ),
        Err(other) => panic!("unexpected error {other:?}"),
    }

    // The blocker sits in shared temp storage, not under `temp`, so dropping
    // the worktree does not reap it.
    let _ = fs::remove_file(&blocker);
}

/// Retained tool output is verbatim command output from a worker that has
/// read the repository — build logs, test output, whatever the command
/// printed. It must never inherit the ambient umask: the store sits under
/// the shared, world-readable system temp directory at a path any local
/// process can derive, so the directory's and the file's own permission bits
/// are the entire barrier between one local user's retained output and every
/// other local user on the host.
#[test]
fn retained_tool_output_is_not_readable_by_other_users() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());
    let retained = executor
        .tool_output_retention_dir(&authority().execution_id)
        .join("c_perms.txt");
    let _ = fs::remove_dir_all(retained.parent().unwrap());
    let script: String = (1..=20).map(|n| format!("line{n}\\n")).collect();
    executor
        .execute(
            &call(
                CapabilityId::RunCommand,
                "c_perms",
                serde_json::json!({ "argv": ["printf", script] }),
            ),
            &authority(),
        )
        .unwrap();

    assert!(
        retained.exists(),
        "expected retained output at {retained:?}"
    );

    let dir_mode = fs::metadata(retained.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        dir_mode & 0o077,
        0,
        "retention directory is accessible to other users (mode {dir_mode:o})"
    );

    let file_mode = fs::metadata(&retained).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        file_mode & 0o077,
        0,
        "retained output is readable by other users (mode {file_mode:o})"
    );

    // The store root the execution directory is created inside, too: a
    // parent another user owns is a parent that can replace what is under
    // it, however the execution directory's own bits are set.
    let store_root = retained.parent().unwrap().parent().unwrap();
    let root_mode = fs::metadata(store_root).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        root_mode & 0o077,
        0,
        "retention store root {store_root:?} is accessible to other users (mode {root_mode:o})"
    );

    // The store is outside the worktree, so dropping `temp` does not reap it.
    // `discard_retained_tool_output` is the stated end of the retention
    // lifecycle; exercise it here rather than leaving temp state behind.
    executor.discard_retained_tool_output(&authority().execution_id);
    assert!(
        !retained.exists(),
        "discarding retained output left {retained:?} behind"
    );
}
