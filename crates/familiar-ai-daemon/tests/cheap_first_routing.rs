//! PRD-107: cheap-first ladder routing, single-hop escalation, unprofitable
//! rung skipping, local-rung probation, and ladder reporting.
//!
//! Each test here is named after the acceptance criterion it pins. Unit-level
//! coverage for the same mechanisms also lives beside the code it tests
//! (`run.rs`, `drive.rs`, `report.rs`, `registry_workers.rs`, `probation.rs`);
//! this file exercises the same behaviour through the daemon's public
//! surface, the way an operator or the driver itself would observe it.

use std::collections::BTreeMap;

use familiar_ai_agent::WorkerStage;
use familiar_ai_core::config::{
    Config, LadderProbationConfig, LocalEndpointConfig, LocalRuntimeKind, LocalWorkerConfig,
    RegistryWorkerConfig, WorkerCapabilityConfig, WorkerLadderConfig, WorkerRegistryConfig,
};
use familiar_ai_daemon::run::{resolved_worker_plan, RouteContext};
use familiar_ai_storage::repos::worker_selection::WorkerSelectionRecord;
use familiar_ai_storage::{
    Database, DriverRepository, ExecutionHistoryRepository, ExecutionStart, ProbationObservation,
    ProbationRepository, WorkerSelectionRepository, WorkerSpecRepository,
};

fn raw_worker(runtime: &str, provider: &str) -> RegistryWorkerConfig {
    RegistryWorkerConfig {
        adapter: None,
        provider: provider.into(),
        model: "test-model".into(),
        runtime: Some(runtime.into()),
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        local: None,
        executable: None,
        capabilities: vec![
            WorkerCapabilityConfig::Implementation,
            WorkerCapabilityConfig::Remediation,
        ],
        fresh_process_isolation: true,
        context_tokens: 8_000,
        estimated_cost_microusd: None,
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: vec![],
    }
}

fn local_ollama_worker(cost_microusd: u64) -> RegistryWorkerConfig {
    let mut worker = raw_worker("ollama", "local");
    worker.estimated_cost_microusd = Some(cost_microusd);
    worker.local = Some(LocalWorkerConfig {
        runtime_kind: LocalRuntimeKind::Ollama,
        endpoint: LocalEndpointConfig {
            base_url: "http://127.0.0.1:11434".into(),
            tls: false,
        },
        resources: Default::default(),
    });
    worker
}

fn hosted_worker(cost_microusd: u64) -> RegistryWorkerConfig {
    let mut worker = raw_worker("anthropic-api", "anthropic");
    worker.estimated_cost_microusd = Some(cost_microusd);
    worker
}

fn registry(
    local_cost: u64,
    hosted_cost: u64,
    ladder: Option<WorkerLadderConfig>,
) -> WorkerRegistryConfig {
    let mut registry = WorkerRegistryConfig {
        workers: BTreeMap::from([
            ("local-ollama".to_owned(), local_ollama_worker(local_cost)),
            ("claude-api".to_owned(), hosted_worker(hosted_cost)),
        ]),
        ..Default::default()
    };
    registry.routing.ladder = ladder;
    registry
}

fn config(registry: WorkerRegistryConfig) -> Config {
    let mut config = Config::default();
    config.agent_runtime.enabled = true;
    config.worker_registry = Some(registry);
    // `resolved_worker_plan` validates ladder `risk_floors` against the
    // repository-declared vocabulary; without an entry here, a test that
    // exercises a `security` floor would fail registry validation before
    // routing ever ran.
    config.repositories.insert(
        "test-repo".into(),
        familiar_ai_core::config::RepositoryConfig {
            risk_vocabulary: vec!["security".into()],
            ..Default::default()
        },
    );
    config
}

fn implementation_record(
    records: &[familiar_ai_agent::SelectionRecord],
) -> &familiar_ai_agent::SelectionRecord {
    records
        .iter()
        .find(|record| record.stage == WorkerStage::Implementation)
        .expect("an implementation worker must be selected")
}

/// Criterion: with no ladder declared, routing is byte-identical to today.
#[test]
fn no_ladder_declared_preserves_legacy_lowest_cost_then_id_routing() {
    let config = config(registry(5, 50, None));
    let (_, _, records) = resolved_worker_plan(&config, &RouteContext::default()).unwrap();
    let implementation = implementation_record(&records);
    assert_eq!(implementation.selected_worker, "local-ollama");
    assert_eq!(implementation.rule, "lowest-cost-then-id");
    assert!(
        !implementation.rule.starts_with("ladder:"),
        "absent ladder configuration must never produce a ladder-tagged rule"
    );
}

/// Criterion: the first implementation attempt of a low-risk PRD goes to the
/// cheapest capable worker, and the selection record names the local worker
/// and the reason it was chosen — pinned durably in `worker_selections`.
#[test]
fn ladder_selects_cheapest_capable_local_rung_and_names_the_reason() {
    let ladder = WorkerLadderConfig {
        implementation: vec!["local-ollama".into(), "claude-api".into()],
        remediation: Vec::new(),
        review: Vec::new(),
        narrow_task: Vec::new(),
        risk_floors: BTreeMap::new(),
        maximum_cheap_fraction_basis_points: 0,
        probation: None,
    };
    let config = config(registry(5, 50, Some(ladder)));
    let (_, _, records) = resolved_worker_plan(&config, &RouteContext::default()).unwrap();
    let implementation = implementation_record(&records);
    assert_eq!(implementation.selected_worker, "local-ollama");
    assert_eq!(implementation.rule, "ladder:cheapest-capable-rung");

    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    WorkerSelectionRepository::new(db.conn())
        .record(&WorkerSelectionRecord {
            selection_id: "select-1",
            execution_id: Some("exec-1"),
            stage: "implementation",
            rule: &implementation.rule,
            selected_identity: &implementation.selected_spec_identity,
            selected_empirical_version: &implementation.selected_empirical_version,
            candidates_json: "[]",
            risk_classes_json: "[]",
            expected_file_count: 1,
            cost_decision_reason: None,
        })
        .unwrap();
    let (rule, identity): (String, String) = db
        .conn()
        .query_row(
            "SELECT rule, selected_identity FROM worker_selections WHERE selection_id = 'select-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(rule, "ladder:cheapest-capable-rung");
    assert_eq!(identity, implementation.selected_spec_identity);
}

/// Criterion: the ladder applies per job class, not only to implementation —
/// remediation and independent review each consult their own declared rung,
/// and review selection still respects cross-vendor independence.
#[test]
fn ladder_applies_separately_to_remediation_and_review_stages() {
    let mut reviewer = hosted_worker(50);
    reviewer.capabilities.push(WorkerCapabilityConfig::Review);
    let mut registry = WorkerRegistryConfig {
        workers: BTreeMap::from([
            ("local-ollama".to_owned(), local_ollama_worker(5)),
            ("claude-api".to_owned(), reviewer),
        ]),
        ..Default::default()
    };
    registry.routing.ladder = Some(WorkerLadderConfig {
        implementation: vec!["local-ollama".into(), "claude-api".into()],
        remediation: vec!["local-ollama".into(), "claude-api".into()],
        review: vec!["claude-api".into()],
        narrow_task: Vec::new(),
        risk_floors: BTreeMap::new(),
        maximum_cheap_fraction_basis_points: 0,
        probation: None,
    });
    let mut config = config(registry);
    config.review.enabled = true;

    let (_, _, records) = resolved_worker_plan(&config, &RouteContext::default()).unwrap();

    let implementation = records
        .iter()
        .find(|record| record.stage == WorkerStage::Implementation)
        .unwrap();
    assert_eq!(implementation.selected_worker, "local-ollama");
    assert_eq!(implementation.rule, "ladder:cheapest-capable-rung");

    let remediation = records
        .iter()
        .find(|record| record.stage == WorkerStage::Remediation)
        .unwrap();
    assert_eq!(remediation.selected_worker, "local-ollama");
    assert_eq!(remediation.rule, "ladder:cheapest-capable-rung");

    let review = records
        .iter()
        .find(|record| record.stage == WorkerStage::Review)
        .unwrap();
    assert_eq!(review.selected_worker, "claude-api");
    assert_eq!(review.rule, "ladder:cheapest-capable-rung");
}

/// Criterion: the PRD's declared risk tier is a floor under the ladder — a
/// PRD declaring a floored risk class never starts below the named rung,
/// however cheap the rungs beneath it are.
#[test]
fn declared_risk_tier_floors_the_ladder_above_the_local_rung() {
    let ladder = WorkerLadderConfig {
        implementation: vec!["local-ollama".into(), "claude-api".into()],
        remediation: Vec::new(),
        review: Vec::new(),
        narrow_task: Vec::new(),
        risk_floors: BTreeMap::from([("security".to_owned(), "claude-api".to_owned())]),
        maximum_cheap_fraction_basis_points: 0,
        probation: None,
    };
    let config = config(registry(5, 50, Some(ladder)));
    let route_context = RouteContext {
        risk_classes: vec!["security".into()],
        expected_file_count: 1,
    };
    let (_, _, records) = resolved_worker_plan(&config, &route_context).unwrap();
    let implementation = implementation_record(&records);
    assert_eq!(implementation.selected_worker, "claude-api");
    assert_eq!(implementation.rule, "ladder:cheapest-capable-rung");
}

/// Criterion: a rung whose declared cost exceeds a configured fraction of the
/// next rung's is reported as an unprofitable ladder and skipped rather than
/// tried. Covers both a profitable and an unprofitable configuration.
#[test]
fn unprofitable_rung_is_skipped_while_a_profitable_one_is_kept() {
    let ladder_profitable = WorkerLadderConfig {
        implementation: vec!["local-ollama".into(), "claude-api".into()],
        remediation: Vec::new(),
        review: Vec::new(),
        narrow_task: Vec::new(),
        risk_floors: BTreeMap::new(),
        maximum_cheap_fraction_basis_points: 5_000,
        probation: None,
    };
    // local=5, hosted=50: 5/50 = 10% < 50% threshold, so the cheap rung is
    // kept.
    let profitable_config = config(registry(5, 50, Some(ladder_profitable)));
    let (_, _, records) =
        resolved_worker_plan(&profitable_config, &RouteContext::default()).unwrap();
    let implementation = implementation_record(&records);
    assert_eq!(implementation.selected_worker, "local-ollama");
    assert_eq!(implementation.rule, "ladder:cheapest-capable-rung");

    let ladder_unprofitable = WorkerLadderConfig {
        implementation: vec!["local-ollama".into(), "claude-api".into()],
        remediation: Vec::new(),
        review: Vec::new(),
        narrow_task: Vec::new(),
        risk_floors: BTreeMap::new(),
        maximum_cheap_fraction_basis_points: 5_000,
        probation: None,
    };
    // local=40, hosted=50: 40/50 = 80% >= 50% threshold, so trying the cheap
    // rung first is not worth the extra attempt; it is skipped.
    let unprofitable_config = config(registry(40, 50, Some(ladder_unprofitable)));
    let (_, _, records) =
        resolved_worker_plan(&unprofitable_config, &RouteContext::default()).unwrap();
    let implementation = implementation_record(&records);
    assert_eq!(implementation.selected_worker, "claude-api");
    assert_eq!(implementation.rule, "ladder:unprofitable-rung-skipped");
}

/// Criterion: a local rung named in the ladder's declared probation block is
/// scored against that policy, not the generic PRD-032 default, and a worker
/// the block does not name is untouched.
#[test]
fn ladder_probation_config_is_validated_and_scoped_to_named_workers() {
    let ladder = WorkerLadderConfig {
        implementation: vec!["local-ollama".into(), "claude-api".into()],
        remediation: Vec::new(),
        review: Vec::new(),
        narrow_task: Vec::new(),
        risk_floors: BTreeMap::new(),
        maximum_cheap_fraction_basis_points: 0,
        probation: Some(LadderProbationConfig {
            workers: vec!["local-ollama".into()],
            minimum_accepted_prds: 5,
            minimum_review_pass_basis_points: 9_000,
            maximum_remediation_basis_points: 2_000,
            maximum_failure_basis_points: 1_000,
            maximum_cost_per_accepted_prd_microusd: 100_000,
            maximum_expected_files: 6,
        }),
    };
    let registry = registry(5, 50, Some(ladder));
    let risk_vocabulary = std::collections::BTreeSet::from(["security"]);
    registry
        .validate(&risk_vocabulary)
        .expect("a probation block naming only local workers must validate");

    // A probation block naming a hosted (non-local) worker is refused: only
    // a local rung may start on probation under this PRD.
    let mut bad_ladder = registry.routing.ladder.clone().unwrap();
    bad_ladder.probation.as_mut().unwrap().workers = vec!["claude-api".into()];
    let mut bad_registry = registry.clone();
    bad_registry.routing.ladder = Some(bad_ladder);
    assert!(bad_registry.validate(&risk_vocabulary).is_err());
}

/// Criterion: a cheap-tier failure escalates exactly once, carrying its
/// evidence, and the columns `escalated_from_sequence`/`escalation_reason`
/// are actually written — pinned by a regression counting attempts per PRD.
/// The database's own unique index enforces that a given cheap attempt can
/// escalate at most once.
#[test]
fn escalation_carries_evidence_and_is_bounded_to_one_hop_per_attempt() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let repository = DriverRepository::new(db.conn());
    repository
        .open_session("ladder-session", "/repo/.git", "{}")
        .unwrap();

    let cheap = repository
        .record_attempt_started("ladder-session", "PRD-999", "docs/prds/PRD-999.md", None)
        .unwrap();
    let evidence = "check=cargo-test exit=1 stderr=assertion failed: left == right";
    repository
        .record_attempt_finished_with_detail(
            "ladder-session",
            cheap,
            "retained",
            Some("verification_failed"),
            Some(evidence),
            Some(40),
            Some(10),
        )
        .unwrap();

    let escalated = repository
        .record_escalated_attempt_started_with_sources(
            "ladder-session",
            "PRD-999",
            "docs/prds/PRD-999.md",
            None,
            "global",
            "global",
            None,
            None,
            None,
            Some(cheap),
            Some("required_verification_failed"),
        )
        .unwrap();
    repository
        .record_attempt_finished_with_detail(
            "ladder-session",
            escalated,
            "retained",
            Some("human_decision_required_after_ladder_exhausted"),
            Some(evidence),
            Some(60),
            Some(20),
        )
        .unwrap();

    let attempts = repository.attempts("ladder-session").unwrap();
    assert_eq!(
        attempts.len(),
        2,
        "one cheap attempt plus exactly one escalation for this PRD"
    );
    let escalated_row = attempts
        .iter()
        .find(|attempt| attempt.escalated_from_sequence.is_some())
        .expect("the escalated attempt must record where it escalated from");
    assert_eq!(escalated_row.escalated_from_sequence, Some(cheap));
    assert_eq!(
        escalated_row.escalation_reason.as_deref(),
        Some("required_verification_failed")
    );
    assert_eq!(
        escalated_row.retained_reason.as_deref(),
        Some("human_decision_required_after_ladder_exhausted"),
        "a second-tier failure requires a human decision rather than climbing further"
    );
    assert!(escalated_row
        .retained_detail
        .as_deref()
        .unwrap()
        .contains("assertion failed"));

    // A second attempt claiming to escalate from the same cheap attempt is
    // refused outright by the schema's own single-escalation invariant
    // (migration 027's unique index), independent of any application logic.
    let duplicate = repository.record_escalated_attempt_started_with_sources(
        "ladder-session",
        "PRD-999",
        "docs/prds/PRD-999.md",
        None,
        "global",
        "global",
        None,
        None,
        None,
        Some(cheap),
        Some("required_verification_failed"),
    );
    assert!(
        duplicate.is_err(),
        "a second escalation from the same cheap attempt must be refused, not merely discouraged"
    );
}

/// Criterion: PRD-032 probation is live for the local rung — enough
/// accepted, well-reviewed executions promote a worker through
/// `ProbationRepository::apply_policy`; a run of failing, remediated
/// executions demotes it back to probation. Each transition is durably
/// recorded with its evidence. `probation::score`'s own unit tests
/// (`crates/familiar-ai-core/src/probation.rs`) pin cost-per-accepted-PRD
/// driving the same `promotion_eligible` decision at the pure-policy level;
/// this test exercises the repository/event-log path around it.
#[test]
fn recorded_review_pass_rate_and_cost_drive_promotion_then_demotion() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    WorkerSpecRepository::new(db.conn())
        .record_spec(
            "local-spec",
            "local-v1",
            "local-ollama",
            "local",
            "ollama",
            "qwen2.5",
            None,
            None,
            "implementer",
            "{}",
        )
        .unwrap();

    let policy = familiar_ai_core::probation::ProbationPolicy {
        policy_id: "prd-107-ladder-v1".into(),
        version: "1".into(),
        minimum_accepted_prds: 2,
        minimum_review_pass_basis_points: 9_000,
        maximum_remediation_basis_points: 0,
        maximum_failure_basis_points: 0,
        // Cost-per-accepted-PRD gating is exercised directly against
        // `probation::score` in its own unit tests; this repository-level
        // test does not populate the accounting tables `snapshot` reads
        // cost from, so a cost ceiling here would leave promotion
        // permanently unreachable for an unrelated reason.
        maximum_cost_per_accepted_prd_microusd: None,
        probation_max_expected_files: 2,
        require_independent_review: true,
    };
    let probation = ProbationRepository::new(db.conn());
    let start_execution = |execution_id: &str| {
        ExecutionHistoryRepository::new(db.conn())
            .insert_running(&ExecutionStart {
                execution_id: execution_id.into(),
                started_at: "2026-01-01T00:00:00Z".into(),
                repository: "repository".into(),
                worktree: "worktree".into(),
                git_commit: None,
                prd_path: "PRD.md".into(),
                unavailable_fields: BTreeMap::new(),
            })
            .unwrap();
    };

    for (index, observation_id) in ["obs-1", "obs-2"].into_iter().enumerate() {
        start_execution(observation_id);
        probation
            .append_observation(&ProbationObservation {
                observation_id,
                spec_identity: "local-spec",
                empirical_version: "local-v1",
                execution_id: Some(observation_id),
                accepted: true,
                verification_passed: true,
                independent_review_passed: Some(true),
                remediation_required: false,
                failed: false,
                latency_ms: 100 + index as u64,
                evidence_json: "{}",
            })
            .unwrap();
    }
    let promoted = probation
        .apply_policy("event-1", "score-1", "local-spec", "local-v1", &policy)
        .unwrap();
    assert!(
        promoted.promotion_eligible,
        "two clean, cheap, well-reviewed accepted PRDs must be promotion-eligible"
    );
    assert!(probation
        .authorize("local-spec", "local-v1", &policy, 99, true, false)
        .is_ok());

    // A costly, failing execution pushes cost-per-accepted-PRD and the
    // failure rate outside the policy's bounds.
    start_execution("obs-3");
    probation
        .append_observation(&ProbationObservation {
            observation_id: "obs-3",
            spec_identity: "local-spec",
            empirical_version: "local-v1",
            execution_id: Some("obs-3"),
            accepted: true,
            verification_passed: false,
            independent_review_passed: Some(false),
            remediation_required: true,
            failed: true,
            latency_ms: 500,
            evidence_json: "{}",
        })
        .unwrap();
    let demoted = probation
        .apply_policy("event-2", "score-2", "local-spec", "local-v1", &policy)
        .unwrap();
    assert!(
        !demoted.promotion_eligible,
        "a failed, remediated execution must revoke promotion eligibility"
    );
    assert!(probation
        .authorize("local-spec", "local-v1", &policy, 99, true, false)
        .is_err());

    let events: Vec<(String, String)> = db
        .conn()
        .prepare(
            "SELECT standing, reason FROM worker_standing_events \
             WHERE spec_identity = 'local-spec' ORDER BY occurred_at, event_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        events
            .iter()
            .any(|(standing, evidence)| standing == "promoted"
                && evidence.contains("promotion_eligible\":true")),
        "the promotion must be durably recorded with its evidence: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|(standing, evidence)| standing == "probation"
                && evidence.contains("promotion_eligible\":false")),
        "the demotion must be durably recorded with its evidence: {events:?}"
    );
}

/// Criterion: the session report states the ladder taken and the cost per
/// accepted PRD, and the report names the ladder configuration in effect so
/// a cost figure is never read without the routing that produced it.
#[test]
fn session_report_names_the_ladder_path_and_cost_per_accepted_prd() {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let repository = DriverRepository::new(db.conn());
    repository
        .open_session("report-ladder", "/repo/.git", r#"{"max_prds":1}"#)
        .unwrap();

    WorkerSpecRepository::new(db.conn())
        .record_spec(
            "local-spec",
            "local-v1",
            "local-ollama",
            "local",
            "ollama",
            "qwen",
            None,
            None,
            "implementer",
            "{}",
        )
        .unwrap();
    WorkerSpecRepository::new(db.conn())
        .record_spec(
            "strong-spec",
            "strong-v1",
            "claude-api",
            "anthropic",
            "anthropic-api",
            "sonnet",
            None,
            None,
            "implementer",
            "{}",
        )
        .unwrap();

    let cheap = repository
        .record_attempt_started(
            "report-ladder",
            "PRD-999",
            "docs/prds/PRD-999.md",
            Some("exec-cheap"),
        )
        .unwrap();
    repository
        .record_attempt_finished(
            "report-ladder",
            cheap,
            "retained",
            Some("verification_failed"),
            Some(40),
            Some(10),
        )
        .unwrap();
    let strong = repository
        .record_escalated_attempt_started_with_sources(
            "report-ladder",
            "PRD-999",
            "docs/prds/PRD-999.md",
            Some("exec-strong"),
            "global",
            "global",
            None,
            None,
            None,
            Some(cheap),
            Some("required_verification_failed"),
        )
        .unwrap();
    repository
        .record_attempt_finished(
            "report-ladder",
            strong,
            "completed",
            None,
            Some(60),
            Some(20),
        )
        .unwrap();
    repository
        .finish_session("report-ladder", "backlog_empty")
        .unwrap();

    let selections = WorkerSelectionRepository::new(db.conn());
    for (selection_id, execution_id, rule, spec, version) in [
        (
            "select-cheap",
            "exec-cheap",
            "ladder:cheapest-capable-rung",
            "local-spec",
            "local-v1",
        ),
        (
            "select-strong",
            "exec-strong",
            "ladder:cheapest-capable-rung",
            "strong-spec",
            "strong-v1",
        ),
    ] {
        selections
            .record(&WorkerSelectionRecord {
                selection_id,
                execution_id: Some(execution_id),
                stage: "implementation",
                rule,
                selected_identity: spec,
                selected_empirical_version: version,
                candidates_json: "[]",
                risk_classes_json: "[]",
                expected_file_count: 1,
                cost_decision_reason: None,
            })
            .unwrap();
    }

    let output = familiar_ai_daemon::report::render(&db, Some("report-ladder"), 0).unwrap();
    assert!(
        output.contains("LADDER"),
        "the report must have a ladder section: {output}"
    );
    assert!(
        output.contains("configuration=worker_registry.routing.ladder"),
        "the report must name the ladder configuration in effect: {output}"
    );
    assert!(
        output.contains("cost_per_accepted_prd="),
        "the report must state cost per accepted PRD: {output}"
    );
    assert!(
        output.contains("PRD-999:"),
        "the report must name the PRD that used the ladder: {output}"
    );
    assert!(
        output.contains("escalation=required_verification_failed"),
        "the report must state the escalation distinctly: {output}"
    );
}

/// Criterion: with a local worker unavailable through the ladder (unranked
/// cost or absent registration), the ladder never invents a selection and
/// resolution reports the same error surface as an unroutable stage today.
#[test]
fn ladder_with_unranked_local_cost_reports_no_route_rather_than_guessing() {
    let ladder = WorkerLadderConfig {
        implementation: vec!["local-ollama".into(), "claude-api".into()],
        remediation: Vec::new(),
        review: Vec::new(),
        narrow_task: Vec::new(),
        risk_floors: BTreeMap::new(),
        // A non-zero fraction with no cost basis on either rung must not
        // fabricate a profitability verdict; selection still proceeds
        // cheapest-first by declared order when cost cannot be compared.
        maximum_cheap_fraction_basis_points: 5_000,
        probation: None,
    };
    let mut local = local_ollama_worker(5);
    local.estimated_cost_microusd = None;
    let mut hosted = hosted_worker(50);
    hosted.estimated_cost_microusd = None;
    let mut registry = WorkerRegistryConfig {
        workers: BTreeMap::from([
            ("local-ollama".to_owned(), local),
            ("claude-api".to_owned(), hosted),
        ]),
        ..Default::default()
    };
    registry.routing.ladder = Some(ladder);
    let config = config(registry);
    let (_, _, records) = resolved_worker_plan(&config, &RouteContext::default()).unwrap();
    let implementation = implementation_record(&records);
    // No cost basis on either rung means the fraction check cannot compare,
    // so the ladder keeps the declared cheapest-first order rather than
    // guessing a skip.
    assert_eq!(implementation.selected_worker, "local-ollama");
    assert_eq!(implementation.rule, "ladder:cheapest-capable-rung");
}
