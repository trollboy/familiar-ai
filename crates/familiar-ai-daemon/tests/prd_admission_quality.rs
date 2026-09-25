use familiar_ai_core::backlog::{
    admission_quality, DiscoveredPrd, PrdId, PrdLocation, PrdMetadata, RepositoryIdentity,
    RepositoryPath,
};
use familiar_ai_storage::{Database, FindingOutcome, ReviewRepository};
use tempfile::TempDir;

fn prd(id: u64, expected: &[&str], criteria: &[&str]) -> DiscoveredPrd {
    DiscoveredPrd {
        id: PrdId::new(id),
        number: id,
        path: RepositoryPath::new(format!("docs/prds/PRD-{id:03}.md")).unwrap(),
        location: PrdLocation::Active,
        title: format!("PRD {id}"),
        dependencies: vec![],
        metadata: PrdMetadata {
            contract_version: Some(1),
            status: Some("ready".into()),
            expected_files: expected.iter().map(|value| (*value).into()).collect(),
            acceptance_criteria: criteria.iter().map(|value| (*value).into()).collect(),
            risk_classes: vec!["persistence".into()],
            ..Default::default()
        },
        content_hash: format!("hash-{id}"),
    }
}

fn repository(temp: &TempDir) -> RepositoryIdentity {
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::create_dir_all(temp.path().join("crates/child")).unwrap();
    RepositoryIdentity {
        worktree: temp.path().into(),
        key: "repository".into(),
    }
}

#[test]
fn malformed_input_is_refused_by_name_before_it_can_be_approved() {
    let temp = TempDir::new().unwrap();
    let repository = repository(&temp);
    let candidate = prd(
        93,
        &["src/lib.rs"],
        &[
            "Implement the feature",
            "The `src/undeclared.rs` result is reported",
        ],
    );
    let report = admission_quality(&repository, std::slice::from_ref(&candidate), &candidate);
    let refusal = report.refusal().unwrap();
    assert!(refusal.contains("acceptance_criteria[0]"));
    assert!(refusal.contains("src/undeclared.rs"));
    assert!(refusal.contains("expected_files"));
}

#[test]
fn directory_claim_is_refused_and_names_queued_conflicts() {
    let temp = TempDir::new().unwrap();
    let repository = repository(&temp);
    let broad = prd(
        93,
        &["crates/"],
        &["The result is reported before selection"],
    );
    let child = prd(
        94,
        &["crates/child/src.rs"],
        &["The result is reported before selection"],
    );
    let report = admission_quality(&repository, &[broad.clone(), child], &broad);
    let refusal = report.refusal().unwrap();
    assert!(refusal.contains("directory claim 'crates/' is refused"));
    assert!(refusal.contains("PRD-94"));
}

#[test]
fn passing_quality_is_durable_but_does_not_grant_human_approval() {
    let temp = TempDir::new().unwrap();
    let repository = repository(&temp);
    let candidate = prd(
        93,
        &["src/lib.rs"],
        &["The result is reported before selection"],
    );
    let report = admission_quality(&repository, std::slice::from_ref(&candidate), &candidate);
    assert!(report.passed());
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let reviews = ReviewRepository::new(db.conn());
    reviews
        .record_admission_quality(&repository.key, &candidate.content_hash, &report)
        .unwrap();
    let rows = reviews
        .admission_quality(&repository.key, "PRD-93")
        .unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row.passed));
    let approvals: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM planner_batches", [], |row| row.get(0))
        .unwrap();
    assert_eq!(approvals, 0, "quality checks must never grant approval");
}

#[test]
fn finding_outcomes_calibrate_without_changing_routes() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    db.conn()
        .execute("INSERT INTO review_tasks(task_id,task_json,policy_json,created_at) VALUES('task','{}','{}','now')", [])
        .unwrap();
    db.conn().execute("INSERT INTO review_cycles(cycle_id,task_id,attempt,state,disposition,cycle_json,started_at) VALUES('cycle','task',1,'completed','ready_for_human_approval','{}','now')",[]).unwrap();
    for (id, raised) in [
        ("one", "2026-01-01"),
        ("two", "2026-01-02"),
        ("three", "2026-01-03"),
    ] {
        db.conn().execute("INSERT INTO reviewer_finding_outcomes(cycle_id,finding_id,reviewer_identity,outcome,raised_at) VALUES('cycle',?1,'reviewer','unresolved',?2)", [id, raised]).unwrap();
    }
    let before: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM worker_selections", [], |row| {
            row.get(0)
        })
        .unwrap();
    let reviews = ReviewRepository::new(db.conn());
    reviews
        .resolve_finding(
            "cycle",
            "one",
            FindingOutcome::Confirmed,
            "system:integration",
            "fixed",
            Some("abc123"),
        )
        .unwrap();
    reviews
        .resolve_finding(
            "cycle",
            "two",
            FindingOutcome::Waived,
            "human:owner",
            "accepted tradeoff",
            None,
        )
        .unwrap();
    reviews
        .resolve_finding(
            "cycle",
            "three",
            FindingOutcome::Invalid,
            "human:owner",
            "claim contradicted by evidence",
            None,
        )
        .unwrap();
    let calibration = reviews.reviewer_calibration("reviewer", 10).unwrap();
    assert_eq!(
        (
            calibration.raised,
            calibration.confirmed,
            calibration.waived,
            calibration.invalid,
            calibration.unresolved
        ),
        (3, 1, 1, 1, 0)
    );
    let policy = familiar_ai_core::config::ReviewConfig {
        reviewer_calibration_window: 10,
        reviewer_calibration_minimum_sample: 3,
        reviewer_invalid_rate_bps: 3000,
        ..Default::default()
    };
    let input = reviews
        .configured_reviewer_routing_input("reviewer", &policy)
        .unwrap();
    assert!(input.probation_recommended);
    let after: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM worker_selections", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, after, "calibration must not mutate routing");
}
