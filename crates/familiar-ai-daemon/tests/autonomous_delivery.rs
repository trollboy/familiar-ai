//! PRD-077 closure regression for FAM-BUG-019: a two-PRD wave whose members
//! share a mutable scope completes END TO END through `drive` alone — fake
//! implementation, clean structured review, merge-queue integration in review
//! order, and backlog completion — with zero manual Git operations. The
//! second PRD must execute from a base that already contains the first PRD's
//! integrated work.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use familiar_ai_agent::{
    AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult, IsolationCapability,
};
use familiar_ai_core::config::{ReviewAgentConfig, ReviewVerificationConfig};
use familiar_ai_core::{AppPaths, Config};
use familiar_ai_daemon::drive::{drive, DriveWarrant};
use familiar_ai_daemon::run::AgentSet;
use familiar_ai_storage::{Database, DriverRepository, OrchestrationRepository};

static WORKING_DIRECTORY: Mutex<()> = Mutex::new(());

/// Implementation fake: creates one file per PRD inside the execution
/// worktree and records whether the FIRST PRD's output was already present in
/// its base — the proof that integration, not luck, ordered the wave.
struct WritingAgent;

impl CodingAgent for WritingAgent {
    fn isolation_capability(&self) -> IsolationCapability {
        IsolationCapability::FreshProcessPerExecution
    }
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        _output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let number = request
            .prompt
            .lines()
            .find_map(|line| line.strip_prefix("## PRD: docs/prds/PRD-"))
            .and_then(|rest| rest.trim_end_matches(".md").parse::<u32>().ok())
            .expect("prompt names the PRD");
        let first_present = request.working_directory.join("src/prd_1.txt").exists();
        fs::write(
            request
                .working_directory
                .join(format!("src/prd_{number}.txt")),
            format!("prd1_present={first_present}\n"),
        )
        .unwrap();
        Ok(ExecutionResult {
            agent_version: Some("fake 1".into()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            exit_code: Some(0),
            ..ExecutionResult::default()
        })
    }
}

/// Reviewer fake: reads the serialized review request from its isolated
/// workspace and returns the minimal clean wire result — identity echoes and
/// zero findings.
struct CleanReviewer;

impl CodingAgent for CleanReviewer {
    fn execute(
        &self,
        request: ExecutionRequest<'_>,
        output: &mut dyn std::io::Write,
    ) -> Result<ExecutionResult, AgentExecutionError> {
        let raw = fs::read_to_string(request.working_directory.join("review-request.json"))
            .expect("review workspace carries the request");
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let wire = serde_json::json!({
            "review_id": value["review_id"],
            "reviewed_manifest_hash": value["manifest"]["manifest_hash"],
            "findings": [],
        });
        output.write_all(wire.to_string().as_bytes()).unwrap();
        Ok(ExecutionResult {
            agent_version: Some("fake-reviewer 1".into()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            exit_code: Some(0),
            ..ExecutionResult::default()
        })
    }
}

fn git(repository: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repository)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn prd(number: u32) -> String {
    format!(
        "# PRD-{number:03}: Fixture {number}\n\n\
         **Status:** Ready for implementation\n\n\
         ## Objective\n\nFixture objective.\n\n\
         ## Acceptance Criteria\n\n1. Fixture criterion.\n\n\
         ## Expected Files\n\n- `src/`\n"
    )
}

fn fixture() -> (tempfile::TempDir, AppPaths, Config) {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("repo");
    fs::create_dir_all(repository.join("docs/prds")).unwrap();
    fs::create_dir_all(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "// fixture\n").unwrap();
    for number in 1..=2 {
        fs::write(
            repository.join(format!("docs/prds/PRD-{number:03}.md")),
            prd(number),
        )
        .unwrap();
    }
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "fixture"]);

    let app = temp.path().join("app");
    let paths = AppPaths {
        config_dir: app.join("config"),
        data_dir: app.join("data"),
        state_dir: app.join("state"),
        runtime_dir: app.join("runtime"),
        log_dir: app.join("log"),
        socket_path: app.join("runtime/socket"),
        pid_path: app.join("state/pid"),
    };
    fs::create_dir_all(&paths.data_dir).unwrap();
    let mut config = Config::default();
    config.database.path = Some(paths.data_dir.join("familiar.db"));
    config.review.enabled = true;
    config.review.max_total_tokens = 5_000_000;
    config.review.scope.allow_prd_expected_file_expansion = true;
    config.review.verification = vec![ReviewVerificationConfig {
        check_id: "noop".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "true".into()],
        working_directory: ".".into(),
        timeout_ms: 60_000,
        required: true,
        path_prefixes: Vec::new(),
        environment: Default::default(),
    }];
    config.review.implementation_agent = ReviewAgentConfig {
        adapter_id: "fake".into(),
        agent_id: "fake-implementation".into(),
        provider: Some("fake-implementer".into()),
        model: None,
    };
    config.review.reviewer_agent = ReviewAgentConfig {
        adapter_id: "fake".into(),
        agent_id: "fake-reviewer".into(),
        provider: Some("fake-reviewer".into()),
        model: None,
    };
    config.driver.max_parallel_components = 2;
    (temp, paths, config)
}

fn with_working_directory<T>(repository: &Path, body: impl FnOnce() -> T) -> T {
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(repository).unwrap();
    let result = body();
    std::env::set_current_dir(previous).unwrap();
    result
}

#[test]
fn shared_scope_wave_completes_end_to_end_through_drive_alone() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, config) = fixture();
    let repository = temp.path().join("repo");
    let implementation = WritingAgent;
    let reviewer = CleanReviewer;
    let set = AgentSet {
        implementation: &implementation,
        reviewer: &reviewer,
        remediation: &reviewer,
    };
    let warrant = DriveWarrant {
        max_prds: 2,
        max_duration_ms: 600_000,
        ..DriveWarrant::default()
    };
    let summary = with_working_directory(&repository, || {
        drive(&set, &config, &paths, warrant).unwrap()
    });

    assert_eq!(
        summary.completed, 2,
        "both PRDs must complete autonomously (termination: {:?})",
        summary.termination
    );

    let db = Database::open(&config.database.path.clone().unwrap()).unwrap();
    let session = DriverRepository::new(db.conn())
        .latest_session()
        .unwrap()
        .expect("session recorded");

    // Integration actually advanced: the session's integration revision is a
    // real commit different from the fixture base.
    let integration = OrchestrationRepository::new(db.conn())
        .integration_revision(&session.session_id)
        .unwrap();
    let base = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repository)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    assert_ne!(integration, base, "integration revision must advance");

    // The integrated revision contains BOTH PRDs' outputs, and PRD-2 saw
    // PRD-1's work in its own base — integration ordered the wave.
    let show = |path: &str| -> String {
        let output = Command::new("git")
            .args(["show", &format!("{integration}:{path}")])
            .current_dir(&repository)
            .output()
            .unwrap();
        assert!(output.status.success(), "missing {path} in integration");
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(show("src/prd_1.txt"), "prd1_present=false\n");
    assert_eq!(
        show("src/prd_2.txt"),
        "prd1_present=true\n",
        "PRD-2 must build on PRD-1's integrated base"
    );

    // The shared scope serialized the wave through the queue, durably.
    let decisions = DriverRepository::new(db.conn())
        .selection_decisions(&session.session_id)
        .unwrap();
    assert!(
        decisions.iter().any(|(prd, decision, _)| prd == "PRD-2"
            && (decision == "deferred_scope_overlap" || decision == "deferred_scope_held")),
        "PRD-2 must defer on the shared scope before its turn: {decisions:?}"
    );
    assert_eq!(
        decisions
            .iter()
            .filter(|(_, decision, _)| decision == "ready_selected")
            .count(),
        2
    );
}

/// PRD-077 circuit breaker: three identical deterministic terminal failures
/// stop the session with one executable recovery plan instead of burning the
/// remaining warrant (the wave-3 synthetic-model shape, reproduced here with
/// review_disabled as the shared deterministic cause).
#[test]
fn three_identical_deterministic_failures_trip_the_circuit_breaker() {
    let _guard = WORKING_DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    let (temp, paths, mut config) = fixture();
    // Disjoint scopes and disabled review: every attempt deterministically
    // retains as review_disabled.
    config.review.enabled = false;
    config.driver.max_parallel_components = 1;
    let repository = temp.path().join("repo");
    for number in 3..=4u32 {
        fs::write(
            repository.join(format!("docs/prds/PRD-{number:03}.md")),
            prd(number).replace("- `src/`", &format!("- `component{number}/`")),
        )
        .unwrap();
    }
    // Make the first two disjoint as well so all four are admissible.
    for number in 1..=2u32 {
        fs::write(
            repository.join(format!("docs/prds/PRD-{number:03}.md")),
            prd(number).replace("- `src/`", &format!("- `component{number}/`")),
        )
        .unwrap();
    }
    git(&repository, &["add", "-A"]);
    git(&repository, &["commit", "-qm", "breaker fixture"]);
    let implementation = WritingAgent;
    let reviewer = CleanReviewer;
    let set = AgentSet {
        implementation: &implementation,
        reviewer: &reviewer,
        remediation: &reviewer,
    };
    let warrant = DriveWarrant {
        max_prds: 4,
        max_duration_ms: 600_000,
        ..DriveWarrant::default()
    };
    let summary = with_working_directory(&repository, || {
        drive(&set, &config, &paths, warrant).unwrap()
    });
    assert_eq!(
        summary.termination,
        familiar_ai_daemon::drive::DriveTermination::DeterministicFailureCascade,
        "third identical deterministic failure must stop the session"
    );
    assert!(
        summary.attempted <= 3,
        "the fourth PRD must never be attempted (attempted {})",
        summary.attempted
    );
    let db = Database::open(&config.database.path.clone().unwrap()).unwrap();
    let session = DriverRepository::new(db.conn())
        .latest_session()
        .unwrap()
        .unwrap();
    let detail = session.termination_detail.unwrap_or_default();
    assert!(
        detail.contains("deterministic_failure_cascade reason=review_disabled"),
        "recovery plan must name the shared cause: {detail}"
    );
    assert!(
        detail.contains("familiar-ai drive"),
        "recovery plan must be executable: {detail}"
    );
}
