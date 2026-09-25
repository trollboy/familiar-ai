//! PRD-073 warm local model residency regressions.
//!
//! The four properties the PRD names are covered here directly: one model
//! load across consecutive calls, LRU eviction at a ceiling, bounded
//! restarts ending in a loud failure, and an off-by-default routing
//! identity. Alongside them sit the rules that make residency honest —
//! residency never loads an unregistered artifact, disabling one stops the
//! server and records the stop, and every local execution's ledger row
//! carries whether it ran warm and which server served it.
//!
//! No test launches a real serving runtime. Residency's guarantees are
//! properties of the manager, so the serving process is a fake launcher
//! whose loads, stops and probe failures are directly observable; the one
//! end-to-end test that needs real reported usage drives the real PRD-058
//! loop against a `wiremock` loopback endpoint.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use familiar_ai_agent::local_worker::{LocalAuthToken, LocalInferenceAdapter};
use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, ExecutionError, ExecutionOutcome,
    InMemoryToolJournal, LoopCeilings, LoopConfig, StablePrefix, StopReason, ToolExecutor,
    ValidatedCall, VolatileTask,
};
use familiar_ai_core::config::{
    ArtifactManifest, ArtifactVerificationState, LocalEndpointConfig, LocalRuntimeKind,
    LocalWorkerConfig, ModelArtifactConfig, ModelArtifactProvenance, ModelResidencyConfig,
    RegistryWorkerConfig, ResidentModelConfig, WorkerCapabilityConfig, WorkerRegistryConfig,
    ARTIFACT_DIGEST_ALGORITHM, ARTIFACT_MANIFEST_SCHEMA, LOCAL_PROVIDER,
};
use familiar_ai_daemon::local_worker_runtime::{
    attach_observation_residency, persist_local_telemetry, LocalResidencyAttribution,
    LocalRunMeasurements,
};
use familiar_ai_daemon::model_residency::{
    reasons, ProcessResidentLauncher, ResidencyManager, ResidencyOutcome, ResidentLaunchSpec,
    ResidentServerLauncher,
};
use familiar_ai_llm::attempt::AttemptId;
use familiar_ai_llm::residency::{CacheEvidence, ResidencyState, NO_CACHE_ACCOUNTING_REASON};
use familiar_ai_storage::repos::local_telemetry::{
    LocalArtifactVerificationState, LocalTelemetryRepository,
};
use familiar_ai_storage::repos::model_residency::{ModelResidencyRepository, ResidencyEvent};
use familiar_ai_storage::{AccountingRepository, Database, ModelArtifactRepository};
use serde_json::{json, Value};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------
// Fake serving runtime
// ---------------------------------------------------------------------

#[derive(Default)]
struct FakeState {
    /// Server identities launched, in order. Its length is the number of
    /// model loads actually paid for.
    launched: Vec<String>,
    stopped: Vec<String>,
    running: BTreeSet<String>,
    /// Server identities whose probe fails: a process that died or hung.
    unhealthy: BTreeSet<String>,
    /// When set, every launch attempt fails with this message.
    launch_error: Option<String>,
}

#[derive(Default)]
struct FakeLauncher {
    state: Mutex<FakeState>,
}

impl FakeLauncher {
    fn launch_count(&self) -> usize {
        self.state.lock().unwrap().launched.len()
    }

    fn stopped(&self) -> Vec<String> {
        self.state.lock().unwrap().stopped.clone()
    }

    fn running(&self) -> BTreeSet<String> {
        self.state.lock().unwrap().running.clone()
    }

    /// Simulates a resident that died or hung: its probe now fails while
    /// nothing else changes.
    fn break_server(&self, server_identity: &str) {
        self.state
            .lock()
            .unwrap()
            .unhealthy
            .insert(server_identity.to_string());
    }

    fn fail_launches(&self, message: &str) {
        self.state.lock().unwrap().launch_error = Some(message.to_string());
    }
}

impl ResidentServerLauncher for FakeLauncher {
    fn launch(&self, _spec: &ResidentLaunchSpec, server_identity: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.launch_error.clone() {
            return Err(error);
        }
        state.launched.push(server_identity.to_string());
        state.running.insert(server_identity.to_string());
        Ok(())
    }

    fn stop(&self, server_identity: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.stopped.push(server_identity.to_string());
        state.running.remove(server_identity);
        Ok(())
    }

    fn probe(&self, server_identity: &str, _base_url: &str) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        if !state.running.contains(server_identity) {
            return Err("resident process is not running".into());
        }
        if state.unhealthy.contains(server_identity) {
            return Err("resident endpoint stopped answering".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

const ARTIFACT_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ARTIFACT_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const ARTIFACT_C: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn database() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    db
}

fn artifact(id: &str) -> ModelArtifactConfig {
    ModelArtifactConfig {
        id: id.into(),
        state: ArtifactVerificationState::Verified,
        runtime_alias: None,
        manifest: Some(ArtifactManifest {
            schema_version: ARTIFACT_MANIFEST_SCHEMA,
            digest_algorithm: ARTIFACT_DIGEST_ALGORITHM.into(),
            files: vec![],
            base_artifact: None,
            adapters: vec![],
            merged: false,
            identity: Default::default(),
        }),
        provenance: ModelArtifactProvenance::default(),
    }
}

fn register_artifact(db: &Database, alias: &str, id: &str) {
    ModelArtifactRepository::new(db)
        .register(alias, &artifact(id))
        .unwrap();
}

fn local_worker(artifact_id: &str, base_url: &str) -> RegistryWorkerConfig {
    RegistryWorkerConfig {
        adapter: None,
        provider: LOCAL_PROVIDER.into(),
        model: "llama3".into(),
        runtime: Some("ollama".into()),
        model_artifact: Some(artifact_id.into()),
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        local: Some(LocalWorkerConfig {
            runtime_kind: LocalRuntimeKind::Ollama,
            endpoint: LocalEndpointConfig {
                base_url: base_url.into(),
                tls: false,
            },
            resources: Default::default(),
        }),
        executable: None,
        capabilities: vec![WorkerCapabilityConfig::Implementation],
        fresh_process_isolation: true,
        context_tokens: 0,
        estimated_cost_microusd: None,
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: Vec::new(),
    }
}

fn registry(workers: &[(&str, &str, &str)]) -> WorkerRegistryConfig {
    let mut registry = WorkerRegistryConfig::default();
    for (worker_id, artifact_id, base_url) in workers {
        registry
            .workers
            .insert((*worker_id).into(), local_worker(artifact_id, base_url));
    }
    registry
}

fn resident(worker: &str, memory_mb: Option<u64>) -> ResidentModelConfig {
    ResidentModelConfig {
        enabled: true,
        worker: worker.into(),
        launch: vec!["fake-serve".into(), "--model".into(), worker.into()],
        memory_mb,
        ready_timeout_secs: 1,
    }
}

fn residency_config(
    max_residents: u32,
    memory_ceiling_mb: Option<u64>,
    max_restarts: u32,
    residents: &[(&str, &str, Option<u64>)],
) -> ModelResidencyConfig {
    let mut entries = BTreeMap::new();
    for (key, worker, memory_mb) in residents {
        entries.insert((*key).into(), resident(worker, *memory_mb));
    }
    ModelResidencyConfig {
        enabled: true,
        max_residents,
        memory_ceiling_mb,
        health_interval_secs: 1,
        max_restarts,
        residents: entries,
    }
}

fn manager(config: ModelResidencyConfig) -> (ResidencyManager, Arc<FakeLauncher>) {
    let launcher = Arc::new(FakeLauncher::default());
    (ResidencyManager::new(config, launcher.clone()), launcher)
}

fn events(db: &Database, resident_key: &str) -> Vec<(ResidencyEvent, Option<String>, Option<u32>)> {
    ModelResidencyRepository::new(db.conn())
        .events_for(resident_key)
        .unwrap()
        .into_iter()
        .map(|event| (event.event, event.reason, event.restart_attempt))
        .collect()
}

fn current_server(manager: &ResidencyManager, worker_identity: &str) -> String {
    manager
        .directory()
        .get(worker_identity)
        .expect("a resident endpoint")
        .server_identity
        .clone()
}

// ---------------------------------------------------------------------
// 1. One load across consecutive calls, spanning drive sessions
// ---------------------------------------------------------------------

/// The whole point of residency: two consecutive local calls against one
/// resident model pay for exactly one model load, proven by the recorded
/// load events rather than by timing.
#[test]
fn two_consecutive_calls_against_one_resident_incur_one_model_load() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        2,
        &[("llama3", "llama3-ollama", None)],
    ));

    let first = manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    let second = manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();

    assert!(
        matches!(first, ResidencyOutcome::Cold { .. }),
        "the first call loads the model: {first:?}"
    );
    assert!(
        matches!(second, ResidencyOutcome::Warm { .. }),
        "the second call reuses the loaded server: {second:?}"
    );
    assert_eq!(first.residency_state(), ResidencyState::Cold);
    assert_eq!(second.residency_state(), ResidencyState::Warm);
    // Both calls were served by the same process.
    assert_eq!(first.server_identity(), second.server_identity());

    assert_eq!(launcher.launch_count(), 1);
    assert_eq!(
        ModelResidencyRepository::new(db.conn())
            .load_count("llama3")
            .unwrap(),
        1,
        "one recorded load event, not one per call"
    );
}

/// Residency spans drive sessions for the daemon's lifetime: the manager
/// outlives any single session, so a later session's call is still warm and
/// still pays no second load.
#[test]
fn residency_survives_between_drive_sessions_while_the_daemon_runs() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        2,
        &[("llama3", "llama3-ollama", None)],
    ));

    // Drive session one.
    for _ in 0..2 {
        manager
            .ensure_resident(&db, Some(&registry), "llama3")
            .unwrap();
    }
    // The session ends; the daemon (and therefore the manager) does not.
    // A health interval passes between sessions.
    manager.health_tick(&db).unwrap();

    // Drive session two.
    let outcome = manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    assert!(
        matches!(outcome, ResidencyOutcome::Warm { .. }),
        "a later drive session still finds the model resident: {outcome:?}"
    );
    assert_eq!(launcher.launch_count(), 1);
    assert_eq!(
        ModelResidencyRepository::new(db.conn())
            .load_count("llama3")
            .unwrap(),
        1
    );
}

// ---------------------------------------------------------------------
// 2. Budgets and LRU eviction
// ---------------------------------------------------------------------

/// At the count ceiling the least recently used resident is evicted, its
/// server actually stopped, and the eviction recorded with a named reason.
#[test]
fn at_the_count_ceiling_the_least_recently_used_resident_is_evicted_with_a_record() {
    let db = database();
    register_artifact(&db, "a", ARTIFACT_A);
    register_artifact(&db, "b", ARTIFACT_B);
    register_artifact(&db, "c", ARTIFACT_C);
    let registry = registry(&[
        ("worker-a", ARTIFACT_A, "http://127.0.0.1:11434"),
        ("worker-b", ARTIFACT_B, "http://127.0.0.1:11435"),
        ("worker-c", ARTIFACT_C, "http://127.0.0.1:11436"),
    ]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        2,
        &[
            ("a", "worker-a", None),
            ("b", "worker-b", None),
            ("c", "worker-c", None),
        ],
    ));

    manager.ensure_resident(&db, Some(&registry), "a").unwrap();
    manager.ensure_resident(&db, Some(&registry), "b").unwrap();
    // Touching `a` makes `b` the least recently used one.
    let warm = manager.ensure_resident(&db, Some(&registry), "a").unwrap();
    assert!(matches!(warm, ResidencyOutcome::Warm { .. }));
    let server_b = current_server(&manager, "worker-b");

    manager.ensure_resident(&db, Some(&registry), "c").unwrap();

    assert_eq!(
        manager.resident_keys(),
        vec!["a".to_string(), "c".to_string()],
        "the ceiling held and the least recently used resident left"
    );
    assert!(
        launcher.stopped().contains(&server_b),
        "the evicted resident's server was actually stopped"
    );
    assert_eq!(
        events(&db, "b"),
        vec![
            (ResidencyEvent::Loaded, None, None),
            (
                ResidencyEvent::Evicted,
                Some("count-ceiling".to_string()),
                None
            ),
        ]
    );
}

/// A declared memory ceiling evicts before admitting, and a resident too
/// large to ever fit is refused outright rather than evicting everything
/// and failing anyway.
#[test]
fn the_memory_ceiling_evicts_before_admitting_and_refuses_an_oversized_resident() {
    let db = database();
    register_artifact(&db, "a", ARTIFACT_A);
    register_artifact(&db, "b", ARTIFACT_B);
    register_artifact(&db, "c", ARTIFACT_C);
    let registry = registry(&[
        ("worker-a", ARTIFACT_A, "http://127.0.0.1:11434"),
        ("worker-b", ARTIFACT_B, "http://127.0.0.1:11435"),
        ("worker-c", ARTIFACT_C, "http://127.0.0.1:11436"),
    ]);
    let (mut manager, launcher) = manager(residency_config(
        8,
        Some(8192),
        2,
        &[
            ("a", "worker-a", Some(4096)),
            ("b", "worker-b", Some(4096)),
            ("c", "worker-c", Some(16384)),
        ],
    ));

    manager.ensure_resident(&db, Some(&registry), "a").unwrap();
    manager.ensure_resident(&db, Some(&registry), "b").unwrap();
    assert_eq!(manager.resident_keys().len(), 2, "8192mb holds both");

    // `c` cannot fit under the ceiling at any eviction depth.
    let refused = manager.ensure_resident(&db, Some(&registry), "c").unwrap();
    match refused {
        ResidencyOutcome::NotResident { reason } => {
            assert!(
                reason.starts_with(reasons::EXCEEDS_MEMORY_CEILING),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        launcher.launch_count(),
        2,
        "an unfittable resident is never launched"
    );
    assert_eq!(
        manager.resident_keys().len(),
        2,
        "a refusal evicts nothing: the ceiling protects what is already held"
    );

    // A resident that does fit evicts the least recently used one first.
    let mut config = manager.config().clone();
    config
        .residents
        .insert("c".into(), resident("worker-c", Some(4096)));
    manager.set_config(config);
    manager.ensure_resident(&db, Some(&registry), "c").unwrap();
    assert_eq!(
        manager.resident_keys(),
        vec!["b".to_string(), "c".to_string()]
    );
    assert_eq!(
        events(&db, "a")
            .into_iter()
            .filter(|(event, _, _)| *event == ResidencyEvent::Evicted)
            .map(|(_, reason, _)| reason.unwrap())
            .collect::<Vec<_>>(),
        vec!["memory-ceiling".to_string()]
    );
}

// ---------------------------------------------------------------------
// 3. Health, bounded restart, loud failure
// ---------------------------------------------------------------------

/// A resident that dies or hangs is detected by the health check, restarted
/// at most the configured number of times with a durable record of each
/// restart, and then marked failed with a named reason. Crucially, after
/// exhaustion it does not quietly fall back to per-call loading: the next
/// call is refused with the recorded reason and no new load is paid.
#[test]
fn a_dead_resident_is_restarted_within_budget_then_fails_loudly_without_silent_degradation() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        2,
        &[("llama3", "llama3-ollama", None)],
    ));

    manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();

    // Two failures, each inside its own health interval, each restarted.
    for _ in 0..2 {
        let server = current_server(&manager, "llama3-ollama");
        launcher.break_server(&server);
        manager.health_tick(&db).unwrap();
    }
    let restarts: Vec<u32> = events(&db, "llama3")
        .into_iter()
        .filter(|(event, _, _)| *event == ResidencyEvent::Restarted)
        .map(|(_, _, attempt)| attempt.unwrap())
        .collect();
    assert_eq!(restarts, vec![1, 2], "each restart is durably numbered");
    assert_eq!(launcher.launch_count(), 3, "one load plus two restarts");

    // The third failure exhausts the budget.
    let server = current_server(&manager, "llama3-ollama");
    launcher.break_server(&server);
    manager.health_tick(&db).unwrap();

    let recorded = events(&db, "llama3");
    let failure = recorded
        .iter()
        .find(|(event, _, _)| *event == ResidencyEvent::Failed)
        .expect("a durable failure record");
    assert_eq!(failure.1.as_deref(), Some(reasons::RESTARTS_EXHAUSTED));
    assert!(
        manager.resident_keys().is_empty(),
        "a failed resident is no longer held"
    );
    assert!(
        launcher.running().is_empty(),
        "and its server is not left running"
    );

    // The next call is refused with the named reason rather than silently
    // reverting to a per-call load.
    let after = manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    match after {
        ResidencyOutcome::NotResident { reason } => assert!(
            reason.contains(reasons::RESTARTS_EXHAUSTED),
            "unexpected reason: {reason}"
        ),
        other => panic!("a failed resident must not silently reload: {other:?}"),
    }
    assert_eq!(
        launcher.launch_count(),
        3,
        "no load was paid after exhaustion"
    );
    assert_eq!(
        ModelResidencyRepository::new(db.conn())
            .load_count("llama3")
            .unwrap(),
        3
    );
}

/// A healthy resident writes no record at all. The residency table holds
/// what the daemon did and what went wrong; a row per healthy interval
/// would bury exactly the events an operator needs to find.
#[test]
fn a_healthy_resident_writes_no_record_per_health_interval() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, _launcher) = manager(residency_config(
        2,
        None,
        1,
        &[("llama3", "llama3-ollama", None)],
    ));
    manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    for _ in 0..5 {
        manager.health_tick(&db).unwrap();
    }
    assert_eq!(
        events(&db, "llama3"),
        vec![(ResidencyEvent::Loaded, None, None)],
        "five healthy intervals added nothing to the one load event"
    );
}

/// A launch that never comes up is bounded the same way a dying one is: it
/// is recorded, retried within budget, and then failed with a named reason.
#[test]
fn a_resident_that_never_starts_is_bounded_and_recorded_rather_than_retried_forever() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        1,
        &[("llama3", "llama3-ollama", None)],
    ));
    launcher.fail_launches("no such serving runtime");

    for _ in 0..3 {
        let outcome = manager
            .ensure_resident(&db, Some(&registry), "llama3")
            .unwrap();
        assert!(matches!(outcome, ResidencyOutcome::NotResident { .. }));
    }
    let recorded = events(&db, "llama3");
    assert!(
        recorded
            .iter()
            .any(|(event, reason, _)| *event == ResidencyEvent::Failed
                && reason.as_deref() == Some(reasons::RESTARTS_EXHAUSTED)),
        "an unstartable resident fails loudly: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .all(|(event, _, _)| *event != ResidencyEvent::Loaded),
        "nothing was ever loaded"
    );
}

// ---------------------------------------------------------------------
// 4. Off by default
// ---------------------------------------------------------------------

/// With residency unconfigured the manager holds nothing, records nothing,
/// and routing resolves to exactly the worker's own configured endpoint —
/// byte-identical to PRD-063's behavior.
#[test]
fn residency_defaults_off_and_routing_is_identical_to_the_configured_endpoint() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let default = ModelResidencyConfig::default();
    assert!(!default.enabled, "residency is off by default");
    let (mut manager, launcher) = manager(default);

    let outcome = manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    assert_eq!(
        outcome,
        ResidencyOutcome::NotResident {
            reason: reasons::DISABLED.into()
        }
    );
    assert_eq!(launcher.launch_count(), 0);
    assert!(manager.directory().is_empty());

    let resolved = manager
        .directory()
        .resolve("llama3-ollama", "http://127.0.0.1:11434");
    assert_eq!(resolved.base_url, "http://127.0.0.1:11434");
    assert_eq!(resolved.residency_state, ResidencyState::Cold);
    assert_eq!(resolved.resident_server_identity, None);

    let count: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM model_residency_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0, "a disabled feature writes nothing");
}

/// The global switch and the per-resident switch are independent: enabling
/// residency does not enable every declared resident.
#[test]
fn a_declared_but_disabled_resident_is_configuration_not_activation() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let mut config = residency_config(2, None, 2, &[("llama3", "llama3-ollama", None)]);
    config.residents.get_mut("llama3").unwrap().enabled = false;
    let (mut manager, launcher) = manager(config);

    let outcome = manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    assert_eq!(
        outcome,
        ResidencyOutcome::NotResident {
            reason: reasons::DISABLED.into()
        }
    );
    assert_eq!(launcher.launch_count(), 0);
}

// ---------------------------------------------------------------------
// 5. The artifact gate and the disable path
// ---------------------------------------------------------------------

/// Residency never loads a model the PRD-062 artifact registry does not
/// know, even when everything else about the configuration is valid.
#[test]
fn residency_never_loads_a_model_outside_the_artifact_registry() {
    let db = database();
    // Deliberately not registered.
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        2,
        &[("llama3", "llama3-ollama", None)],
    ));

    let outcome = manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    match outcome {
        ResidencyOutcome::NotResident { reason } => assert!(
            reason.starts_with(reasons::ARTIFACT_NOT_REGISTERED),
            "unexpected reason: {reason}"
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(launcher.launch_count(), 0);

    // Registering it makes the very same call succeed.
    register_artifact(&db, "llama3", ARTIFACT_A);
    assert!(matches!(
        manager
            .ensure_resident(&db, Some(&registry), "llama3")
            .unwrap(),
        ResidencyOutcome::Cold { .. }
    ));
}

/// Disabling a resident stops its server and records the stop. The CLI
/// writes the audited configuration change; the daemon that owns the
/// process reconciles to it.
#[test]
fn disabling_a_resident_stops_the_server_and_records_the_stop() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        2,
        &[("llama3", "llama3-ollama", None)],
    ));
    manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    let server = current_server(&manager, "llama3-ollama");

    let mut config = manager.config().clone();
    config.residents.get_mut("llama3").unwrap().enabled = false;
    manager.set_config(config);
    manager.health_tick(&db).unwrap();

    assert!(manager.resident_keys().is_empty());
    assert!(manager.directory().is_empty());
    assert!(launcher.stopped().contains(&server));
    assert!(
        events(&db, "llama3").iter().any(|(event, reason, _)| {
            *event == ResidencyEvent::Stopped
                && reason.as_deref() == Some(reasons::DISABLED_BY_CONFIGURATION)
        }),
        "the stop is durable, not just in-memory"
    );
}

/// Daemon shutdown stops every resident and records each stop, so a
/// restarted daemon never inherits an unrecorded orphan.
#[test]
fn daemon_shutdown_stops_every_resident_with_a_record() {
    let db = database();
    register_artifact(&db, "a", ARTIFACT_A);
    register_artifact(&db, "b", ARTIFACT_B);
    let registry = registry(&[
        ("worker-a", ARTIFACT_A, "http://127.0.0.1:11434"),
        ("worker-b", ARTIFACT_B, "http://127.0.0.1:11435"),
    ]);
    let (mut manager, launcher) = manager(residency_config(
        2,
        None,
        2,
        &[("a", "worker-a", None), ("b", "worker-b", None)],
    ));
    manager.ensure_resident(&db, Some(&registry), "a").unwrap();
    manager.ensure_resident(&db, Some(&registry), "b").unwrap();

    assert_eq!(manager.stop_all(&db, reasons::DAEMON_SHUTDOWN).unwrap(), 2);
    assert!(launcher.running().is_empty());
    for key in ["a", "b"] {
        assert!(events(&db, key).iter().any(|(event, reason, _)| {
            *event == ResidencyEvent::Stopped && reason.as_deref() == Some(reasons::DAEMON_SHUTDOWN)
        }));
    }
}

// ---------------------------------------------------------------------
// 6. Configuration validation
// ---------------------------------------------------------------------

/// Residency configuration is refused, with the offending value named,
/// before it can ever be persisted: an absent worker, a worker with no
/// local profile, no artifact, or no launch command.
#[test]
fn residency_configuration_fails_closed_with_the_offending_value_named() {
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);

    let mut absent = residency_config(1, None, 2, &[("llama3", "no-such-worker", None)]);
    absent.residents.get_mut("llama3").unwrap().worker = "no-such-worker".into();
    let error = absent.validate(Some(&registry)).unwrap_err();
    assert!(error.contains("no-such-worker"), "{error}");

    let mut no_launch = residency_config(1, None, 2, &[("llama3", "llama3-ollama", None)]);
    no_launch.residents.get_mut("llama3").unwrap().launch = Vec::new();
    let error = no_launch.validate(Some(&registry)).unwrap_err();
    assert!(error.contains("launch"), "{error}");

    // A memory ceiling with an undeclared resident size cannot be enforced,
    // so it is refused rather than guessed.
    let unsized_resident = residency_config(1, Some(8192), 2, &[("llama3", "llama3-ollama", None)]);
    let error = unsized_resident.validate(Some(&registry)).unwrap_err();
    assert!(error.contains("memory_mb"), "{error}");

    // A worker with no PRD-063 local profile cannot be held resident.
    let mut hosted = registry.clone();
    hosted.workers.get_mut("llama3-ollama").unwrap().local = None;
    hosted.workers.get_mut("llama3-ollama").unwrap().provider = "anthropic".into();
    let error = residency_config(1, None, 2, &[("llama3", "llama3-ollama", None)])
        .validate(Some(&hosted))
        .unwrap_err();
    assert!(error.contains("local profile"), "{error}");

    // A local worker with no PRD-062 artifact cannot be held resident.
    let mut no_artifact = registry.clone();
    no_artifact
        .workers
        .get_mut("llama3-ollama")
        .unwrap()
        .model_artifact = None;
    let error = residency_config(1, None, 2, &[("llama3", "llama3-ollama", None)])
        .validate(Some(&no_artifact))
        .unwrap_err();
    assert!(error.contains("model_artifact"), "{error}");
}

// ---------------------------------------------------------------------
// 7. Ledger attribution
// ---------------------------------------------------------------------

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

fn loop_config() -> LoopConfig {
    LoopConfig {
        worker_spec_identity: "wspec-sha256:residency-test".into(),
        worker_empirical_version: "wver-sha256:residency-test".into(),
        model: "llama3".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings {
            max_iterations: 4,
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

fn attempt_ids() -> impl FnMut() -> AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        AttemptId(format!("att_{n}"))
    }
}

fn setup_execution(db: &Database, execution_id: &str) {
    db.conn()
        .execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,?2,'local-worker','running','repo','wt','docs/prds/PRD-073.md','[]')",
            rusqlite::params![execution_id, chrono::Utc::now().to_rfc3339()],
        )
        .unwrap();
}

/// Runs the real PRD-058 loop against a fake local endpoint whose usage
/// object reports `cached_tokens`, and returns the loop outcome.
async fn run_against_endpoint(
    cached_tokens: Option<u64>,
) -> (MockServer, familiar_ai_agent::raw_runtime::RunOutcome) {
    let server = MockServer::start().await;
    let usage = match cached_tokens {
        Some(tokens) => json!({
            "prompt_tokens": 120,
            "completion_tokens": 8,
            "prompt_tokens_details": {"cached_tokens": tokens}
        }),
        None => json!({"prompt_tokens": 120, "completion_tokens": 8}),
    };
    let frames = vec![
        json!({"choices": [{"delta": {"content": "done"}}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": usage}),
    ];
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse(&frames), "text/event-stream"))
        .mount(&server)
        .await;

    let worker = LocalWorkerConfig {
        runtime_kind: LocalRuntimeKind::Ollama,
        endpoint: LocalEndpointConfig {
            base_url: server.uri(),
            tls: false,
        },
        resources: Default::default(),
    };
    let adapter =
        LocalInferenceAdapter::from_registry_config(&worker, LocalAuthToken::new(None)).unwrap();
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
            bytes: "stable".into(),
            version: "v1".into(),
        },
        &VolatileTask {
            bytes: "task".into(),
        },
        &loop_config(),
        attempt_ids(),
    )
    .await;
    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    (server, outcome)
}

fn usage_of(
    outcome: &familiar_ai_agent::raw_runtime::RunOutcome,
) -> familiar_ai_llm::attempt::UsageCategories {
    outcome.attempts.iter().fold(
        Default::default(),
        |accumulator: familiar_ai_llm::attempt::UsageCategories, attempt| {
            accumulator.merge(&attempt.usage)
        },
    )
}

/// A warm call against a runtime that reports cached prefix tokens records
/// a cache hit; the same warm call against a runtime that reports nothing
/// records `unknown` with a stated reason. Residency state alone never
/// stands in for a cache measurement.
#[tokio::test]
async fn warm_cache_evidence_is_recorded_only_when_the_runtime_reports_it() {
    let (_server, hit) = run_against_endpoint(Some(96)).await;
    let (_server2, silent) = run_against_endpoint(None).await;

    let hit_evidence = CacheEvidence::classify(ResidencyState::Warm, &usage_of(&hit));
    assert_eq!(hit_evidence, CacheEvidence::WarmHit);

    let silent_evidence = CacheEvidence::classify(ResidencyState::Warm, &usage_of(&silent));
    assert_eq!(
        silent_evidence,
        CacheEvidence::Unknown {
            reason: NO_CACHE_ACCOUNTING_REASON.into()
        }
    );

    // A runtime that reports zero cached tokens is a measured miss, which
    // is a different fact from reporting nothing.
    let (_server3, miss) = run_against_endpoint(Some(0)).await;
    assert_eq!(
        CacheEvidence::classify(ResidencyState::Warm, &usage_of(&miss)),
        CacheEvidence::WarmMiss
    );
}

/// Every local execution's telemetry row and ledger observation carry the
/// residency state and the identity of the server that served it, so
/// latency and utilization partition by warmth.
#[tokio::test]
async fn local_execution_records_residency_state_and_resident_identity() {
    let db = database();
    setup_execution(&db, "exec_1");
    let (_server, outcome) = run_against_endpoint(Some(96)).await;
    let attribution = LocalResidencyAttribution::from_usage(
        ResidencyState::Warm,
        Some("llama3#1"),
        &usage_of(&outcome),
    );
    assert_eq!(attribution.cache_evidence, CacheEvidence::WarmHit);

    let telemetry_ids = persist_local_telemetry(
        &LocalTelemetryRepository::new(db.conn()),
        "exec_1",
        "implementation",
        "llama3-ollama",
        "ollama",
        Some(ARTIFACT_A),
        LocalArtifactVerificationState::Verified,
        &LocalRunMeasurements {
            wall_time_ms: Some(500),
            ..Default::default()
        },
        &outcome,
        0,
        Some(&attribution),
    )
    .unwrap();
    let telemetry_id = &telemetry_ids[0];

    let recorded: (Option<String>, Option<String>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT residency_state,resident_server_identity,cache_evidence FROM local_worker_telemetry WHERE telemetry_id=?1",
            [telemetry_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        recorded,
        (
            Some("warm".into()),
            Some("llama3#1".into()),
            Some("warm-hit".into())
        )
    );

    // The same attribution reaches the PRD-051 ledger, keyed to the
    // observation the execution wrote.
    let accounting = AccountingRepository::new(db.conn());
    let observation_id = accounting
        .append_observation(&familiar_ai_storage::repos::accounting::UsageObservation {
            execution_id: "exec_1",
            attempt_id: "att_1",
            stage: "implementation",
            session_id: None,
            worker_identity: "llama3-ollama",
            adapter: "local",
            cli_version: None,
            model_identity: Some("llama3"),
            service_tier: None,
            provider_request_id: None,
            uncached_input_tokens: Some(24),
            cache_read_tokens: Some(96),
            cache_write_tokens: None,
            output_tokens: Some(8),
            reasoning_output_tokens: None,
            unknown_reason: None,
            period_start: "2026-09-12T00:00:00Z",
            period_end: "2026-09-12T00:00:01Z",
            terminal_status: "succeeded",
            source_event_hash: "residency-test-hash",
            provider_cost_lexical: None,
            project_resolution_evidence: None,
            output_register_id: "raw-runtime-none",
            output_register_version: "1",
            input_compression_id: "raw-runtime-none",
            input_compression_version: "1",
            compression_experiment: None,
            compression_lane: None,
            edit_form_id: "raw-runtime-none",
            edit_form_version: "1",
            truncation_config_id: "raw-runtime-none",
            truncation_config_version: "1",
        })
        .unwrap()
        .unwrap();

    let residency_repo = ModelResidencyRepository::new(db.conn());
    attach_observation_residency(&residency_repo, &observation_id, &attribution).unwrap();
    assert_eq!(
        residency_repo
            .observation_residency(&observation_id)
            .unwrap(),
        Some(
            familiar_ai_storage::repos::model_residency::RecordedObservationResidency {
                residency_state: "warm".into(),
                resident_server_identity: Some("llama3#1".into()),
                cache_evidence: "warm-hit".into(),
                cache_evidence_reason: None,
            }
        )
    );
}

/// An unknown cache evidence must say why it is unknown, and a warm record
/// must name the server that served it. Both are refused rather than
/// written as a silent hole.
#[test]
fn residency_attribution_refuses_to_record_an_unexplained_hole() {
    use familiar_ai_storage::repos::model_residency::{
        CacheEvidenceRecord, ObservationResidency, ResidencyStateRecord,
    };
    let db = database();
    let repo = ModelResidencyRepository::new(db.conn());

    let warm_without_server = repo.attach_to_observation(
        "obs_x",
        &ObservationResidency {
            residency_state: ResidencyStateRecord::Warm,
            resident_server_identity: None,
            cache_evidence: CacheEvidenceRecord::WarmHit,
            cache_evidence_reason: None,
        },
    );
    assert!(warm_without_server.is_err());

    let unknown_without_reason = repo.attach_to_observation(
        "obs_x",
        &ObservationResidency {
            residency_state: ResidencyStateRecord::Cold,
            resident_server_identity: None,
            cache_evidence: CacheEvidenceRecord::Unknown,
            cache_evidence_reason: None,
        },
    );
    assert!(unknown_without_reason.is_err());
}

/// Residency records are append-only: the durable history of what the
/// daemon did cannot be edited away.
#[test]
fn residency_records_are_append_only() {
    let db = database();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let (mut manager, _launcher) = manager(residency_config(
        2,
        None,
        2,
        &[("llama3", "llama3-ollama", None)],
    ));
    manager
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();

    assert!(db
        .conn()
        .execute(
            "UPDATE model_residency_events SET event='stopped' WHERE resident_key='llama3'",
            []
        )
        .is_err());
    assert!(db
        .conn()
        .execute(
            "DELETE FROM model_residency_events WHERE resident_key='llama3'",
            []
        )
        .is_err());
}

// ---------------------------------------------------------------------
// 8. Enabling residency is an audited configuration mutation
// ---------------------------------------------------------------------

const CONFIG_FIXTURE: &str = r#"# operator comment that must survive every mutation

[worker_registry.workers.llama3-ollama]
provider = "local"
model = "llama3"
runtime = "ollama"
model_artifact = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
capabilities = ["implementation"]

[worker_registry.workers.llama3-ollama.local]
runtime_kind = "ollama"

[worker_registry.workers.llama3-ollama.local.endpoint]
base_url = "http://127.0.0.1:11434"
"#;

fn config_context() -> (
    tempfile::TempDir,
    familiar_ai_daemon::config_cli::ConfigContext,
) {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, CONFIG_FIXTURE).unwrap();
    let context = familiar_ai_daemon::config_cli::ConfigContext {
        config_path,
        data_dir: directory.path().join("data"),
    };
    (directory, context)
}

fn decisions(
    context: &familiar_ai_daemon::config_cli::ConfigContext,
) -> Vec<familiar_ai_storage::repos::config_decision::ConfigDecision> {
    let config = familiar_ai_core::config::Config::load(Some(&context.config_path)).unwrap();
    let db = Database::open(&config.database.resolve_path(&context.data_dir)).unwrap();
    db.run_migrations().unwrap();
    familiar_ai_storage::ConfigDecisionRepository::new(&db)
        .list(10)
        .unwrap()
}

/// Residency defaults off; enabling it per model artifact is an explicit
/// configuration mutation that preserves the operator's file, records a
/// decision row, and is refused outright when the artifact or worker does
/// not qualify.
#[test]
fn enabling_and_disabling_residency_are_audited_configuration_mutations() {
    use familiar_ai_daemon::cli::model_residency::{execute_with_context, ResidencyAction};

    let (_directory, context) = config_context();
    // Nothing is resident until someone says so.
    let before = familiar_ai_core::config::Config::load(Some(&context.config_path))
        .map(|config| config.model_residency.enabled)
        .unwrap();
    assert!(!before, "residency is off in an untouched configuration");

    execute_with_context(
        ResidencyAction::Enable {
            key: "llama3".into(),
            worker: "llama3-ollama".into(),
            launch: vec!["ollama".into(), "serve".into()],
            memory_mb: Some(4096),
            ready_timeout_secs: 30,
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap();

    let written = std::fs::read_to_string(&context.config_path).unwrap();
    assert!(
        written.contains("# operator comment that must survive every mutation"),
        "the operator's file is preserved, not rewritten"
    );
    assert!(written.contains("added by familiar-ai model-residency enable — human:test"));

    let config = familiar_ai_core::config::Config::load(Some(&context.config_path)).unwrap();
    assert!(config.model_residency.enabled);
    let resident = config.model_residency.residents.get("llama3").unwrap();
    assert!(resident.enabled);
    assert_eq!(resident.worker, "llama3-ollama");
    assert_eq!(resident.launch, vec!["ollama".to_string(), "serve".into()]);
    assert_eq!(resident.memory_mb, Some(4096));
    // The declared residency is internally consistent against the registry.
    config.validate().unwrap();

    let recorded = decisions(&context);
    assert_eq!(
        recorded.first().map(|decision| decision.command.as_str()),
        Some("familiar-ai model-residency enable")
    );
    assert_eq!(
        recorded.first().map(|decision| decision.actor.as_str()),
        Some("human:test")
    );
    assert_ne!(
        recorded.first().map(|decision| &decision.before_hash),
        recorded.first().map(|decision| &decision.after_hash),
        "the decision row binds the exact before/after configuration"
    );

    // Disabling is its own audited mutation, and keeps the declaration so
    // re-enabling is not a re-derivation.
    execute_with_context(
        ResidencyAction::Disable {
            key: "llama3".into(),
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap();
    let config = familiar_ai_core::config::Config::load(Some(&context.config_path)).unwrap();
    let resident = config.model_residency.residents.get("llama3").unwrap();
    assert!(!resident.enabled, "the resident is off");
    assert_eq!(
        resident.launch,
        vec!["ollama".to_string(), "serve".into()],
        "its declared launch command survives"
    );
    assert!(!config.model_residency.is_active("llama3"));
    let recorded = decisions(&context);
    assert_eq!(
        recorded.first().map(|decision| decision.command.as_str()),
        Some("familiar-ai model-residency disable")
    );
    assert_eq!(recorded.len(), 2);
}

/// A residency entry naming a worker that cannot be held resident is
/// refused before a single byte is written.
#[test]
fn enabling_residency_for_an_unqualified_worker_writes_nothing() {
    use familiar_ai_daemon::cli::model_residency::{execute_with_context, ResidencyAction};

    let (_directory, context) = config_context();
    let before = std::fs::read_to_string(&context.config_path).unwrap();
    let error = execute_with_context(
        ResidencyAction::Enable {
            key: "ghost".into(),
            worker: "no-such-worker".into(),
            launch: vec!["ollama".into(), "serve".into()],
            memory_mb: None,
            ready_timeout_secs: 30,
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap_err();
    assert!(error.contains("no-such-worker"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&context.config_path).unwrap(),
        before,
        "a refused mutation leaves the configuration untouched"
    );

    // And a missing launch command is refused for the same reason: Familiar
    // never guesses how a serving runtime starts.
    let error = execute_with_context(
        ResidencyAction::Enable {
            key: "llama3".into(),
            worker: "llama3-ollama".into(),
            launch: Vec::new(),
            memory_mb: None,
            ready_timeout_secs: 30,
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap_err();
    assert!(error.contains("--launch"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&context.config_path).unwrap(),
        before
    );
}

/// The reason residency state is recorded at all: latency, load time and
/// utilization partition by whether the model was already warm, so a
/// promotion of any residency default can cite a measurement.
#[test]
fn local_telemetry_partitions_latency_and_utilization_by_residency() {
    use familiar_ai_storage::repos::local_telemetry::LocalTelemetryRow;

    let db = database();
    setup_execution(&db, "exec_1");
    let repo = LocalTelemetryRepository::new(db.conn());
    let row = |residency_state, server, cache_evidence, wall_ms, load_ms, utilization| {
        LocalTelemetryRow {
            execution_id: "exec_1",
            attempt_id: "att_1",
            stage: "implementation",
            spec_identity: "wspec-sha256:residency",
            empirical_version: "wver-sha256:residency",
            worker_identity: "llama3-ollama",
            runtime_id: "ollama",
            model_artifact_id: Some(ARTIFACT_A),
            artifact_verification_state: LocalArtifactVerificationState::Verified.into(),
            wall_time_ms: Some(wall_ms),
            load_time_ms: load_ms,
            accelerator_utilization_pct: Some(utilization),
            residency_state: Some(residency_state),
            resident_server_identity: server,
            cache_evidence: Some(cache_evidence),
            ..Default::default()
        }
    };
    // One cold run that paid the load, two warm runs that did not.
    repo.record_telemetry(&row("cold", None, "cold-load", 9_000, Some(7_000), 40.0))
        .unwrap();
    repo.record_telemetry(&row(
        "warm",
        Some("llama3#1"),
        "warm-hit",
        1_000,
        None,
        80.0,
    ))
    .unwrap();
    repo.record_telemetry(&row(
        "warm",
        Some("llama3#1"),
        "warm-miss",
        2_000,
        None,
        70.0,
    ))
    .unwrap();
    // A pre-residency row stays out of both partitions: "ran before
    // residency existed" is not "ran cold".
    repo.record_telemetry(&LocalTelemetryRow {
        execution_id: "exec_1",
        attempt_id: "att_0",
        stage: "implementation",
        spec_identity: "wspec-sha256:residency",
        empirical_version: "wver-sha256:residency",
        worker_identity: "llama3-ollama",
        runtime_id: "ollama",
        artifact_verification_state: LocalArtifactVerificationState::Verified.into(),
        wall_time_ms: Some(50_000),
        ..Default::default()
    })
    .unwrap();

    let partitions = ModelResidencyRepository::new(db.conn())
        .telemetry_by_residency(Some("llama3-ollama"))
        .unwrap();
    assert_eq!(
        partitions.len(),
        2,
        "cold and warm, not the unattributed row"
    );

    let cold = &partitions[0];
    assert_eq!(cold.residency_state, "cold");
    assert_eq!(cold.runs, 1);
    assert_eq!(cold.mean_wall_time_ms, Some(9_000.0));
    assert_eq!(cold.mean_load_time_ms, Some(7_000.0));

    let warm = &partitions[1];
    assert_eq!(warm.residency_state, "warm");
    assert_eq!(warm.runs, 2);
    assert_eq!(warm.mean_wall_time_ms, Some(1_500.0));
    assert_eq!(
        warm.mean_load_time_ms, None,
        "a warm run reports no load time, and no zero is invented for it"
    );
    assert_eq!(warm.mean_accelerator_utilization_pct, Some(75.0));
}

// ---------------------------------------------------------------------
// 9. The production launcher and the daemon task
// ---------------------------------------------------------------------

fn process_spec(base_url: &str, launch: Vec<String>) -> ResidentLaunchSpec {
    ResidentLaunchSpec {
        resident_key: "llama3".into(),
        worker_identity: "llama3-ollama".into(),
        model_artifact_id: ARTIFACT_A.into(),
        runtime_id: "ollama".into(),
        base_url: base_url.into(),
        launch,
        memory_mb: None,
        ready_timeout_secs: 1,
    }
}

/// `probe` is blocking by design (the daemon calls it from `spawn_blocking`),
/// so tests drive it the same way rather than from the async context.
async fn probe(
    launcher: &Arc<ProcessResidentLauncher>,
    server_identity: &str,
    base_url: &str,
) -> Result<(), String> {
    let launcher = launcher.clone();
    let server_identity = server_identity.to_string();
    let base_url = base_url.to_string();
    tokio::task::spawn_blocking(move || launcher.probe(&server_identity, &base_url))
        .await
        .unwrap()
}

/// The production launcher against a real child process and a real loopback
/// endpoint: it starts the operator's argv, reports a live-and-answering
/// server as healthy, and reports a process that exited, one that was
/// stopped, and one that was never started as unhealthy.
#[tokio::test]
async fn the_process_launcher_starts_stops_and_honestly_probes_a_real_child() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .mount(&server)
        .await;
    let launcher = Arc::new(ProcessResidentLauncher::new(2));
    let spec = process_spec(&server.uri(), vec!["sleep".into(), "300".into()]);

    // Never launched: not running, and never confused with healthy.
    assert!(probe(&launcher, "ghost#1", &spec.base_url)
        .await
        .unwrap_err()
        .contains("not running"));

    launcher.launch(&spec, "live#1").unwrap();
    assert!(
        probe(&launcher, "live#1", &spec.base_url).await.is_ok(),
        "a live child behind an answering endpoint is healthy"
    );

    // Stopping it makes it unhealthy immediately, and stopping twice is not
    // an error — the daemon stops best-effort on paths that may race.
    launcher.stop("live#1").unwrap();
    launcher.stop("live#1").unwrap();
    assert!(probe(&launcher, "live#1", &spec.base_url)
        .await
        .unwrap_err()
        .contains("not running"));

    // A process that exits on its own is detected as exited, even though
    // the endpoint it was supposed to serve still answers.
    let dead = process_spec(&server.uri(), vec!["true".into()]);
    launcher.launch(&dead, "dead#1").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let error = probe(&launcher, "dead#1", &dead.base_url)
        .await
        .unwrap_err();
    assert!(error.contains("exited"), "{error}");
}

/// A live process whose endpoint refuses, errors, or never answers is
/// unhealthy: residency is about a server that actually serves, not a
/// process that merely exists.
#[tokio::test]
async fn a_live_process_behind_a_broken_endpoint_is_unhealthy() {
    let failing = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/v1/models"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&failing)
        .await;
    let launcher = Arc::new(ProcessResidentLauncher::new(2));

    let spec = process_spec(&failing.uri(), vec!["sleep".into(), "300".into()]);
    launcher.launch(&spec, "sick#1").unwrap();
    let error = probe(&launcher, "sick#1", &spec.base_url)
        .await
        .unwrap_err();
    assert!(error.contains("503"), "{error}");

    // Nothing is listening here at all.
    let unreachable = process_spec("http://127.0.0.1:1", vec!["sleep".into(), "300".into()]);
    launcher.launch(&unreachable, "gone#1").unwrap();
    let error = probe(&launcher, "gone#1", &unreachable.base_url)
        .await
        .unwrap_err();
    assert!(error.contains("unreachable"), "{error}");

    launcher.stop("sick#1").unwrap();
    launcher.stop("gone#1").unwrap();
}

/// An argv that cannot be executed is a named launch failure, not a panic
/// and not a silently-absent resident.
#[test]
fn an_unexecutable_launch_command_fails_with_the_program_named() {
    let launcher = ProcessResidentLauncher::default();
    let missing = process_spec(
        "http://127.0.0.1:11434",
        vec!["familiar-ai-no-such-serving-runtime".into()],
    );
    let error = launcher.launch(&missing, "missing#1").unwrap_err();
    assert!(
        error.contains("familiar-ai-no-such-serving-runtime"),
        "{error}"
    );

    let empty = process_spec("http://127.0.0.1:11434", Vec::new());
    assert!(launcher
        .launch(&empty, "empty#1")
        .unwrap_err()
        .contains("empty"));
}

fn daemon_config(residency: ModelResidencyConfig) -> familiar_ai_core::config::Config {
    familiar_ai_core::config::Config {
        model_residency: residency,
        ..Default::default()
    }
}

/// With residency disabled the daemon task is a no-op that returns at once,
/// which is what makes spawning it unconditionally free.
#[tokio::test]
async fn the_daemon_residency_task_is_a_no_op_when_residency_is_disabled() {
    let db = Arc::new(Mutex::new(database()));
    let (inner, launcher) = manager(ModelResidencyConfig::default());
    let residency_manager = Arc::new(Mutex::new(inner));
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        familiar_ai_daemon::model_residency::run(
            db,
            residency_manager,
            daemon_config(ModelResidencyConfig::default()),
            shutdown_rx,
        ),
    )
    .await
    .expect("the disabled task returns immediately rather than ticking forever");
    assert_eq!(launcher.launch_count(), 0);
}

/// The daemon task health-checks on its interval and, on shutdown, stops
/// every resident and records each stop — so a restarted daemon never
/// inherits an orphan nobody recorded.
#[tokio::test]
async fn the_daemon_residency_task_health_checks_then_stops_residents_on_shutdown() {
    let raw = database();
    register_artifact(&raw, "llama3", ARTIFACT_A);
    let registry = registry(&[("llama3-ollama", ARTIFACT_A, "http://127.0.0.1:11434")]);
    let config = residency_config(2, None, 2, &[("llama3", "llama3-ollama", None)]);
    let launcher = Arc::new(FakeLauncher::default());
    let mut inner = ResidencyManager::new(config.clone(), launcher.clone());
    inner
        .ensure_resident(&raw, Some(&registry), "llama3")
        .unwrap();
    let server = inner
        .directory()
        .get("llama3-ollama")
        .unwrap()
        .server_identity
        .clone();

    let db = Arc::new(Mutex::new(raw));
    let residency_manager = Arc::new(Mutex::new(inner));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(familiar_ai_daemon::model_residency::run(
        db.clone(),
        residency_manager.clone(),
        daemon_config(config),
        shutdown_rx,
    ));

    // Let at least one health interval elapse against a healthy resident.
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    assert_eq!(
        launcher.launch_count(),
        1,
        "a healthy resident is not reloaded by its health check"
    );

    shutdown_tx.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the task drains within the shutdown budget")
        .unwrap();

    assert!(launcher.stopped().contains(&server));
    assert!(launcher.running().is_empty());
    let db = db.lock().unwrap();
    assert!(
        events(&db, "llama3").iter().any(|(event, reason, _)| {
            *event == ResidencyEvent::Stopped && reason.as_deref() == Some(reasons::DAEMON_SHUTDOWN)
        }),
        "the shutdown stop is durable"
    );
}

/// `model-residency status` reports the configuration and the recorded
/// lifecycle events, both when residency is configured and when it is not.
#[test]
fn residency_status_reports_configuration_and_recorded_events() {
    use familiar_ai_daemon::cli::model_residency::{execute_with_context, ResidencyAction};

    let (_directory, context) = config_context();
    // Nothing configured yet: status is still a useful, non-failing answer.
    execute_with_context(ResidencyAction::Status { limit: 5 }, &context).unwrap();

    execute_with_context(
        ResidencyAction::Enable {
            key: "llama3".into(),
            worker: "llama3-ollama".into(),
            launch: vec!["ollama".into(), "serve".into()],
            memory_mb: None,
            ready_timeout_secs: 30,
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap();
    execute_with_context(ResidencyAction::Status { limit: 5 }, &context).unwrap();

    // With recorded events, status surfaces them newest-first.
    let config = familiar_ai_core::config::Config::load(Some(&context.config_path)).unwrap();
    let db = Database::open(&config.database.resolve_path(&context.data_dir)).unwrap();
    db.run_migrations().unwrap();
    register_artifact(&db, "llama3", ARTIFACT_A);
    let registry = config.worker_registry.clone().unwrap();
    let launcher = Arc::new(FakeLauncher::default());
    let mut inner = ResidencyManager::new(config.model_residency.clone(), launcher);
    inner
        .ensure_resident(&db, Some(&registry), "llama3")
        .unwrap();
    assert_eq!(
        ModelResidencyRepository::new(db.conn())
            .recent(5)
            .unwrap()
            .len(),
        1
    );
    drop(db);
    execute_with_context(ResidencyAction::Status { limit: 5 }, &context).unwrap();

    // A resident that was never declared cannot be disabled.
    let error = execute_with_context(
        ResidencyAction::Disable {
            key: "ghost".into(),
            actor: Some("human:test".into()),
        },
        &context,
    )
    .unwrap_err();
    assert!(error.contains("ghost"), "{error}");
}

/// Re-enabling an already-enabled resident is refused rather than silently
/// rewriting the operator's declaration.
#[test]
fn enabling_an_already_enabled_resident_is_refused() {
    use familiar_ai_daemon::cli::model_residency::{execute_with_context, ResidencyAction};

    let (_directory, context) = config_context();
    let enable = || ResidencyAction::Enable {
        key: "llama3".into(),
        worker: "llama3-ollama".into(),
        launch: vec!["ollama".into(), "serve".into()],
        memory_mb: None,
        ready_timeout_secs: 30,
        actor: Some("human:test".into()),
    };
    execute_with_context(enable(), &context).unwrap();
    let error = execute_with_context(enable(), &context).unwrap_err();
    assert!(error.contains("already enabled"), "{error}");
}
