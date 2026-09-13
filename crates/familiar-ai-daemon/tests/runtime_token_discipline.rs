//! PRD-072 raw-runtime token-discipline regressions: targeted-edit anchor
//! divergence, crash-replay identity for a targeted edit, bounded tool
//! results with lossless retention, and byte-identical defaults when token
//! discipline is disabled. Exercises `SandboxedToolExecutor` directly
//! through the same `ToolExecutor` trait `familiar_ai_agent::raw_runtime`
//! drives it through — no live or billable call anywhere in this file.

use std::fs;

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

fn disabled_executor(worktree_root: std::path::PathBuf) -> SandboxedToolExecutor {
    SandboxedToolExecutor::new(
        worktree_root,
        no_sandbox(),
        2_000,
        1 << 20,
        TokenDisciplineConfig::default(),
    )
    .unwrap()
}

#[test]
fn anchor_divergence_is_rejected_and_leaves_the_file_untouched() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("lib.rs"),
        "fn main() {\n    current();\n}\n",
    )
    .unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());

    let edit = call(
        CapabilityId::ApplyEdit,
        "call_1",
        serde_json::json!({
            "path": "lib.rs",
            "change_kind": "search-replace",
            "content": r#"[{"search":"stale_call();","replace":"new();"}]"#,
        }),
    );
    let error = executor.execute(&edit, &authority()).unwrap_err();
    match error {
        ExecutionError::Failed(detail) => assert!(
            detail.contains("AnchorDivergence"),
            "expected a named anchor-divergence diagnostic, got: {detail}"
        ),
        other => panic!("expected ExecutionError::Failed, got {other:?}"),
    }
    // Never a silent misapply: the file is exactly what it was before.
    assert_eq!(
        fs::read_to_string(temp.path().join("lib.rs")).unwrap(),
        "fn main() {\n    current();\n}\n"
    );
}

#[test]
fn targeted_edit_replay_after_crash_reproduces_the_identical_file_state() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("lib.rs"), "fn main() {\n    old();\n}\n").unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());

    let edit = call(
        CapabilityId::ApplyEdit,
        "call_1",
        serde_json::json!({
            "path": "lib.rs",
            "change_kind": "search-replace",
            "content": r#"[{"search":"old();","replace":"new();"}]"#,
        }),
    );

    // First execution: the write actually lands.
    let first = executor.execute(&edit, &authority()).unwrap();
    let after_first = fs::read_to_string(temp.path().join("lib.rs")).unwrap();
    assert_eq!(after_first, "fn main() {\n    new();\n}\n");

    // Simulates a resumed loop replaying the identical call after a crash
    // between the write landing on disk and its journal result being
    // recorded (PRD-058's write-ahead journal: intent-only-no-result on an
    // idempotent-write replays). The anchor is gone, but resolve_edit
    // recognizes the replacement is already present and reproduces the
    // same file state rather than failing closed on its own prior write.
    let second = executor.execute(&edit, &authority()).unwrap();
    let after_second = fs::read_to_string(temp.path().join("lib.rs")).unwrap();
    assert_eq!(after_second, after_first);
    // The replay is reported distinctly from the first, genuine write — the
    // model must never see a diverged (or, as here, already-applied) no-op
    // reported the same way as `wrote N bytes`.
    assert!(first.result_text.starts_with("wrote"));
    assert!(second
        .result_text
        .contains("already applied, no bytes written"));
    assert_ne!(first.result_hash, second.result_hash);
}

#[test]
fn bounded_command_result_shows_head_tail_and_retains_full_output_losslessly() {
    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());

    // 20 lines of stdout, well beyond the 10-line window configured above.
    let script: String = (1..=20).map(|n| format!("line{n}\\n")).collect();
    let run = call(
        CapabilityId::RunCommand,
        "call_paged",
        serde_json::json!({ "argv": ["printf", script] }),
    );
    let outcome = executor.execute(&run, &authority()).unwrap();

    assert!(
        outcome.result_text.contains("lines elided"),
        "expected an elided-line marker, got: {}",
        outcome.result_text
    );
    assert!(outcome
        .result_text
        .contains(".familiar/tool-output/call_paged.txt"));
    assert!(outcome.result_text.contains("line1"));
    assert!(outcome.result_text.contains("line20"));
    // The model's window must not contain every line — that is the point
    // of bounding it in the first place.
    assert!(!outcome.result_text.contains("line10"));

    // Lossless retention lives in daemon-owned storage outside the
    // governed worktree — never as untracked bytes inside it, where it
    // would need write-scope authorization no PRD-013 Expected Files entry
    // could ever cover, and would pollute the worktree's own
    // diff/expected-files evidence.
    assert!(!temp.path().join(".familiar").exists());
    let full = fs::read_to_string(
        executor
            .tool_output_retention_dir("exec_1")
            .join("call_paged.txt"),
    )
    .unwrap();
    for n in 1..=20 {
        assert!(
            full.contains(&format!("line{n}")),
            "missing line{n} in retained output"
        );
    }

    // A worker can always retrieve any elided region through the handle:
    // the executor resolves the same opaque handle string as a read-file
    // target even though it is not a real worktree-relative path.
    let page = call(
        CapabilityId::ReadFile,
        "call_page_2",
        serde_json::json!({
            "path": ".familiar/tool-output/call_paged.txt",
            "start_line": 12,
            "end_line": 14,
        }),
    );
    let outcome = executor.execute(&page, &authority()).unwrap();
    // Lines 1-2 of the retained file are the "exit_status=..."/"stdout:"
    // preamble, so line N of stdout is retained-file line N+2.
    assert_eq!(outcome.result_text, "line10\nline11\nline12");
}

#[test]
fn retention_write_failure_falls_back_to_unbounded_result_without_a_broken_handle() {
    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());

    // Force retention to fail: pre-create a plain file at the exact path
    // the executor would otherwise create as this execution's retention
    // directory. `execution_id` scopes this to `authority()`'s own
    // "exec_1", so it cannot collide with any other test's retention
    // writes running concurrently in the same process.
    let retention_dir = executor.tool_output_retention_dir("exec_1");
    fs::create_dir_all(retention_dir.parent().unwrap()).unwrap();
    fs::write(&retention_dir, b"not a directory").unwrap();

    let script: String = (1..=20).map(|n| format!("line{n}\\n")).collect();
    let run = call(
        CapabilityId::RunCommand,
        "call_fail",
        serde_json::json!({ "argv": ["printf", script] }),
    );
    let outcome = executor.execute(&run, &authority()).unwrap();

    // Retention is a precondition of bounding: when the full output cannot
    // be durably persisted, the call falls back to the unbounded (byte-
    // capped) result rather than emitting a paging handle for a region
    // that no longer exists.
    assert!(!outcome.result_text.contains("lines elided"));
    assert!(!outcome.result_text.contains(".familiar/tool-output"));
    assert!(outcome.result_text.contains("line1\n"));
    assert!(outcome.result_text.contains("line20"));

    fs::remove_file(&retention_dir).ok();
}

#[test]
fn disabled_token_discipline_reproduces_pre_prd_behavior_byte_for_byte() {
    let temp = tempfile::tempdir().unwrap();
    let mut executor = disabled_executor(temp.path().to_path_buf());

    // A whole-file apply-edit's result text carries no change_kind
    // annotation when discipline is off — exactly the PRD-058 wording.
    let edit = call(
        CapabilityId::ApplyEdit,
        "call_1",
        serde_json::json!({ "path": "lib.rs", "content": "fn main() {}" }),
    );
    let outcome = executor.execute(&edit, &authority()).unwrap();
    assert_eq!(outcome.result_text, "wrote 12 bytes to lib.rs");

    // A large command result is truncated only by the raw byte cap, never
    // windowed into head/tail with an elided count or a paging handle.
    let script: String = (1..=20).map(|n| format!("line{n}\\n")).collect();
    let run = call(
        CapabilityId::RunCommand,
        "call_2",
        serde_json::json!({ "argv": ["printf", script] }),
    );
    let outcome = executor.execute(&run, &authority()).unwrap();
    assert!(!outcome.result_text.contains("lines elided"));
    assert!(!outcome.result_text.contains("paging"));
    assert!(outcome.result_text.contains("line1\n"));
    assert!(outcome.result_text.contains("line20"));
    assert!(!temp.path().join(".familiar").exists());

    // A large file read returns the whole file unconditionally — no
    // "specify start_line/end_line" refusal.
    let long_file = (1..=50)
        .map(|n| format!("line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(temp.path().join("big.txt"), &long_file).unwrap();
    let read = call(
        CapabilityId::ReadFile,
        "call_3",
        serde_json::json!({ "path": "big.txt" }),
    );
    let outcome = executor.execute(&read, &authority()).unwrap();
    assert_eq!(outcome.result_text, long_file);
}

#[test]
fn tool_output_handle_path_traversal_is_rejected_not_diverted() {
    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());

    // A malicious `call_id` crafted to walk out of the retention directory
    // and read an arbitrary `.txt`-suffixed file the daemon process can
    // see. This must be refused with a diagnostic — never silently resolve
    // outside `tool_output_retention_dir`.
    let traversal = call(
        CapabilityId::ReadFile,
        "call_1",
        serde_json::json!({
            "path": ".familiar/tool-output/../../../../../../etc/passwd.txt",
        }),
    );
    let error = executor.execute(&traversal, &authority()).unwrap_err();
    match error {
        ExecutionError::Failed(detail) => assert!(
            !detail.contains("root:"),
            "must never leak file content in the error, got: {detail}"
        ),
        other => panic!("expected ExecutionError::Failed, got {other:?}"),
    }
}

#[test]
fn tool_output_handle_diversion_is_inactive_when_token_discipline_is_disabled() {
    let temp = tempfile::tempdir().unwrap();
    // A genuine worktree file that happens to collide with the handle
    // naming convention. With token discipline disabled, `read-file` MUST
    // resolve it as an ordinary worktree-relative path (byte-for-byte
    // pre-PRD-072 behavior) rather than diverting to retention storage.
    fs::create_dir_all(temp.path().join(".familiar/tool-output")).unwrap();
    fs::write(
        temp.path().join(".familiar/tool-output/call_paged.txt"),
        "genuine worktree contents",
    )
    .unwrap();
    let mut executor = disabled_executor(temp.path().to_path_buf());

    let read = call(
        CapabilityId::ReadFile,
        "call_1",
        serde_json::json!({ "path": ".familiar/tool-output/call_paged.txt" }),
    );
    let outcome = executor.execute(&read, &authority()).unwrap();
    assert_eq!(outcome.result_text, "genuine worktree contents");
}

#[cfg(unix)]
#[test]
fn retained_tool_output_directory_and_file_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());

    let script: String = (1..=20).map(|n| format!("line{n}\\n")).collect();
    let run = call(
        CapabilityId::RunCommand,
        "call_perm",
        serde_json::json!({ "argv": ["printf", script] }),
    );
    executor.execute(&run, &authority()).unwrap();

    let retention_dir = executor.tool_output_retention_dir("exec_1");
    let dir_mode = fs::metadata(&retention_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        dir_mode, 0o700,
        "retained tool output lives under the shared system temp directory; \
         its directory must be owner-only, not world- or group-readable"
    );
    let file_mode = fs::metadata(retention_dir.join("call_perm.txt"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        file_mode, 0o600,
        "a retained tool-output file must be owner-only, not world- or group-readable"
    );
}

#[test]
fn file_read_beyond_the_span_requires_an_explicit_range_when_enabled() {
    let temp = tempfile::tempdir().unwrap();
    let mut executor = enabled_executor(temp.path().to_path_buf());
    let long_file = (1..=50)
        .map(|n| format!("line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(temp.path().join("big.txt"), &long_file).unwrap();

    let whole = call(
        CapabilityId::ReadFile,
        "call_1",
        serde_json::json!({ "path": "big.txt" }),
    );
    let error = executor.execute(&whole, &authority()).unwrap_err();
    match error {
        ExecutionError::Failed(detail) => assert!(detail.contains("start_line")),
        other => panic!("expected ExecutionError::Failed, got {other:?}"),
    }

    let ranged = call(
        CapabilityId::ReadFile,
        "call_2",
        serde_json::json!({ "path": "big.txt", "start_line": 2, "end_line": 3 }),
    );
    let outcome = executor.execute(&ranged, &authority()).unwrap();
    assert_eq!(outcome.result_text, "line2\nline3");
}
