use familiar_ai_core::metrics::north_star::{
    validate_execution_outcome_closure, ExecutionOutcomeClosure,
};
use familiar_ai_daemon::cli::metrics::{check_document_claims, render, MetricsRequest};
use familiar_ai_storage::{DeliveryLedgerRepository, DriverRepository, LandingPath};

fn fixture() -> familiar_ai_storage::Database {
    let db = familiar_ai_storage::Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let repo = DriverRepository::new(db.conn());
    for (session, repository, prd, cost, started) in [
        (
            "previous",
            "familiar.git",
            "PRD-1",
            Some(1_000),
            "2026-08-10",
        ),
        ("current", "familiar.git", "PRD-2", Some(600), "2026-09-10"),
        (
            "spectra",
            "spectra.git",
            "PRD-9",
            Some(999_999),
            "2026-09-10",
        ),
    ] {
        repo.open_session(session, repository, "{}").unwrap();
        let seq = repo
            .record_attempt_started(session, prd, &format!("docs/prds/{prd}.md"), None)
            .unwrap();
        repo.record_attempt_finished(session, seq, "completed", None, cost, None)
            .unwrap();
        db.conn()
            .execute(
                "UPDATE driver_sessions SET started_at=?1 WHERE session_id=?2",
                [started, session],
            )
            .unwrap();
        db.conn()
            .execute(
                "UPDATE driver_attempts SET started_at=?1,ended_at=?1 WHERE session_id=?2",
                [started, session],
            )
            .unwrap();
        DeliveryLedgerRepository::new(db.conn())
            .mark_integrated(
                LandingPath::DriveMergeQueue {
                    session_id: session,
                    sequence: seq,
                },
                &format!("{session}-sha"),
            )
            .unwrap();
    }
    db
}

fn request() -> MetricsRequest {
    MetricsRequest {
        host: "linux".into(),
        repository_key: "familiar.git".into(),
        current_start: "2026-09-01".into(),
        current_end: "2026-10-01".into(),
        previous_start: "2026-08-01".into(),
        previous_end: "2026-09-01".into(),
        required_hosts: vec!["linux".into()],
    }
}

#[test]
fn renders_scoped_bounded_absolute_metrics_and_trends() {
    let output = render(&fixture(), &request()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["current"]["scope"]["repository_key"], "familiar.git");
    assert_eq!(
        value["current"]["scope"]["hosts"],
        serde_json::json!(["linux"])
    );
    assert_eq!(value["current"]["accepted_prds"], 1);
    assert_eq!(value["current"]["cost_per_accepted_prd_microusd"], 600.0);
    assert_eq!(value["cost_per_accepted_prd_change_microusd"], -400.0);
    assert_eq!(value["current"]["human_touches_per_accepted_prd"], 0.0);
    assert_eq!(value["human_touches_per_accepted_prd_change"], 0.0);
}

#[test]
fn refuses_project_scope_when_a_declared_host_ledger_is_missing() {
    let mut request = request();
    request.required_hosts.push("m1-mac".into());
    assert_eq!(
        render(&fixture(), &request).unwrap_err(),
        "project-wide metric is uncomputable; missing host ledger(s): m1-mac"
    );
}

#[test]
fn stale_documented_claim_is_caught_after_ledger_changes() {
    let db = fixture();
    let claim = r#"familiar-delivery-claim: {"repository_key":"familiar.git","hosts":["linux"],"window_start":"2026-09-01","window_end":"2026-10-01","accepted_prds":1,"unattended_prds":1,"measured_cost_executions":1,"total_cost_executions":1}"#;
    assert_eq!(check_document_claims(&db, "linux", claim).unwrap(), 1);
    db.conn()
        .execute(
            "UPDATE driver_attempts SET known_cost_microusd=NULL WHERE session_id='current'",
            [],
        )
        .unwrap();
    let error = check_document_claims(&db, "linux", claim).unwrap_err();
    assert!(error.contains("stale delivery claim"));
    assert!(error.contains("ledger=1/1/0/1"));
}

#[test]
fn execution_outcome_closure_requires_query_and_recorded_result() {
    let narrative = ExecutionOutcomeClosure {
        bug: "FAM-BUG-019".into(),
        query: None,
        recorded_result: Some("wave looked successful".into()),
    };
    assert!(validate_execution_outcome_closure(&narrative)
        .unwrap_err()
        .contains("narrative evidence is insufficient"));
    let supported = ExecutionOutcomeClosure {
        bug: "FAM-BUG-019".into(),
        query: Some("SELECT count(*) FROM driver_attempts WHERE integrated_at IS NOT NULL".into()),
        recorded_result: Some("1".into()),
    };
    validate_execution_outcome_closure(&supported).unwrap();
}
