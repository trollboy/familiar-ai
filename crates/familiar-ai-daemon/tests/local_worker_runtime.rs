//! PRD-063 daemon-side integration coverage: the local worker adapter
//! driven through the real PRD-058 loop against a `wiremock` loopback fake
//! server, with PRD-064 reservation acquisition/resolution and PRD-051
//! telemetry persistence wired through the ordinary
//! `familiar_ai_daemon::local_worker_runtime` functions. No test performs,
//! or could perform, real model execution or any network beyond the
//! loopback fake.

use familiar_ai_agent::local_worker::{
    LocalAuthToken, LocalChatConfig, LocalInferenceAdapter, LocalRuntimeKind,
};
use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, ExecutionError, ExecutionOutcome,
    InMemoryToolJournal, LoopCeilings, LoopConfig, StablePrefix, StopReason, ToolExecutor,
    ValidatedCall, VolatileTask,
};
use familiar_ai_core::config::{
    LocalEndpointConfig, LocalResourceProfileConfig, LocalRuntimeKind as ConfigLocalRuntimeKind,
    LocalWorkerConfig,
};
use familiar_ai_core::{GrantMode, ReservationOwnerIdentity, ResourceType};
use familiar_ai_daemon::local_worker_runtime::{
    acquire_with_unknown_capacity_policy, artifact_verification_state_for,
    define_pools_from_resource_profile, execution_resource_requests,
    execution_resource_requests_for_profile, persist_local_telemetry, resolve_reservation,
    LocalRunMeasurements, UnknownCapacityPolicy,
};
use familiar_ai_llm::attempt::AttemptId;
use familiar_ai_storage::repos::local_telemetry::{
    LocalArtifactVerificationState, LocalTelemetryRepository,
};
use familiar_ai_storage::repos::reservation::{
    AcquireOutcome, ReservationRepository, SettlementObservation,
};
use familiar_ai_storage::Database;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn setup_execution(db: &Database, execution_id: &str) {
    db.conn()
        .execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,?2,'local-worker','running','repo','wt','docs/prds/PRD-063.md','[]')",
            rusqlite::params![execution_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
}

fn owner(execution_id: &str) -> ReservationOwnerIdentity {
    ReservationOwnerIdentity {
        owner_instance_id: format!("{execution_id}:owner"),
        installation_id: None,
        nonce_or_generation: execution_id.to_string(),
        owner_kind: "local-worker".into(),
        project_id: "project-1".into(),
        execution_id: execution_id.to_string(),
        component_id: "component-1".into(),
    }
}

fn attempt_id_source() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn base_config() -> LoopConfig {
    LoopConfig {
        worker_spec_identity: "wspec-sha256:local-daemon-test".into(),
        worker_empirical_version: "wver-sha256:local-daemon-test".into(),
        model: "llama3".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings {
            max_iterations: 10,
            max_output_tokens: None,
            max_wall_clock_ms: None,
        },
        offered_capabilities: vec![CapabilityId::ReadFile],
        structured_output: None,
        authority: AuthorityContext {
            project_id: "project-1".into(),
            execution_id: "exec_1".into(),
            attempt_id: "attempt_1".into(),
            worker_id: "worker_1".into(),
        },
    }
}

#[derive(Default)]
struct NoopExecutor;
impl ToolExecutor for NoopExecutor {
    fn execute(
        &mut self,
        call: &ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        Ok(ExecutionOutcome {
            result_text: "ok".into(),
            result_hash: format!("hash-{}", call.call_id),
        })
    }
}

struct AllowAll;
impl familiar_ai_agent::raw_runtime::ToolAuthorizer for AllowAll {
    fn authorize(
        &self,
        _call: &ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> familiar_ai_agent::raw_runtime::AuthorizationDecision {
        familiar_ai_agent::raw_runtime::AuthorizationDecision::Authorized
    }
}

fn sse(frames: &[Value]) -> String {
    let mut body: String = frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    body
}

async fn mount_sse(server: &MockServer, frames: &[Value]) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse(frames), "text/event-stream"))
        .mount(server)
        .await;
}

async fn mount_tags(server: &MockServer, model: &str, digest: &str) {
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"name": model, "digest": digest}]
        })))
        .mount(server)
        .await;
}

/// End-to-end: derive PRD-064 pool capacity and the `InferenceAdapter`
/// straight from a worker's registered PRD-063 `[..local]` profile (never
/// hand-picked positionally), verify the endpoint's claimed artifact, run
/// the real loop against a fake endpoint, commit the reservation with
/// observed consumption, and persist honest telemetry with no invented
/// cost.
#[tokio::test]
async fn full_pipeline_reserves_runs_commits_and_records_telemetry() {
    let server = MockServer::start().await;
    mount_sse(
        &server,
        &[
            json!({"choices": [{"delta": {"content": "done"}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 12, "completion_tokens": 4}}),
        ],
    )
    .await;
    mount_tags(&server, "llama3", "sha256:aaaa").await;

    let mut db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");

    let worker_config = LocalWorkerConfig {
        runtime_kind: ConfigLocalRuntimeKind::Ollama,
        endpoint: LocalEndpointConfig {
            base_url: server.uri(),
            tls: false,
        },
        resources: LocalResourceProfileConfig {
            accelerator_memory_mb: Some(4096),
            concurrent_inference_slots: Some(1),
            ..Default::default()
        },
    };

    let requests =
        execution_resource_requests_for_profile("local:ollama:llama3", &worker_config.resources);
    let grant = {
        let mut repo = ReservationRepository::new(db.conn_mut());
        define_pools_from_resource_profile(
            &mut repo,
            "local:ollama:llama3",
            &worker_config.resources,
        )
        .unwrap();
        // The pool's total hardware capacity (how much accelerator memory
        // this machine has) is an operator-level fact distinct from this
        // worker's own declared footprint request
        // (`accelerator_memory_mb`) — the resource profile supplies the
        // latter, not the former, so the pool itself stays hand-defined
        // here.
        repo.define_pool(
            "local:ollama:llama3:accelerator-memory",
            &ResourceType::AcceleratorMemory,
            8192,
            false,
        )
        .unwrap();
        match acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("exec_1"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap()
        {
            AcquireOutcome::Granted(grant) => grant,
            AcquireOutcome::Refused { unavailable } => {
                panic!("expected a grant, refused: {unavailable:?}")
            }
        }
    };

    let adapter =
        LocalInferenceAdapter::from_registry_config(&worker_config, LocalAuthToken::new(None))
            .unwrap();
    let verification = adapter.verify_artifact("llama3", Some("sha256:aaaa")).await;
    assert_eq!(
        artifact_verification_state_for(&verification),
        LocalArtifactVerificationState::Verified
    );

    let mut executor = NoopExecutor;
    let authorizer = AllowAll;
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "ctx".into(),
            version: "v1".into(),
        },
        &VolatileTask {
            bytes: "task".into(),
        },
        &base_config(),
        attempt_id_source(),
    )
    .await;
    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );

    let settlement = {
        let mut repo = ReservationRepository::new(db.conn_mut());
        resolve_reservation(
            &mut repo,
            &grant.reservation_id,
            outcome.stop_reason,
            requests,
            "test",
        )
        .unwrap()
        .unwrap()
    };
    assert_eq!(settlement.state, "committed");
    assert!(!settlement.unknown_consumption);

    let telemetry_repo = LocalTelemetryRepository::new(db.conn());
    let telemetry_id = persist_local_telemetry(
        &telemetry_repo,
        "exec_1",
        "implementation",
        "local-ollama-llama3",
        "ollama",
        Some("sha256:aaaa"),
        artifact_verification_state_for(&verification),
        &LocalRunMeasurements {
            wall_time_ms: Some(500),
            ..Default::default()
        },
        &outcome,
        0,
    )
    .unwrap();
    let rows = telemetry_repo.telemetry_for_execution("exec_1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, telemetry_id);
}

/// Mid-run memory pressure / unexpected external contention: settlement
/// with an observation larger than the granted amount is recorded as an
/// honest overrun, never masked as a plain worker error.
#[tokio::test]
async fn mid_run_memory_pressure_is_recorded_as_honest_overrun() {
    let mut db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let mut repo = ReservationRepository::new(db.conn_mut());
    repo.define_pool(
        "local:ollama:llama3:accelerator-memory",
        &ResourceType::AcceleratorMemory,
        8192,
        false,
    )
    .unwrap();
    let requests = vec![familiar_ai_core::ResourceRequest {
        pool_id: "local:ollama:llama3:accelerator-memory".into(),
        resource_type: ResourceType::AcceleratorMemory,
        amount: 4096,
    }];
    let AcquireOutcome::Granted(grant) = repo
        .acquire(&owner("exec_1"), &requests, GrantMode::AllOrNothing, None)
        .unwrap()
    else {
        panic!("expected grant");
    };
    // Observed consumption during the run exceeded the reservation —
    // unexpected pressure, not a masked worker failure.
    let settled = repo
        .settle(
            &grant.reservation_id,
            SettlementObservation::Known(vec![familiar_ai_core::ResourceRequest {
                pool_id: "local:ollama:llama3:accelerator-memory".into(),
                resource_type: ResourceType::AcceleratorMemory,
                amount: 6000,
            }]),
            "pressure-monitor",
        )
        .unwrap();
    assert!(settled.overrun);
}

/// Endpoint disappearance mid-run: the loop reports a retryable provider
/// failure and records zero fabricated attempts; the reservation is held,
/// never optimistically released, since consumption is unknown.
#[tokio::test]
async fn endpoint_disappearance_holds_reservation_and_records_no_fabricated_attempt() {
    let mut db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    setup_execution(&db, "exec_1");
    let requests = execution_resource_requests("local:ollama:llama3", None, None, false);
    let grant = {
        let mut repo = ReservationRepository::new(db.conn_mut());
        repo.define_pool(
            "local:ollama:llama3:inference-slots",
            &ResourceType::InferenceSlots,
            1,
            false,
        )
        .unwrap();
        match acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("exec_1"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap()
        {
            AcquireOutcome::Granted(grant) => grant,
            AcquireOutcome::Refused { .. } => panic!("expected grant"),
        }
    };

    let adapter = LocalInferenceAdapter::new(
        LocalRuntimeKind::Ollama,
        LocalAuthToken::new(None),
        LocalChatConfig {
            base_url: "http://127.0.0.1:1".into(),
            request_timeout_secs: 2,
        },
    )
    .unwrap();
    let mut executor = NoopExecutor;
    let authorizer = AllowAll;
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &StablePrefix {
            bytes: "ctx".into(),
            version: "v1".into(),
        },
        &VolatileTask {
            bytes: "task".into(),
        },
        &base_config(),
        attempt_id_source(),
    )
    .await;
    assert!(outcome.attempts.is_empty());

    let mut repo = ReservationRepository::new(db.conn_mut());
    let settlement = resolve_reservation(
        &mut repo,
        &grant.reservation_id,
        outcome.stop_reason,
        vec![],
        "test",
    )
    .unwrap()
    .unwrap();
    assert_eq!(settlement.state, "held");
    assert!(settlement.unknown_consumption);
}

/// Reads a pool's configured capacity straight from storage. There is no
/// public accessor, and the assertion this test needs is precisely that the
/// operator's number survived.
fn pool_capacity(db: &Database, pool_id: &str, resource_type: &ResourceType) -> i64 {
    db.conn()
        .query_row(
            "SELECT capacity FROM resource_pools WHERE pool_id=?1 AND resource_type=?2",
            rusqlite::params![pool_id, resource_type.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

/// N1: `acquire_with_unknown_capacity_policy` documents itself as applying
/// the policy "to any pool Familiar has never observed". It cannot actually
/// tell that case apart from a pool that exists and merely cannot satisfy
/// this request — `acquire` reports both as `Refused { unavailable }` — so
/// `SerializeConservatively` calls `define_pool(.., 1, false)` on pools the
/// operator configured.
///
/// `define_pool` upserts under a guard
/// (`available + (new_capacity - old_capacity) >= 0`), which blocks the
/// rewrite only while the pool is genuinely busy. A pool that is *mostly
/// free* and refuses merely because one request exceeds it passes the guard
/// and is silently rewritten: `concurrent_inference_slots = 4` becomes
/// capacity 1, permanently, and Familiar's record of the machine no longer
/// matches what the operator configured.
#[test]
fn conservative_bootstrap_never_rewrites_an_operator_configured_pool() {
    let mut db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();

    let profile = LocalResourceProfileConfig {
        concurrent_inference_slots: Some(4),
        ..Default::default()
    };
    // One oversized request against the operator's own pool: five slots on a
    // machine configured for four. Nothing is outstanding, so this is
    // refusal by size, not by contention.
    let requests = vec![familiar_ai_core::ResourceRequest {
        pool_id: "local:ollama:llama3:inference-slots".into(),
        resource_type: ResourceType::InferenceSlots,
        amount: 5,
    }];

    {
        let mut repo = ReservationRepository::new(db.conn_mut());
        define_pools_from_resource_profile(&mut repo, "local:ollama:llama3", &profile).unwrap();
        let outcome = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("exec_oversized"),
            &requests,
            UnknownCapacityPolicy::SerializeConservatively,
        )
        .unwrap();
        // Five slots never fit in four; the policy must not invent a way.
        assert!(
            matches!(outcome, AcquireOutcome::Refused { .. }),
            "an oversized request must stay refused, got {outcome:?}"
        );
    }

    assert_eq!(
        pool_capacity(
            &db,
            "local:ollama:llama3:inference-slots",
            &ResourceType::InferenceSlots
        ),
        4,
        "the operator's configured capacity was rewritten by the conservative \
         bootstrap; it may only ever define a pool Familiar has never observed"
    );
}

/// The other half of the contract, so a fix cannot simply stop bootstrapping.
/// A pool that genuinely has no row must still get exactly one conservative
/// occupant — never a larger invented number.
#[test]
fn conservative_bootstrap_still_defines_a_never_observed_pool() {
    let mut db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();

    // No define_pool call ever ran for this resource.
    let requests = vec![familiar_ai_core::ResourceRequest {
        pool_id: "local:ollama:llama3:inference-slots".into(),
        resource_type: ResourceType::InferenceSlots,
        amount: 1,
    }];

    {
        let mut repo = ReservationRepository::new(db.conn_mut());
        let outcome = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("exec_unknown"),
            &requests,
            UnknownCapacityPolicy::SerializeConservatively,
        )
        .unwrap();
        assert!(
            matches!(outcome, AcquireOutcome::Granted(_)),
            "an unobserved pool must be bootstrapped to one occupant, got {outcome:?}"
        );
    }

    assert_eq!(
        pool_capacity(
            &db,
            "local:ollama:llama3:inference-slots",
            &ResourceType::InferenceSlots
        ),
        1,
        "an unobserved pool must be bootstrapped to exactly one occupant"
    );
}

// ---------------------------------------------------------------------
// Production dispatch is not wired yet (see this crate's
// `local_worker_runtime` module docs): a `provider = "local"` worker's
// `runtime` (e.g. `"ollama"`) can collide with an unrelated pre-existing
// CLI-driven adapter id of the same name. `run::resolved_worker_plan` must
// fail closed rather than silently building the wrong, unverified,
// unreserved, untelemetered CLI-driven agent in its place.
// ---------------------------------------------------------------------

#[test]
fn worker_selection_refuses_a_local_profile_worker_instead_of_misdispatching_it() {
    use familiar_ai_core::config::{
        LocalEndpointConfig, LocalRuntimeKind as ConfigLocalRuntimeKind, LocalWorkerConfig,
        RegistryWorkerConfig, WorkerCapabilityConfig, WorkerRegistryConfig, LOCAL_PROVIDER,
    };
    use familiar_ai_core::Config;
    use familiar_ai_daemon::run::{resolved_worker_plan, RouteContext};

    let local_worker = RegistryWorkerConfig {
        adapter: None,
        provider: LOCAL_PROVIDER.into(),
        model: "llama3".into(),
        runtime: Some("ollama".into()),
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        local: Some(LocalWorkerConfig {
            runtime_kind: ConfigLocalRuntimeKind::Ollama,
            endpoint: LocalEndpointConfig {
                base_url: "http://127.0.0.1:11434".into(),
                tls: false,
            },
            resources: Default::default(),
        }),
        executable: None,
        capabilities: vec![
            WorkerCapabilityConfig::Implementation,
            WorkerCapabilityConfig::Remediation,
        ],
        fresh_process_isolation: true,
        context_tokens: 32_000,
        estimated_cost_microusd: Some(1),
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: Vec::new(),
    };

    let mut registry = WorkerRegistryConfig::default();
    registry
        .workers
        .insert("llama3-ollama".into(), local_worker);
    let config = Config {
        worker_registry: Some(registry),
        ..Config::default()
    };

    let error = resolved_worker_plan(&config, &RouteContext::default())
        .expect_err("a local-profile worker must never be silently dispatched");
    assert!(
        error.contains("llama3-ollama") && error.contains("local"),
        "error must name the offending worker and explain the local-dispatch gap, got: {error}"
    );
}
