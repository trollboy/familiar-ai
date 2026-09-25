use familiar_ai_core::config::{
    AgentAdapterKind, Config, LocalCostUnitConfig, LocalEndpointConfig, LocalRuntimeKind,
    LocalWorkerConfig, RegistryWorkerConfig, WorkerCapabilityConfig, WorkerCostBasisConfig,
    WorkerRegistryConfig,
};
use familiar_ai_daemon::cli::workers::{inventory, Blocker};

fn worker(model: &str) -> RegistryWorkerConfig {
    RegistryWorkerConfig {
        adapter: Some(AgentAdapterKind::Codex),
        provider: "openai".into(),
        model: model.into(),
        runtime: None,
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        local: None,
        executable: Some("codex".into()),
        capabilities: vec![WorkerCapabilityConfig::Implementation],
        fresh_process_isolation: true,
        context_tokens: 10_000,
        estimated_cost_microusd: Some(17),
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: vec![],
    }
}

fn config(worker: RegistryWorkerConfig, basis: Option<WorkerCostBasisConfig>) -> Config {
    let mut registry = WorkerRegistryConfig::default();
    registry.workers.insert("worker-a".into(), worker);
    if let Some(basis) = basis {
        registry.cost_bases.insert("worker-a".into(), basis);
    }
    Config {
        worker_registry: Some(registry),
        ..Config::default()
    }
}

#[test]
fn bare_legacy_estimate_is_unmeasured_and_actionable() {
    let row = inventory(&config(worker("gpt"), None)).remove(0);
    assert!(row.blockers.contains(&Blocker::CostUnmeasured));
    assert_eq!(
        row.cost_missing_input.as_deref(),
        Some("published rates, accepted-execution measurements, or operator declaration")
    );
    assert!(row.remediation.contains(
        &"familiar-ai config model cost-basis worker-a --estimate-microusd AMOUNT --actor ACTOR --reason REASON".to_owned()
    ));
}

#[test]
fn declared_estimate_carries_actor_and_reason() {
    let basis = WorkerCostBasisConfig::OperatorDeclared {
        estimate_microusd: 17,
        actor: "human:operator".into(),
        reason: "provider calculator".into(),
    };
    let row = inventory(&config(worker("gpt"), Some(basis))).remove(0);
    assert!(!row.blockers.contains(&Blocker::CostUnmeasured));
    assert_eq!(row.cost_basis.as_deref(), Some("operator-declared"));
}

#[test]
fn local_non_monetary_cost_requires_explicit_ordering_policy() {
    let mut local = worker("local-model");
    local.provider = "local".into();
    local.runtime = Some("ollama".into());
    local.adapter = None;
    local.estimated_cost_microusd = None;
    local.local = Some(LocalWorkerConfig {
        runtime_kind: LocalRuntimeKind::Ollama,
        endpoint: LocalEndpointConfig {
            base_url: "http://127.0.0.1:11434".into(),
            tls: false,
        },
        resources: Default::default(),
    });
    let unorderable = WorkerCostBasisConfig::Local {
        unit: LocalCostUnitConfig::LocalToken,
        amount: 2,
        monetary_ordering_policy: None,
        actor: "human:operator".into(),
        reason: "electricity excluded".into(),
    };
    let row = inventory(&config(local.clone(), Some(unorderable))).remove(0);
    assert!(row.blockers.contains(&Blocker::CostUnmeasured));
    assert_eq!(
        row.cost_missing_input.as_deref(),
        Some("monetary_ordering_policy")
    );
    assert_eq!(
        row.cost_microusd, None,
        "local cost must never be invented as zero USD"
    );

    let orderable = WorkerCostBasisConfig::Local {
        unit: LocalCostUnitConfig::LocalToken,
        amount: 2,
        monetary_ordering_policy: Some("local-before-hosted".into()),
        actor: "human:operator".into(),
        reason: "operator accepts local-token ordering".into(),
    };
    let row = inventory(&config(local, Some(orderable))).remove(0);
    assert!(!row.blockers.contains(&Blocker::CostUnmeasured));
    assert_eq!(row.cost_basis.as_deref(), Some("local"));
    assert_eq!(
        row.cost_microusd, None,
        "ordering policy must not manufacture USD"
    );
}

#[test]
fn route_inputs_remain_legacy_fields_not_cost_basis() {
    let worker = worker("gpt");
    let original = serde_json::to_vec(&(
        worker.estimated_cost_microusd,
        worker.available,
        &worker.capabilities,
    ))
    .unwrap();
    let configured = config(
        worker,
        Some(WorkerCostBasisConfig::PublishedTokenRates {
            estimate_microusd: 17,
            source: "provider-pricing".into(),
            effective_at: "2026-09-25T00:00:00Z".into(),
        }),
    );
    let current = &configured.worker_registry.unwrap().workers["worker-a"];
    assert_eq!(
        original,
        serde_json::to_vec(&(
            current.estimated_cost_microusd,
            current.available,
            &current.capabilities
        ))
        .unwrap()
    );
}

#[test]
fn cost_evidence_and_supersession_are_append_only() {
    let db = familiar_ai_storage::Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let repo = familiar_ai_storage::AccountingRepository::new(db.conn());
    repo.record_worker_cost_basis(
        "declared",
        "worker-a",
        "operator-declared",
        Some(17),
        "microUSD",
        r#"{"actor":"human:operator","reason":"quote"}"#,
    )
    .unwrap();
    repo.record_worker_cost_basis(
        "measured",
        "worker-a",
        "measured-accepted-executions",
        Some(12),
        "microUSD",
        r#"{"accepted":3,"unknown":1}"#,
    )
    .unwrap();
    let coverage = familiar_ai_storage::repos::accounting::WorkerCostCoverage {
        worker_identity: "worker-a".into(),
        accepted_executions: 3,
        known_cost_executions: 2,
        unknown_cost_executions: 1,
        measured_average_microusd: Some(12),
    };
    repo.record_cost_supersession("sup-1", "worker-a", "declared", "measured", &coverage)
        .unwrap();
    let stored: (u64,u64) = db.conn().query_row("SELECT accepted_executions,unknown_cost_executions FROM worker_cost_supersessions WHERE supersession_id='sup-1'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(stored, (3, 1));
    assert!(db
        .conn()
        .execute(
            "UPDATE worker_cost_bases SET estimate_microusd=0 WHERE basis_id='declared'",
            []
        )
        .is_err());
}

#[test]
fn accepted_measurements_automatically_supersede_and_count_unknown_costs() {
    let db = familiar_ai_storage::Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let repo = familiar_ai_storage::AccountingRepository::new(db.conn());
    repo.register_project(
        "prj_cost_measurement1",
        "fixture",
        "repository",
        "/machine/git/common",
        "test",
    )
    .unwrap();
    repo.record_worker_cost_basis(
        "declared",
        "worker-a",
        "operator-declared",
        Some(17),
        "microUSD",
        r#"{"actor":"human:operator","reason":"quote"}"#,
    )
    .unwrap();

    for index in 0..3 {
        let execution = format!("accepted-{index}");
        db.conn().execute("INSERT INTO execution_history(execution_id,started_at,ended_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,'2026-09-25T00:00:00Z','2026-09-25T00:00:01Z','worker-a','succeeded','repo','repo','docs/prds/PRD-086.md','[]')", [&execution]).unwrap();
        let hash = format!("sha256:accepted-{index}");
        let observation = repo
            .append_observation(&familiar_ai_storage::repos::accounting::UsageObservation {
                execution_id: &execution,
                attempt_id: &execution,
                stage: "implementation",
                session_id: Some("cost-session"),
                worker_identity: "worker-a",
                adapter: "codex",
                cli_version: None,
                model_identity: Some("gpt"),
                service_tier: None,
                provider_request_id: None,
                uncached_input_tokens: Some(10),
                cache_read_tokens: None,
                cache_write_tokens: None,
                output_tokens: Some(5),
                reasoning_output_tokens: None,
                unknown_reason: None,
                period_start: "2026-09-25T00:00:00Z",
                period_end: "2026-09-25T00:00:01Z",
                terminal_status: "succeeded",
                source_event_hash: &hash,
                provider_cost_lexical: None,
                project_resolution_evidence: Some("/machine/git/common"),
                output_register_id: "none",
                output_register_version: "none",
                input_compression_id: "none",
                input_compression_version: "none",
                compression_experiment: None,
                compression_lane: None,
                edit_form_id: "none",
                edit_form_version: "none",
                truncation_config_id: "none",
                truncation_config_version: "none",
            })
            .unwrap()
            .unwrap();
        if index < 2 {
            repo.append_vendor_estimate(&observation, "0.000012")
                .unwrap();
        }
    }

    let measured = repo
        .maybe_supersede_worker_cost("worker-a", "declared", 3)
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.maybe_supersede_worker_cost("worker-a", "declared", 3)
            .unwrap(),
        Some(measured.clone())
    );
    let stored: (String, u64, u64, u64) = db.conn().query_row("SELECT measured_basis_id,accepted_executions,known_cost_executions,unknown_cost_executions FROM worker_cost_supersessions WHERE declared_basis_id='declared'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(stored, (measured, 3, 2, 1));
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT count(*) FROM worker_cost_supersessions",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn routing_persists_unrankable_distinct_from_ranked_and_lost() {
    use familiar_ai_storage::repos::worker_selection::{
        WorkerSelectionRecord, WorkerSelectionRepository,
    };

    let db = familiar_ai_storage::Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let selections = WorkerSelectionRepository::new(db.conn());
    for (id, reason) in [
        ("selection-unrankable", "cost-could-not-rank"),
        ("selection-ranked", "cost-ranked-and-lost"),
    ] {
        selections
            .record(&WorkerSelectionRecord {
                selection_id: id,
                execution_id: None,
                stage: "implementation",
                rule: "lowest-cost-then-id",
                selected_identity: "selected-worker",
                selected_empirical_version: "selected-worker-v1",
                candidates_json: r#"[{"worker_id":"cheaper-worker"}]"#,
                risk_classes_json: "[]",
                expected_file_count: 1,
                cost_decision_reason: Some(reason),
            })
            .unwrap();
    }

    let stored: Vec<(String, String)> = {
        let mut statement = db
            .conn()
            .prepare("SELECT selection_id,cost_decision_reason FROM worker_selections ORDER BY selection_id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        stored,
        vec![
            ("selection-ranked".into(), "cost-ranked-and-lost".into()),
            ("selection-unrankable".into(), "cost-could-not-rank".into()),
        ]
    );
}
