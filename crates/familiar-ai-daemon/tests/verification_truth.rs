//! Pinned trust-boundary regressions for PRD-067.  These live in their own
//! integration target so concurrent daemon PRDs do not share a fixture scope.

use std::collections::BTreeMap;

use familiar_ai_review::{
    AgentAssignment, AgentObservation, AgentRole, BlockingPolicy, CommandVerificationRunner,
    EvidenceRef, ExecutionUsage, ReviewDisposition, ReviewPackageBudget, ReviewPackageManifest,
    ReviewRequest, ReviewResult, ReviewTask, ReviewValidationError, VerificationCheck,
    VerificationEvidence, VerificationRunner, VerificationStatus,
};
use familiar_ai_storage::Database;

#[test]
fn wave2_six_fixture_failures_reject_narrated_focused_test_success() {
    const CHECK_ID: &str = "focused-tests";
    const TESTED_IDENTITY: &str = "fnv1a64:candidate";

    let failed_fixtures = (1..=6)
        .map(|fixture| VerificationEvidence {
            check_id: CHECK_ID.into(),
            argv: vec!["cargo".into(), "test".into(), "--test".into(), "fixture".into()],
            working_directory: ".".into(),
            environment_identity: [("executor".into(), "fixture-process".into())].into(),
            tool_identity: Some("cargo-test-fixture".into()),
            tested_identity: TESTED_IDENTITY.into(),
            started_at: format!("2026-08-30T12:00:0{fixture}Z"),
            ended_at: format!("2026-08-30T12:00:0{fixture}Z"),
            duration_ms: 1,
            exit_code: Some(1),
            signal: None,
            status: VerificationStatus::Failed,
            required: true,
            summary: format!(
                "fixture {fixture} failed; worker narration nevertheless claimed focused tests passed"
            ),
            stdout: None,
            stderr: None,
            truncated: false,
        })
        .collect();
    let assignment = |role| AgentAssignment {
        adapter_id: "fixture-agent".into(),
        agent_id: "fixture-agent".into(),
        provider: None,
        requested_model: None,
        role,
        session_id: Some("wave2".into()),
    };
    let request = ReviewRequest {
        review_id: "wave2-review".into(),
        task: ReviewTask {
            task_id: "wave2".into(),
            objective: "pin durable verification truth".into(),
            acceptance_criteria: vec![],
            base_revision: "base".into(),
            allowed_paths: vec![],
            prohibited_changes: vec![],
            verification_plan_id: "wave2".into(),
        },
        implementation: assignment(AgentRole::Implementation),
        reviewer: assignment(AgentRole::Review),
        base_revision: "base".into(),
        candidate_revision: Some("candidate".into()),
        changed_files: vec![],
        diff: EvidenceRef {
            content_hash: TESTED_IDENTITY.into(),
            media_type: "text/x-diff".into(),
            byte_size: 0,
            repository: "fixture-repository".into(),
            revision: "candidate".into(),
            storage_ref: "fixture".into(),
            truncated: false,
            omitted_bytes: 0,
        },
        disclosed_diff: String::new(),
        contracts: vec![],
        invariants: vec![],
        verification: failed_fixtures,
        prior_findings: vec![],
        budget: ReviewPackageBudget {
            max_bytes: 1,
            max_estimated_tokens: 1,
        },
        manifest: ReviewPackageManifest {
            manifest_hash: "wave2-manifest".into(),
            diff_hash: TESTED_IDENTITY.into(),
            included_sources: vec![],
            omissions: vec![],
            total_bytes: 0,
            estimated_tokens: 0,
        },
    };
    let narrated_success = ReviewResult {
        review_id: request.review_id.clone(),
        reviewer: AgentObservation {
            assignment: request.reviewer.clone(),
            agent_version: None,
            reported_model: None,
            unavailable_fields: BTreeMap::new(),
        },
        started_at: "2026-08-30T12:01:00Z".into(),
        ended_at: "2026-08-30T12:01:00Z".into(),
        duration_ms: 1,
        findings: vec![],
        reviewed_manifest_hash: request.manifest.manifest_hash.clone(),
        usage: ExecutionUsage::default(),
        disposition: ReviewDisposition::ReadyForHumanApproval,
        unavailable_fields: BTreeMap::new(),
    };

    let phase_failure = BlockingPolicy::default()
        .apply_and_validate(&request, narrated_success)
        .unwrap_err();
    assert_eq!(
        phase_failure,
        ReviewValidationError::NarrationContradiction(CHECK_ID.into())
    );
}

#[test]
fn environment_denial_is_durable_and_names_the_check_and_environment() {
    let repository = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    let runner = CommandVerificationRunner::new(artifacts.path().into(), 1024);
    let check = VerificationCheck {
        check_id: "focused-tests".into(),
        argv: vec!["definitely-not-an-installed-verification-tool".into()],
        working_directory: ".".into(),
        environment: BTreeMap::new(),
        timeout_ms: 100,
        required: true,
        path_prefixes: vec![],
    };
    let evidence = runner.run(repository.path(), &check, "candidate").unwrap();
    assert_eq!(evidence.status, VerificationStatus::EnvironmentDenied);
    assert!(evidence.summary.contains("focused-tests"));
    assert_eq!(evidence.environment_identity["executor"], "local-process");
    assert!(evidence.environment_identity.contains_key("repository"));
}

#[test]
fn migration_032_persists_waiver_and_verification_truth_dimensions() {
    let database = Database::open_in_memory().unwrap();
    database.run_migrations().unwrap();
    let waiver_table: i64 = database.conn().query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='review_finding_waivers'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(waiver_table, 1);
    let columns = database
        .conn()
        .prepare("PRAGMA table_info(review_verification_evidence)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.contains(&"repository_key".into()));
    assert!(columns.contains(&"environment_identity_json".into()));
    assert!(columns.contains(&"classification".into()));
}
