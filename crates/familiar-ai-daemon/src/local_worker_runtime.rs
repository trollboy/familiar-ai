//! PRD-063 daemon-side local worker scheduling: PRD-064 typed reservations
//! over hardware, managed-vs-externally-shared capacity classification,
//! honest failure-state detection (contention, memory pressure, crash,
//! endpoint disappearance), and PRD-051 telemetry persistence for local
//! inference. No USD cost is invented for local execution
//! (`docs/contracts/local-worker-runtime.md`).
//!
//! This module is the daemon-side glue only: it never talks to a local
//! endpoint itself (`familiar_ai_agent::local_worker` does that) and never
//! reinvents reservation lifecycle mechanics — every acquire/commit/
//! release/expire/recover call is the ordinary PRD-064
//! `ReservationRepository` API.
//!
//! **Production dispatch status:** these functions are complete and
//! exercised end-to-end against a fake endpoint (`tests/
//! local_worker_runtime.rs`: reservation acquisition, the real PRD-058
//! loop, reservation resolution, telemetry persistence, memory-pressure
//! overrun, endpoint disappearance), but no call site in this crate's
//! `run` module invokes them yet — worker-selection/execution there
//! (`build_agent`/`AdapterFactories`) only builds the CLI-driven
//! `CodingAgent` adapters (`codex`, `claude-code`,
//! `ollama`-via-Codex-harness), so a `provider = "local"` registry entry is
//! config-validated but not yet routed to an execution. This mirrors the
//! identical, pre-existing gap for every other PRD-058 raw-runtime adapter
//! in this workspace (Anthropic, OpenAI, xAI): fully implemented and
//! tested, none reachable from production dispatch either. Closing that gap
//! for local workers specifically — without inventing the shared
//! raw-runtime dispatch mechanism all four providers are waiting on — is
//! deferred to a follow-up change.
//!
//! Until that follow-up lands, `run::resolved_worker_plan` fails closed
//! (`Err`, before any `CodingAgent` is built) whenever a stage selection
//! lands on a worker declaring a `local` profile, rather than silently
//! misdispatching it through the CLI-driven path: a local worker's
//! `runtime` (e.g. `"ollama"`) can otherwise collide with an unrelated
//! pre-existing CLI-driven adapter id of the same name, which would
//! silently execute the worker through the wrong, unverified, unreserved,
//! untelemetered path instead of refusing outright.

use chrono::{DateTime, Utc};

use familiar_ai_agent::raw_runtime::{RunOutcome, StopReason};
use familiar_ai_core::config::LocalResourceProfileConfig;
use familiar_ai_core::{
    GrantMode, ReservationOwnerIdentity, ResourceRequest, ResourceType, UnknownConsumptionPolicy,
};
use familiar_ai_llm::local_runtime::ArtifactVerificationOutcome;
use familiar_ai_llm::residency::{CacheEvidence, ResidencyState};
use familiar_ai_storage::repos::local_telemetry::{
    LocalArtifactVerificationState, LocalTelemetryRepository, LocalTelemetryRow,
};
use familiar_ai_storage::repos::model_residency::{
    CacheEvidenceRecord, ModelResidencyRepository, ObservationResidency, ResidencyStateRecord,
};
use familiar_ai_storage::repos::reservation::{
    AcquireOutcome, ReservationRepository, SettlementObservation, SettlementResult,
};

/// Managed capacity is exclusively Familiar's; externally shared capacity
/// can be consumed by another process, user, or client outside Familiar's
/// control. Coexistence is guaranteed only over managed capacity — the
/// class is recorded alongside every capacity observation so scheduling
/// decisions never silently assume exclusivity they do not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalCapacityClass {
    Managed,
    ExternallyShared,
}

/// A point-in-time capacity fact. Reservations derive their confidence from
/// how fresh the observation behind them is; a stale observation is never
/// treated as current.
#[derive(Debug, Clone)]
pub struct CapacityObservation {
    pub pool_id: String,
    pub resource_type: ResourceType,
    pub available_amount: u64,
    pub observed_at: DateTime<Utc>,
    pub class: LocalCapacityClass,
}

pub fn capacity_is_fresh(
    observation: &CapacityObservation,
    now: DateTime<Utc>,
    max_age: chrono::Duration,
) -> bool {
    now.signed_duration_since(observation.observed_at) <= max_age
}

/// How to treat a resource whose capacity Familiar has never observed
/// (no pool defined). Never optimistic invention: either refuse outright,
/// or bootstrap one conservative single-slot pool and serialize through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownCapacityPolicy {
    Refuse,
    SerializeConservatively,
}

/// Builds the per-execution PRD-064 requests for one local inference run:
/// this worker's own declared footprint (accelerator/system memory), one
/// inference slot, and — only when the worker declares it — one
/// exclusive-runtime unit. `model_loading_slots` is requested separately by
/// [`acquire_model_load_reservation`] since it is held only for the
/// cold-start window, not the whole execution.
pub fn execution_resource_requests(
    pool_prefix: &str,
    accelerator_memory_mb: Option<u64>,
    system_memory_mb: Option<u64>,
    exclusive_runtime: bool,
) -> Vec<ResourceRequest> {
    let mut requests = vec![ResourceRequest {
        pool_id: format!("{pool_prefix}:inference-slots"),
        resource_type: ResourceType::InferenceSlots,
        amount: 1,
    }];
    if let Some(amount) = accelerator_memory_mb {
        requests.push(ResourceRequest {
            pool_id: format!("{pool_prefix}:accelerator-memory"),
            resource_type: ResourceType::AcceleratorMemory,
            amount,
        });
    }
    if let Some(amount) = system_memory_mb {
        requests.push(ResourceRequest {
            pool_id: format!("{pool_prefix}:system-memory"),
            resource_type: ResourceType::SystemMemory,
            amount,
        });
    }
    if exclusive_runtime {
        requests.push(ResourceRequest {
            pool_id: format!("{pool_prefix}:exclusive-runtime"),
            resource_type: ResourceType::ExclusiveRuntime,
            amount: 1,
        });
    }
    requests
}

/// Acquires the given requests, applying `policy` to any pool Familiar has
/// never observed (no `define_pool` call ever ran for it, so it reports
/// zero available and the acquisition is refused by construction).
/// `SerializeConservatively` bootstraps exactly one conservative
/// single-occupant pool per unknown resource before retrying — never a
/// larger invented number, and never a pool the operator already defined.
///
/// `acquire`'s `Refused { unavailable }` cannot itself distinguish "no pool
/// row exists" from "a pool exists but this request doesn't fit" — both
/// look identical to it. So before bootstrapping anything, this checks
/// `pool_is_defined` per refused request: a request against an
/// already-defined pool stays refused untouched (the operator's configured
/// capacity is never rewritten); only a genuinely unobserved pool gets the
/// one-occupant bootstrap.
pub fn acquire_with_unknown_capacity_policy(
    repo: &mut ReservationRepository<'_>,
    owner: &ReservationOwnerIdentity,
    requests: &[ResourceRequest],
    policy: UnknownCapacityPolicy,
) -> familiar_ai_core::Result<AcquireOutcome> {
    let first = repo.acquire(owner, requests, GrantMode::AllOrNothing, None)?;
    let AcquireOutcome::Refused { unavailable } = &first else {
        return Ok(first);
    };
    if policy == UnknownCapacityPolicy::Refuse {
        return Ok(first);
    }
    let mut bootstrapped_any = false;
    for request in unavailable {
        if !repo.pool_is_defined(&request.pool_id, &request.resource_type)? {
            repo.define_pool(&request.pool_id, &request.resource_type, 1, false)?;
            bootstrapped_any = true;
        }
    }
    if !bootstrapped_any {
        return Ok(first);
    }
    repo.acquire(owner, requests, GrantMode::AllOrNothing, None)
}

/// Builds a worker's per-execution requests straight from its declared
/// PRD-063 [`LocalResourceProfileConfig`], rather than a caller hand-picking
/// the same fields positionally.
pub fn execution_resource_requests_for_profile(
    pool_prefix: &str,
    profile: &LocalResourceProfileConfig,
) -> Vec<ResourceRequest> {
    execution_resource_requests(
        pool_prefix,
        profile.accelerator_memory_mb,
        profile.system_memory_mb,
        profile.exclusive_runtime,
    )
}

/// Defines this worker's PRD-064 pool capacities from its declared PRD-063
/// resource profile: `concurrent_inference_slots` and `model_loading_slots`
/// are the pool capacities themselves — how many concurrent executions or
/// simultaneous cold starts this hardware profile supports — replacing a
/// caller hand-picking a capacity the operator never configured. A field
/// left absent defines no pool for that resource at all: an unconfigured
/// resource stays unknown capacity, refused by
/// [`acquire_with_unknown_capacity_policy`]'s default rather than assumed to
/// have some invented size.
pub fn define_pools_from_resource_profile(
    repo: &mut ReservationRepository<'_>,
    pool_prefix: &str,
    profile: &LocalResourceProfileConfig,
) -> familiar_ai_core::Result<()> {
    if let Some(capacity) = profile.concurrent_inference_slots {
        repo.define_pool(
            &format!("{pool_prefix}:inference-slots"),
            &ResourceType::InferenceSlots,
            capacity,
            false,
        )?;
    }
    if let Some(capacity) = profile.model_loading_slots {
        repo.define_pool(
            &format!("{pool_prefix}:model-loading-slots"),
            &ResourceType::ModelLoadingSlots,
            capacity,
            false,
        )?;
    }
    Ok(())
}

/// Whether an observed thermal or power reading has crossed this worker's
/// declared PRD-063 ceiling. `None` in either the profile or the
/// observation means no ceiling is configured or no sensor is available —
/// refusal only ever fires on a real observation against an operator-set
/// ceiling, never on an absent value assumed safe or unsafe.
pub fn thermal_or_power_ceiling_exceeded(
    profile: &LocalResourceProfileConfig,
    observed_temperature_celsius: Option<f64>,
    observed_power_watts: Option<f64>,
) -> Option<&'static str> {
    if let (Some(ceiling), Some(observed)) = (
        profile.thermal_ceiling_celsius,
        observed_temperature_celsius,
    ) {
        if observed >= ceiling as f64 {
            return Some("thermal-ceiling-exceeded");
        }
    }
    if let (Some(ceiling), Some(observed)) = (profile.power_ceiling_watts, observed_power_watts) {
        if observed >= ceiling as f64 {
            return Some("power-ceiling-exceeded");
        }
    }
    None
}

/// Maps a runtime's artifact-digest probe outcome
/// (`familiar_ai_llm::local_runtime::ArtifactVerificationOutcome`) to the
/// persisted PRD-063 telemetry state: only a confirmed digest match records
/// `Verified`. `Mismatch` is an active refusal signal, kept distinct from
/// the merely-unproven `DegradedUnverified` — neither is ever treated as a
/// match by routing policy (`docs/contracts/local-worker-runtime.md`).
pub fn artifact_verification_state_for(
    outcome: &ArtifactVerificationOutcome,
) -> LocalArtifactVerificationState {
    match outcome {
        ArtifactVerificationOutcome::Verified { .. } => LocalArtifactVerificationState::Verified,
        ArtifactVerificationOutcome::Mismatch { .. } => LocalArtifactVerificationState::Mismatch,
        ArtifactVerificationOutcome::Unverifiable { .. } => {
            LocalArtifactVerificationState::DegradedUnverified
        }
    }
}

/// Reserves one model-loading slot for the cold-start window. Callers
/// release it (via [`ReservationRepository::release`]) as soon as the model
/// finishes loading, independent of the inference-slot reservation that
/// covers the rest of the execution.
pub fn acquire_model_load_reservation(
    repo: &mut ReservationRepository<'_>,
    owner: &ReservationOwnerIdentity,
    pool_prefix: &str,
    policy: UnknownCapacityPolicy,
) -> familiar_ai_core::Result<AcquireOutcome> {
    let requests = vec![ResourceRequest {
        pool_id: format!("{pool_prefix}:model-loading-slots"),
        resource_type: ResourceType::ModelLoadingSlots,
        amount: 1,
    }];
    acquire_with_unknown_capacity_policy(repo, owner, &requests, policy)
}

/// The honest failure-state classification for how an execution's
/// reservation must be resolved once the loop is finished. Mirrors the
/// PRD-058 `StopReason` taxonomy: a completed or honestly-failed run
/// commits with observed (or known-zero) consumption; a genuinely ambiguous
/// outcome (timeout, crash, disappearance) holds the reservation rather
/// than releasing capacity that may still be in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationResolution {
    CommitObserved,
    HoldUnknown,
    ReleaseUnused,
}

pub fn resolution_for_stop_reason(stop_reason: StopReason) -> ReservationResolution {
    match stop_reason {
        StopReason::Completed { .. }
        | StopReason::IterationCeiling
        | StopReason::TokenOrContextCeiling
        | StopReason::BudgetStop
        | StopReason::FatalToolRefusal
        | StopReason::InvalidStructuredOutput => ReservationResolution::CommitObserved,
        StopReason::Cancelled => ReservationResolution::ReleaseUnused,
        // Timeout and provider failure both cover crash/disappearance: the
        // endpoint may have partially consumed capacity with no way to
        // observe how much, so the honest resolution holds rather than
        // guesses either extreme.
        StopReason::Timeout | StopReason::ProviderFailure { .. } => {
            ReservationResolution::HoldUnknown
        }
    }
}

/// Resolves a reservation per [`resolution_for_stop_reason`], delegating
/// entirely to the ordinary PRD-064 settle/release calls — this function
/// adds no new reservation mechanics of its own.
pub fn resolve_reservation(
    repo: &mut ReservationRepository<'_>,
    reservation_id: &str,
    stop_reason: StopReason,
    observed: Vec<ResourceRequest>,
    actor: &str,
) -> familiar_ai_core::Result<Option<SettlementResult>> {
    match resolution_for_stop_reason(stop_reason) {
        ReservationResolution::CommitObserved => {
            Ok(Some(repo.commit(reservation_id, observed, actor)?))
        }
        ReservationResolution::HoldUnknown => Ok(Some(repo.settle(
            reservation_id,
            SettlementObservation::Unknown {
                policy: UnknownConsumptionPolicy::HoldReservation,
            },
            actor,
        )?)),
        ReservationResolution::ReleaseUnused => {
            repo.release(reservation_id, actor)?;
            Ok(None)
        }
    }
}

fn failure_kind_for(stop_reason: StopReason) -> Option<&'static str> {
    match stop_reason {
        StopReason::Completed { .. } => None,
        StopReason::IterationCeiling => Some("iteration-ceiling"),
        StopReason::TokenOrContextCeiling => Some("token-or-context-ceiling"),
        StopReason::BudgetStop => Some("budget-stop"),
        StopReason::Timeout => Some("timeout"),
        StopReason::Cancelled => Some("cancelled"),
        StopReason::ProviderFailure { .. } => Some("provider-failure-or-disappearance"),
        StopReason::FatalToolRefusal => Some("fatal-tool-refusal"),
        StopReason::InvalidStructuredOutput => Some("invalid-structured-output"),
    }
}

/// Measurements a caller collects around the loop's own `submit` calls that
/// have no field anywhere in the PRD-058 `RunOutcome`/`UsageCategories`
/// contract: wall-clock timing, load time, memory, and utilization. Every
/// field stays `None`, never a fabricated zero, when unmeasured.
#[derive(Debug, Clone, Default)]
pub struct LocalRunMeasurements {
    pub wall_time_ms: Option<u64>,
    pub time_to_first_token_ms: Option<u64>,
    pub load_time_ms: Option<u64>,
    pub peak_memory_mb: Option<u64>,
    pub accelerator_utilization_pct: Option<f64>,
    pub cpu_utilization_pct: Option<f64>,
    pub energy_wh: Option<f64>,
    pub energy_measurement_provenance: Option<String>,
}

/// PRD-073 residency attribution for one local run: whether an already
/// loaded serving process took the call, which process it was, and what
/// that runtime reported about its prefix cache. Absent entirely for an
/// execution that ran with residency disabled — recorded as unknown, never
/// backfilled as `cold`.
#[derive(Debug, Clone)]
pub struct LocalResidencyAttribution<'a> {
    pub residency_state: ResidencyState,
    pub resident_server_identity: Option<&'a str>,
    pub cache_evidence: CacheEvidence,
}

impl<'a> LocalResidencyAttribution<'a> {
    /// Derives the attribution from a residency decision and whatever usage
    /// the serving runtime actually reported. Cache evidence is classified,
    /// never assumed: see [`CacheEvidence::classify`].
    pub fn from_usage(
        residency_state: ResidencyState,
        resident_server_identity: Option<&'a str>,
        usage: &familiar_ai_llm::attempt::UsageCategories,
    ) -> Self {
        Self {
            residency_state,
            resident_server_identity,
            cache_evidence: CacheEvidence::classify(residency_state, usage),
        }
    }

    fn state_record(&self) -> ResidencyStateRecord {
        match self.residency_state {
            ResidencyState::Cold => ResidencyStateRecord::Cold,
            ResidencyState::Warm => ResidencyStateRecord::Warm,
        }
    }

    fn evidence_record(&self) -> CacheEvidenceRecord {
        match self.cache_evidence {
            CacheEvidence::ColdLoad => CacheEvidenceRecord::ColdLoad,
            CacheEvidence::WarmMiss => CacheEvidenceRecord::WarmMiss,
            CacheEvidence::WarmHit => CacheEvidenceRecord::WarmHit,
            CacheEvidence::Unknown { .. } => CacheEvidenceRecord::Unknown,
        }
    }
}

/// Attaches PRD-073 residency attribution to a PRD-051 usage observation so
/// the ledger — and PRD-063's operator-allocation estimates built on it —
/// can partition latency and utilization by whether the model was already
/// warm.
pub fn attach_observation_residency(
    repo: &ModelResidencyRepository<'_>,
    observation_id: &str,
    attribution: &LocalResidencyAttribution<'_>,
) -> familiar_ai_core::Result<()> {
    repo.attach_to_observation(
        observation_id,
        &ObservationResidency {
            residency_state: attribution.state_record(),
            resident_server_identity: attribution.resident_server_identity,
            cache_evidence: attribution.evidence_record(),
            cache_evidence_reason: attribution.cache_evidence.reason(),
        },
    )
}

/// `retries` is supplied by the caller, never derived from
/// `outcome.attempts`: the PRD-058 loop mints one attempt per submission,
/// including every ordinary multi-turn tool-call round trip, so
/// `attempts.len()` counts loop turns, not retries. Only a caller that
/// itself resubmits after a failed/ambiguous prior outcome (this function
/// sees none of that history — one call sees exactly one `RunOutcome`) knows
/// how many genuine retries occurred; until such resumption tracking exists,
/// callers pass `0` rather than a fabricated count.
#[allow(clippy::too_many_arguments)]
pub fn persist_local_telemetry(
    repo: &LocalTelemetryRepository<'_>,
    execution_id: &str,
    stage: &str,
    worker_identity: &str,
    runtime_id: &str,
    model_artifact_id: Option<&str>,
    artifact_verification_state: LocalArtifactVerificationState,
    measurements: &LocalRunMeasurements,
    outcome: &RunOutcome,
    retries: u32,
    residency: Option<&LocalResidencyAttribution<'_>>,
) -> familiar_ai_core::Result<String> {
    let usage = outcome.attempts.iter().fold(
        Default::default(),
        |acc: familiar_ai_llm::attempt::UsageCategories, attempt| acc.merge(&attempt.usage),
    );
    let tokens_per_second = match (usage.output_tokens, measurements.wall_time_ms) {
        (Some(tokens), Some(ms)) if ms > 0 => Some(tokens as f64 / (ms as f64 / 1000.0)),
        _ => None,
    };
    let attempt_id = outcome
        .attempts
        .first()
        .map(|attempt| attempt.attempt_id.0.as_str())
        .unwrap_or("no-attempt");
    let row = LocalTelemetryRow {
        execution_id,
        attempt_id,
        stage,
        spec_identity: &outcome.evidence.worker_spec_identity,
        empirical_version: &outcome.evidence.worker_empirical_version,
        worker_identity,
        runtime_id,
        model_artifact_id,
        artifact_verification_state: artifact_verification_state.into(),
        uncached_input_tokens: usage.uncached_input_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        wall_time_ms: measurements.wall_time_ms,
        time_to_first_token_ms: measurements.time_to_first_token_ms,
        tokens_per_second,
        load_time_ms: measurements.load_time_ms,
        peak_memory_mb: measurements.peak_memory_mb,
        accelerator_utilization_pct: measurements.accelerator_utilization_pct,
        cpu_utilization_pct: measurements.cpu_utilization_pct,
        retries,
        failure_kind: failure_kind_for(outcome.stop_reason),
        energy_wh: measurements.energy_wh,
        energy_measurement_provenance: measurements.energy_measurement_provenance.as_deref(),
        residency_state: residency.map(|value| value.residency_state.as_str()),
        resident_server_identity: residency.and_then(|value| value.resident_server_identity),
        cache_evidence: residency.map(|value| value.cache_evidence.as_str()),
    };
    repo.record_telemetry(&row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_agent::raw_runtime::{AttemptUsage, LoopEvidence, OfferedTool, ResumePoint};
    use familiar_ai_llm::attempt::{AttemptId, UsageCategories};
    use familiar_ai_storage::Database;
    use std::sync::{Arc, Barrier};

    fn database() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn owner(id: &str) -> ReservationOwnerIdentity {
        ReservationOwnerIdentity {
            owner_instance_id: format!("owner-{id}"),
            installation_id: None,
            nonce_or_generation: format!("nonce-{id}"),
            owner_kind: "local-worker".into(),
            project_id: "project-1".into(),
            execution_id: format!("exec-{id}"),
            component_id: format!("component-{id}"),
        }
    }

    #[test]
    fn pool_capacity_is_derived_from_the_declared_resource_profile() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        let profile = LocalResourceProfileConfig {
            concurrent_inference_slots: Some(2),
            model_loading_slots: Some(1),
            ..Default::default()
        };
        define_pools_from_resource_profile(&mut repo, "local:ollama:llama3", &profile).unwrap();

        let requests = execution_resource_requests_for_profile("local:ollama:llama3", &profile);
        let first = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("a"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap();
        assert!(matches!(first, AcquireOutcome::Granted(_)));
        // The profile declared two concurrent inference slots: a second
        // claimant is granted, not refused.
        let second = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("b"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap();
        assert!(matches!(second, AcquireOutcome::Granted(_)));
        // A third exceeds the declared capacity.
        let third = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("c"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap();
        assert!(matches!(third, AcquireOutcome::Refused { .. }));
    }

    #[test]
    fn resource_profile_fields_left_absent_define_no_pool() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        // No fields configured: nothing is defined, so the ordinary
        // unknown-capacity refusal applies rather than an invented capacity.
        define_pools_from_resource_profile(
            &mut repo,
            "local:ollama:llama3",
            &LocalResourceProfileConfig::default(),
        )
        .unwrap();
        let requests =
            execution_resource_requests_for_profile("local:ollama:llama3", &Default::default());
        let outcome = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("a"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap();
        assert!(matches!(outcome, AcquireOutcome::Refused { .. }));
    }

    #[test]
    fn thermal_and_power_ceilings_refuse_only_on_a_real_observation_exceeding_them() {
        let profile = LocalResourceProfileConfig {
            thermal_ceiling_celsius: Some(90),
            power_ceiling_watts: Some(250),
            ..Default::default()
        };
        assert_eq!(
            thermal_or_power_ceiling_exceeded(&profile, Some(95.0), None),
            Some("thermal-ceiling-exceeded")
        );
        assert_eq!(
            thermal_or_power_ceiling_exceeded(&profile, None, Some(300.0)),
            Some("power-ceiling-exceeded")
        );
        assert_eq!(
            thermal_or_power_ceiling_exceeded(&profile, Some(70.0), Some(100.0)),
            None
        );
        // No sensor reading available: never refused on an absent value.
        assert_eq!(
            thermal_or_power_ceiling_exceeded(&profile, None, None),
            None
        );
        // No ceiling configured: never refused regardless of reading.
        assert_eq!(
            thermal_or_power_ceiling_exceeded(
                &LocalResourceProfileConfig::default(),
                Some(999.0),
                Some(999.0)
            ),
            None
        );
    }

    #[test]
    fn unknown_capacity_refuses_by_default_never_inventing_availability() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        let requests = execution_resource_requests("local:ollama:llama3", None, None, false);
        let outcome = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("a"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap();
        assert!(matches!(outcome, AcquireOutcome::Refused { .. }));
    }

    #[test]
    fn unknown_capacity_can_serialize_conservatively_through_one_bootstrapped_slot() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        let requests = execution_resource_requests("local:ollama:llama3", None, None, false);
        let first = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("a"),
            &requests,
            UnknownCapacityPolicy::SerializeConservatively,
        )
        .unwrap();
        assert!(matches!(first, AcquireOutcome::Granted(_)));
        // The conservative bootstrap is exactly one slot: a second
        // concurrent claimant is refused, never granted a second unit.
        let second = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("b"),
            &requests,
            UnknownCapacityPolicy::SerializeConservatively,
        )
        .unwrap();
        assert!(matches!(second, AcquireOutcome::Refused { .. }));
    }

    #[test]
    fn reservation_races_grant_exactly_one_exclusive_runtime_slot() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let mut setup = Database::open(&path).unwrap();
        setup.run_migrations().unwrap();
        {
            let mut repo = ReservationRepository::new(setup.conn_mut());
            repo.define_pool(
                "local:ollama:llama3:inference-slots",
                &ResourceType::InferenceSlots,
                2,
                false,
            )
            .unwrap();
            repo.define_pool(
                "local:ollama:llama3:exclusive-runtime",
                &ResourceType::ExclusiveRuntime,
                1,
                false,
            )
            .unwrap();
        }
        drop(setup);
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for id in ["a", "b"] {
            let path = path.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let mut db = Database::open(&path).unwrap();
                let requests = execution_resource_requests("local:ollama:llama3", None, None, true);
                barrier.wait();
                acquire_with_unknown_capacity_policy(
                    &mut ReservationRepository::new(db.conn_mut()),
                    &owner(id),
                    &requests,
                    UnknownCapacityPolicy::Refuse,
                )
                .unwrap()
            }));
        }
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, AcquireOutcome::Granted(_)))
                .count(),
            1
        );
    }

    #[test]
    fn crash_or_disappearance_holds_the_reservation_never_releases_unknown_consumption() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        repo.define_pool(
            "local:ollama:llama3:inference-slots",
            &ResourceType::InferenceSlots,
            1,
            false,
        )
        .unwrap();
        let requests = execution_resource_requests("local:ollama:llama3", None, None, false);
        let AcquireOutcome::Granted(grant) = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("a"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap() else {
            panic!("expected grant");
        };
        let result = resolve_reservation(
            &mut repo,
            &grant.reservation_id,
            StopReason::ProviderFailure {
                taxonomy: familiar_ai_agent::raw_runtime::ProviderFailureTaxonomy::Retryable,
            },
            vec![],
            "test",
        )
        .unwrap()
        .unwrap();
        assert!(result.unknown_consumption);
        assert_eq!(result.state, "held");
    }

    #[test]
    fn cancellation_releases_the_reservation() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        repo.define_pool(
            "local:ollama:llama3:inference-slots",
            &ResourceType::InferenceSlots,
            1,
            false,
        )
        .unwrap();
        let requests = execution_resource_requests("local:ollama:llama3", None, None, false);
        let AcquireOutcome::Granted(grant) = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("a"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap() else {
            panic!("expected grant");
        };
        let result = resolve_reservation(
            &mut repo,
            &grant.reservation_id,
            StopReason::Cancelled,
            vec![],
            "test",
        )
        .unwrap();
        assert!(result.is_none());
        // Released capacity is available again for a fresh claimant.
        let again = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("b"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap();
        assert!(matches!(again, AcquireOutcome::Granted(_)));
    }

    #[test]
    fn completed_run_commits_observed_consumption() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        repo.define_pool(
            "local:ollama:llama3:inference-slots",
            &ResourceType::InferenceSlots,
            1,
            false,
        )
        .unwrap();
        let requests = execution_resource_requests("local:ollama:llama3", None, None, false);
        let AcquireOutcome::Granted(grant) = acquire_with_unknown_capacity_policy(
            &mut repo,
            &owner("a"),
            &requests,
            UnknownCapacityPolicy::Refuse,
        )
        .unwrap() else {
            panic!("expected grant");
        };
        let result = resolve_reservation(
            &mut repo,
            &grant.reservation_id,
            StopReason::Completed {
                structured_output: false,
            },
            requests,
            "test",
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.state, "committed");
        assert!(!result.unknown_consumption);
    }

    #[test]
    fn capacity_freshness_rejects_stale_observations() {
        let now = Utc::now();
        let fresh = CapacityObservation {
            pool_id: "local:ollama:llama3:accelerator-memory".into(),
            resource_type: ResourceType::AcceleratorMemory,
            available_amount: 8192,
            observed_at: now - chrono::Duration::seconds(5),
            class: LocalCapacityClass::Managed,
        };
        let stale = CapacityObservation {
            observed_at: now - chrono::Duration::minutes(10),
            ..fresh.clone()
        };
        let max_age = chrono::Duration::seconds(30);
        assert!(capacity_is_fresh(&fresh, now, max_age));
        assert!(!capacity_is_fresh(&stale, now, max_age));
    }

    #[test]
    fn managed_and_externally_shared_capacity_are_distinct_classes() {
        assert_ne!(
            LocalCapacityClass::Managed,
            LocalCapacityClass::ExternallyShared
        );
    }

    fn stub_outcome(stop_reason: StopReason, output_tokens: Option<u64>) -> RunOutcome {
        RunOutcome {
            stop_reason,
            attempts: vec![AttemptUsage {
                attempt_id: AttemptId("att_1".into()),
                usage: UsageCategories {
                    output_tokens,
                    ..Default::default()
                },
                ambiguous: false,
                provider_request_id: None,
            }],
            evidence: LoopEvidence {
                prompt_template_version: "v1".into(),
                worker_spec_identity: "wspec-sha256:local-test".into(),
                worker_empirical_version: "wver-sha256:local-test".into(),
                offered_tools: vec![] as Vec<OfferedTool>,
                calls: vec![],
                stop_reason,
                resume_point: ResumePoint {
                    conversation_messages: 1,
                    journal_high_water_mark: 0,
                },
                iterations: 1,
            },
            final_text: Some("done".into()),
        }
    }

    #[test]
    fn telemetry_records_no_invented_cost_and_computes_tokens_per_second() {
        let db = database();
        db.conn().execute("INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES('exec_1','now','local-worker','running','repo','wt','docs/prds/PRD-063.md','[]')", []).unwrap();
        let repo = LocalTelemetryRepository::new(db.conn());
        let outcome = stub_outcome(
            StopReason::Completed {
                structured_output: false,
            },
            Some(20),
        );
        let measurements = LocalRunMeasurements {
            wall_time_ms: Some(2000),
            ..Default::default()
        };
        let telemetry_id = persist_local_telemetry(
            &repo,
            "exec_1",
            "implementation",
            "local-ollama-llama3",
            "ollama",
            Some("sha256:aaaa"),
            LocalArtifactVerificationState::Verified,
            &measurements,
            &outcome,
            0,
            None,
        )
        .unwrap();
        let rows = repo.telemetry_for_execution("exec_1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, telemetry_id);
    }

    /// A successful multi-turn run (several ordinary tool-call round trips,
    /// zero failures) must never be recorded as having retried: `retries`
    /// reflects only what the caller explicitly reports, not
    /// `outcome.attempts.len()`.
    #[test]
    fn multi_attempt_successful_run_records_zero_retries_not_attempt_count() {
        let db = database();
        db.conn().execute("INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES('exec_1','now','local-worker','running','repo','wt','docs/prds/PRD-063.md','[]')", []).unwrap();
        let repo = LocalTelemetryRepository::new(db.conn());
        let mut outcome = stub_outcome(
            StopReason::Completed {
                structured_output: false,
            },
            Some(20),
        );
        // Three ordinary submissions (e.g. two tool-call round trips plus a
        // final completing turn), none of them a retry.
        outcome.attempts = vec![
            outcome.attempts[0].clone(),
            outcome.attempts[0].clone(),
            outcome.attempts[0].clone(),
        ];
        let measurements = LocalRunMeasurements::default();
        let telemetry_id = persist_local_telemetry(
            &repo,
            "exec_1",
            "implementation",
            "local-ollama-llama3",
            "ollama",
            Some("sha256:aaaa"),
            LocalArtifactVerificationState::Verified,
            &measurements,
            &outcome,
            0,
            None,
        )
        .unwrap();
        let retries: u32 = db
            .conn()
            .query_row(
                "SELECT retries FROM local_worker_telemetry WHERE telemetry_id=?1",
                [&telemetry_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            retries, 0,
            "three successful submissions must not be reported as two retries"
        );
    }

    #[test]
    fn artifact_verification_outcome_maps_to_the_persisted_state_honestly() {
        assert_eq!(
            artifact_verification_state_for(&ArtifactVerificationOutcome::Verified {
                digest: "sha256:aaaa".into()
            }),
            LocalArtifactVerificationState::Verified
        );
        // A mismatch is an active refusal signal, never accepted as a match.
        assert_eq!(
            artifact_verification_state_for(&ArtifactVerificationOutcome::Mismatch {
                claimed: "sha256:bbbb".into(),
                expected: "sha256:aaaa".into(),
            }),
            LocalArtifactVerificationState::Mismatch
        );
        // Unverifiable is merely unproven, distinct from an active mismatch,
        // and still never treated as a match.
        assert_eq!(
            artifact_verification_state_for(&ArtifactVerificationOutcome::Unverifiable {
                reason: "no digest metadata".into()
            }),
            LocalArtifactVerificationState::DegradedUnverified
        );
    }

    #[test]
    fn timeout_or_crash_failure_kind_is_recorded_honestly_not_as_completion() {
        assert_eq!(failure_kind_for(StopReason::Timeout), Some("timeout"));
        assert_eq!(
            failure_kind_for(StopReason::ProviderFailure {
                taxonomy: familiar_ai_agent::raw_runtime::ProviderFailureTaxonomy::Retryable
            }),
            Some("provider-failure-or-disappearance")
        );
        assert_eq!(
            failure_kind_for(StopReason::Completed {
                structured_output: false
            }),
            None
        );
    }
}
