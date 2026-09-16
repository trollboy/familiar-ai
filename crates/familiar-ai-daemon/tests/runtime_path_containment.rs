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
    SandboxedToolExecutor {
        worktree_root,
        sandbox: no_sandbox(),
        command_timeout_ms: 2_000,
        max_output_bytes: 1 << 20,
        token_discipline: TokenDisciplineConfig {
            enabled: true,
            targeted_edit_threshold_bytes: 10,
            tool_result_max_lines: 10,
            tool_result_head_lines: 3,
            tool_result_tail_lines: 3,
            file_read_max_lines: 5,
        },
    }
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

/// A hard link is not a symlink, and that is the whole problem: the link *is*
/// the file, so `canonicalize` hands back the in-worktree path and
/// `symlink_metadata` reports an ordinary regular file. Every resolved-path
/// check PRD-081 installed passes it, and the bytes come from outside the
/// worktree all the same.
///
/// `fs.protected_hardlinks` does not save us — it only refuses to link a file
/// the caller does not own, and the daemon's uid owns its own `~/.ssh`. `ln`
/// without `-s` is a hard link, and `ln` is exactly the command the runtime's
/// own tests allowlist.
#[test]
fn read_file_refuses_a_hard_link_to_a_file_outside_the_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa");
    fs::write(&secret, "PRIVATE KEY BYTES\n").unwrap();
    fs::hard_link(&secret, temp.path().join("innocent.txt")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::ReadFile,
            "c_hardlink_read",
            serde_json::json!({ "path": "innocent.txt" }),
        ),
        &authority(),
    );

    match result {
        Err(ExecutionError::Failed(_)) => {}
        Ok(outcome) => panic!(
            "read-file returned bytes from outside the worktree through a hard link: {}",
            outcome.result_text
        ),
        Err(other) => panic!("expected ExecutionError::Failed, got {other:?}"),
    }
}

/// The write direction of the same hole, and the severe one: a hard link
/// turns `apply-edit` into an arbitrary-file overwrite for anything the
/// daemon's uid owns.
#[test]
fn apply_edit_refuses_to_write_through_a_hard_link() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    fs::write(&victim, "ORIGINAL\n").unwrap();
    fs::hard_link(&victim, temp.path().join("innocent.txt")).unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    let result = executor.execute(
        &call(
            CapabilityId::ApplyEdit,
            "c_hardlink_write",
            serde_json::json!({
                "path": "innocent.txt",
                "change_kind": "whole-file",
                "content": "OVERWRITTEN\n",
            }),
        ),
        &authority(),
    );

    assert!(
        matches!(result, Err(ExecutionError::Failed(_))),
        "apply-edit through a hard link must be refused, got {result:?}"
    );
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        "ORIGINAL\n",
        "a file outside the worktree was modified through a hard link"
    );
}

/// A hard link whose every name happens to be inside the worktree is refused
/// too, and that is deliberate rather than collateral.
///
/// Containment here is decided by soundness, not by suspicion. `nlink == 1`
/// proves the file has exactly one name and that name is the resolved,
/// contained one. `nlink > 1` means other names exist and no portable syscall
/// enumerates them — the kernel offers no inode-to-paths lookup — so whether
/// a second name sits outside the root is *undecidable*, not merely
/// unchecked. Undecidable fails closed.
///
/// The cost is nil in practice: git cannot represent a hard link at all (it
/// stores two independent blobs), so nothing in a legitimate checkout depends
/// on one. This test exists to pin the refusal as intended behaviour, so a
/// later "fix" does not quietly reopen the escape by special-casing it.
#[test]
fn a_hard_link_is_refused_even_when_every_name_is_inside_the_worktree() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("real.txt"), "INSIDE\n").unwrap();
    fs::hard_link(temp.path().join("real.txt"), temp.path().join("alias.txt")).unwrap();
    // A genuinely single-named file: linking `real.txt` raised *its* link
    // count too, so the original is no more provable than its alias.
    fs::write(temp.path().join("ordinary.txt"), "ORDINARY\n").unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
    for path in ["alias.txt", "real.txt"] {
        let result = executor.execute(
            &call(
                CapabilityId::ReadFile,
                "c_hardlink_inside",
                serde_json::json!({ "path": path }),
            ),
            &authority(),
        );
        let Err(ExecutionError::Failed(message)) = result else {
            panic!("a multiply-named file cannot be proven contained, got {result:?}");
        };
        assert!(
            message.contains(path) && message.contains("hard link"),
            "the refusal must say which path and why, got {message:?}"
        );
    }

    // This is a link-count rule, not a ban on reading ordinary files.
    let outcome = executor
        .execute(
            &call(
                CapabilityId::ReadFile,
                "c_hardlink_ordinary",
                serde_json::json!({ "path": "ordinary.txt" }),
            ),
            &authority(),
        )
        .expect("a file with one name is provably contained");
    assert!(
        outcome.result_text.contains("ORDINARY"),
        "an ordinary file must stay readable: {}",
        outcome.result_text
    );
}

/// The lexical guard closes plain parent traversal today. Containment is
/// about to be rewritten around canonicalization, so pin the existing
/// behaviour to keep the rewrite from reopening it.
///
/// Criterion 4 of PRD-081 requires the diagnostic to *name* the offending
/// path, so assert on the message and not merely on the error variant: a
/// refusal that said only "refused" would satisfy `matches!` while losing the
/// one fact an operator needs.
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
        let Err(ExecutionError::Failed(message)) = result else {
            panic!("path {path:?} must be refused, got {result:?}");
        };
        assert!(
            message.contains(&format!("{path:?}")),
            "the refusal must name the offending path {path:?}, got {message:?}"
        );
    }
}

/// Retention advertises a paging handle to the model. The write that backs
/// that handle currently discards its error (`let _ = fs::write(..)`) and the
/// handle is emitted regardless — so the model can be handed a pointer to a
/// file that was never created, which is precisely the silent narrowing the
/// surrounding comment claims cannot happen.
#[test]
#[ignore = "PRD-082: fails until containment/retention is fixed; removing this attribute is an acceptance criterion"]
fn a_failed_retention_write_never_advertises_an_unresolvable_handle() {
    let temp = tempfile::tempdir().unwrap();
    // Occupy `.familiar/tool-output` with a regular file so that both
    // create_dir_all and the subsequent write fail.
    fs::create_dir_all(temp.path().join(".familiar")).unwrap();
    fs::write(temp.path().join(".familiar/tool-output"), "blocker").unwrap();

    let mut executor = enabled_executor(temp.path().to_path_buf());
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
}

/// Retained tool output is verbatim command output from a worker that has
/// read the repository. It currently inherits the ambient umask, so on a
/// multi-user host it lands group- and world-readable — and `worktree_root`
/// is operator-configurable, so it need not sit under `$HOME` at all.
#[test]
#[ignore = "PRD-082: fails until containment/retention is fixed; removing this attribute is an acceptance criterion"]
fn retained_tool_output_is_not_readable_by_other_users() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());
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

    let retained = temp.path().join(".familiar/tool-output/c_perms.txt");
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
}
