use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Mutex;

use familiar_ai_core::{DeliveryConfig, DeliveryMode};
use familiar_ai_daemon::delivery::{deliver_with, CommandRunner, DeliveryJournal};
use familiar_ai_daemon::worktree::WorktreeOwnership;
use familiar_ai_storage::{Database, DriverRepository};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

struct RecordingRunner {
    calls: Mutex<Vec<Vec<String>>>,
}

impl RecordingRunner {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, _directory: &Path, argv: &[String]) -> Result<Output, String> {
        self.calls.lock().unwrap().push(argv.to_vec());
        Err("injected network partition".into())
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf, DeliveryConfig) {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let ownership = temp.path().join("attempt.ownership.json");
    fs::write(
        &ownership,
        serde_json::to_vec(&WorktreeOwnership {
            session_id: "session-security".into(),
            prd_id: "PRD-037".into(),
            worktree,
            created_at: "2026-08-30T00:00:00Z".into(),
            heartbeat_at: "2026-08-30T00:00:00Z".into(),
            state: "ready_for_delivery".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let policy = DeliveryConfig {
        mode: DeliveryMode::ReviewedPrManual,
        enabled: true,
        max_deliveries_per_session: 1,
        command_timeout_ms: 1_000,
        remote: "origin".into(),
        base: "main".into(),
        provider_argv: vec!["provider".into()],
        ..DeliveryConfig::default()
    };
    (temp, ownership, policy)
}

#[test]
fn corrupt_delivery_journal_runs_no_external_command() {
    let (_temp, ownership, policy) = fixture();
    fs::write(ownership.with_extension("delivery.json"), b"{truncated").unwrap();
    let runner = RecordingRunner::new();
    let error = deliver_with(&ownership, &policy, &runner).unwrap_err();
    assert!(error.contains("invalid delivery journal"));
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn published_delivery_resume_skips_prior_external_effects() {
    let (_temp, ownership, policy) = fixture();
    let owner: WorktreeOwnership = serde_json::from_slice(&fs::read(&ownership).unwrap()).unwrap();
    fs::write(
        ownership.with_extension("delivery.json"),
        serde_json::to_vec(&DeliveryJournal {
            session_id: owner.session_id,
            prd_id: owner.prd_id,
            worktree: owner.worktree,
            branch: "familiar/session-security/PRD-037".into(),
            pr_number: Some(37),
            phase: "published".into(),
            detail: Some("reboot-equivalent interruption".into()),
            updated_at: "2026-08-30T00:00:00Z".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let runner = RecordingRunner::new();
    let resumed = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(resumed.phase, "awaiting_merge_authority");
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn restart_storm_is_bounded_and_visible() {
    let rendered = familiar_ai_daemon::systemd::unit(
        "ai.familiar.security",
        Path::new("/opt/familiar-ai"),
        Path::new("/srv/repository"),
        Path::new("/var/log/familiar.out"),
        Path::new("/var/log/familiar.err"),
        "/usr/bin:/bin",
        10,
        1,
    )
    .unwrap();
    assert!(rendered.contains("StartLimitIntervalSec=300"));
    assert!(rendered.contains("StartLimitBurst=5"));
    assert!(rendered.contains("RestartSec=10"));
    assert!(rendered.contains("StandardOutput=append:/var/log/familiar.out"));
    assert!(rendered.contains("StandardError=append:/var/log/familiar.err"));
}

#[test]
fn supervisor_injection_is_rejected_or_escaped() {
    assert!(familiar_ai_daemon::systemd::unit(
        "unsafe label;touch-owned",
        Path::new("/opt/familiar-ai"),
        Path::new("/srv/repository"),
        Path::new("/tmp/out"),
        Path::new("/tmp/err"),
        "/usr/bin:/bin",
        10,
        1,
    )
    .is_err());
    assert!(familiar_ai_daemon::systemd::unit(
        "ai.familiar.security",
        Path::new("/opt/familiar-ai"),
        Path::new("/srv/repository\nExecStart=/tmp/owned"),
        Path::new("/tmp/out"),
        Path::new("/tmp/err"),
        "/usr/bin:/bin",
        10,
        1,
    )
    .is_err());
}

#[test]
fn supervisor_does_not_copy_ambient_secrets() {
    const CANARY: &str = "burn-in-canary-value";
    std::env::set_var("SECURITY_BURN_IN_CANARY", CANARY);
    let rendered = familiar_ai_daemon::launchd::plist(
        "ai.familiar.security",
        Path::new("/opt/familiar-ai"),
        Path::new("/srv/repository"),
        Path::new("/tmp/out"),
        Path::new("/tmp/err"),
        "/usr/bin:/bin",
        10,
        1,
    )
    .unwrap();
    std::env::remove_var("SECURITY_BURN_IN_CANARY");
    assert!(!rendered.contains(CANARY));
}

struct AmbiguousCreateRunner {
    calls: Mutex<Vec<Vec<String>>>,
    provider_effects: Mutex<usize>,
}

impl CommandRunner for AmbiguousCreateRunner {
    fn run(&self, _directory: &Path, argv: &[String]) -> Result<Output, String> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let is_create = argv.iter().any(|value| value == "create");
        if is_create {
            *self.provider_effects.lock().unwrap() += 1;
            return Err("response lost after provider accepted create".into());
        }
        let is_view = argv.iter().any(|value| value == "view");
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: if is_view {
                b"37\n".to_vec()
            } else {
                Vec::new()
            },
            stderr: Vec::new(),
        })
    }
}

#[test]
fn ambiguous_pr_create_is_reconciled_without_repeating_external_effects() {
    let (_temp, ownership, policy) = fixture();
    let runner = AmbiguousCreateRunner {
        calls: Mutex::new(Vec::new()),
        provider_effects: Mutex::new(0),
    };

    let delivered = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(delivered.phase, "awaiting_merge_authority");
    assert_eq!(delivered.pr_number, Some(37));
    let calls_after_ambiguous_result = runner.calls.lock().unwrap().len();

    let resumed = deliver_with(&ownership, &policy, &runner).unwrap();
    assert_eq!(resumed.phase, "awaiting_merge_authority");
    assert_eq!(
        runner.calls.lock().unwrap().len(),
        calls_after_ambiguous_result
    );
    assert_eq!(*runner.provider_effects.lock().unwrap(), 1);

    let calls = runner.calls.lock().unwrap();
    for command in ["commit", "push", "create", "view"] {
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.iter().any(|value| value == command))
                .count(),
            1
        );
    }
}

struct CommentCaptureRunner {
    calls: Mutex<Vec<Vec<String>>>,
}

impl CommandRunner for CommentCaptureRunner {
    fn run(&self, _directory: &Path, argv: &[String]) -> Result<Output, String> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let is_view = argv.iter().any(|value| value == "view");
        let is_checks = argv.iter().any(|value| value == "checks");
        Ok(Output {
            status: std::process::ExitStatus::from_raw(if is_checks { 1 << 8 } else { 0 }),
            stdout: if is_view {
                b"37\n".to_vec()
            } else {
                Vec::new()
            },
            stderr: if is_checks {
                b"fixture check failed: burn-in-secret-canary-durable".to_vec()
            } else {
                Vec::new()
            },
        })
    }
}

#[test]
fn hostile_provider_output_is_redacted_from_reports_database_rows_and_comments() {
    const CANARY: &str = "burn-in-secret-canary-durable";
    let (temp, ownership, mut policy) = fixture();
    policy.mode = DeliveryMode::ReviewGatedAutomatic;
    policy.auto_merge = true;
    policy.comment_blockers = true;
    policy.review_gate = Some(familiar_ai_core::ReviewGateConfig {
        implementer: "implementer".into(),
        reviewer: "reviewer".into(),
        approver: "approver".into(),
    });
    policy.staging_environment = "staging".into();
    policy.deploy_argv = vec!["deploy".into()];
    policy.smoke_argv = vec!["smoke".into()];
    policy.rollback_argv = vec!["rollback".into()];
    let database_path = temp.path().join("state.db");
    let db = Database::open(&database_path).unwrap();
    db.run_migrations().unwrap();
    DriverRepository::new(db.conn())
        .open_session("security-report", "/repo/.git", r#"{"max_prds":1}"#)
        .unwrap();
    std::env::set_var("SECURITY_BURN_IN_DURABLE_CANARY", CANARY);
    DriverRepository::new(db.conn())
        .record_session_detail("security-report", CANARY)
        .unwrap();
    let runner = CommentCaptureRunner {
        calls: Mutex::new(Vec::new()),
    };
    assert!(deliver_with(&ownership, &policy, &runner).is_err());
    let report = familiar_ai_daemon::report::render(&db, None).unwrap();
    std::env::remove_var("SECURITY_BURN_IN_DURABLE_CANARY");

    assert!(!report.contains(CANARY));
    drop(db);
    assert!(!String::from_utf8_lossy(&fs::read(database_path).unwrap()).contains(CANARY));
    let rendered_calls = serde_json::to_string(&*runner.calls.lock().unwrap()).unwrap();
    assert!(
        rendered_calls.contains("comment"),
        "comment surface was not exercised"
    );
    assert!(!rendered_calls.contains(CANARY));
}

#[test]
fn coverage_matrix_identifiers_resolve_to_collected_tests() {
    fn collect_sources(directory: &Path, output: &mut String) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_sources(&path, output);
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                output.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let matrix = fs::read_to_string(root.join("docs/security/coverage-matrix.md")).unwrap();
    let mut sources = String::new();
    collect_sources(&root.join("crates"), &mut sources);

    for identifier in matrix
        .split('`')
        .enumerate()
        .filter_map(|(index, value)| (index % 2 == 1).then_some(value))
        .filter(|value| value.contains("::"))
    {
        let test_name = identifier
            .rsplit("::")
            .next()
            .expect("matrix identifier has a test name");
        assert!(
            sources.contains(&format!("fn {test_name}")),
            "coverage matrix identifier does not resolve: {identifier}"
        );
    }
}
