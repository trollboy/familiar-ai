//! PRD-063 local worker telemetry persistence (migration 057). Typed,
//! timestamped observations against the full PRD-057 spec identity — never
//! a USD cost. Operator-allocation cost estimates are a distinct,
//! explicitly opt-in class stored separately and only ever produced for an
//! `enabled` policy.

use chrono::Utc;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection, OptionalExtension};

use familiar_ai_core::FamiliarError;

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

fn random_hex() -> familiar_ai_core::Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new().fill(&mut bytes).map_err(|_| {
        FamiliarError::Database("secure local-telemetry id generation failed".into())
    })?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Which of the closed artifact verification outcomes a telemetry row
/// records. `Mismatch` is distinct from `DegradedUnverified` — a mismatch is
/// an active refusal signal, an unverifiable endpoint is merely unproven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalArtifactVerificationState {
    Verified,
    DegradedUnverified,
    Mismatch,
}

impl LocalArtifactVerificationState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::DegradedUnverified => "degraded-unverified",
            Self::Mismatch => "mismatch",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "verified" => Self::Verified,
            "degraded-unverified" => Self::DegradedUnverified,
            "mismatch" => Self::Mismatch,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalTelemetryRow<'a> {
    pub execution_id: &'a str,
    pub attempt_id: &'a str,
    pub stage: &'a str,
    pub spec_identity: &'a str,
    pub empirical_version: &'a str,
    pub worker_identity: &'a str,
    pub runtime_id: &'a str,
    pub model_artifact_id: Option<&'a str>,
    pub artifact_verification_state: LocalArtifactVerificationStateOpt,
    pub uncached_input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub wall_time_ms: Option<u64>,
    pub time_to_first_token_ms: Option<u64>,
    pub tokens_per_second: Option<f64>,
    pub load_time_ms: Option<u64>,
    pub peak_memory_mb: Option<u64>,
    pub accelerator_utilization_pct: Option<f64>,
    pub cpu_utilization_pct: Option<f64>,
    pub retries: u32,
    pub failure_kind: Option<&'a str>,
    pub energy_wh: Option<f64>,
    pub energy_measurement_provenance: Option<&'a str>,
    /// PRD-073: whether this run's serving process was already loaded
    /// (`warm`) or had to be loaded for it (`cold`). `None` is the honest
    /// record for an execution that predates residency or ran with
    /// residency disabled — never backfilled as `cold`.
    pub residency_state: Option<&'a str>,
    /// The resident server instance that served this run, when one did.
    pub resident_server_identity: Option<&'a str>,
    /// What the serving runtime reported about its prefix cache. `unknown`
    /// where it reports nothing; never inferred from residency state.
    pub cache_evidence: Option<&'a str>,
}

/// Newtype so `LocalTelemetryRow` can `#[derive(Default)]` even though the
/// verification state has no meaningful default value; callers must set it
/// explicitly via `LocalArtifactVerificationState`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalArtifactVerificationStateOpt(pub Option<LocalArtifactVerificationState>);

impl From<LocalArtifactVerificationState> for LocalArtifactVerificationStateOpt {
    fn from(value: LocalArtifactVerificationState) -> Self {
        Self(Some(value))
    }
}

pub struct AllocationPolicy<'a> {
    pub policy_id: &'a str,
    pub policy_version: &'a str,
    pub kind: &'a str,
    pub currency: &'a str,
    pub declared_assumptions_json: &'a str,
    pub enabled: bool,
}

#[allow(clippy::too_many_arguments)]
pub struct AllocationEstimateInput<'a> {
    pub policy_id: &'a str,
    pub policy_version: &'a str,
    pub amount_nanocurrency: i64,
    pub currency: &'a str,
    pub effective_period_start: &'a str,
    pub effective_period_end: &'a str,
    pub input_measurements_json: &'a str,
    pub declared_assumptions_json: &'a str,
    pub provenance: &'a str,
}

pub struct LocalTelemetryRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LocalTelemetryRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_telemetry(
        &self,
        row: &LocalTelemetryRow<'_>,
    ) -> familiar_ai_core::Result<String> {
        let state = row
            .artifact_verification_state
            .0
            .ok_or_else(|| {
                FamiliarError::Config(
                    "local telemetry requires an artifact verification state".into(),
                )
            })?
            .as_str();
        let telemetry_id = format!("ltel_{}", random_hex()?);
        self.conn.execute(
            "INSERT INTO local_worker_telemetry(
                telemetry_id,execution_id,attempt_id,stage,spec_identity,empirical_version,
                worker_identity,runtime_id,model_artifact_id,artifact_verification_state,
                uncached_input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,reasoning_output_tokens,
                wall_time_ms,time_to_first_token_ms,tokens_per_second,load_time_ms,peak_memory_mb,
                accelerator_utilization_pct,cpu_utilization_pct,retries,failure_kind,
                energy_wh,energy_measurement_provenance,recorded_at,
                residency_state,resident_server_identity,cache_evidence
            ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30)",
            params![
                telemetry_id,
                row.execution_id,
                row.attempt_id,
                row.stage,
                row.spec_identity,
                row.empirical_version,
                row.worker_identity,
                row.runtime_id,
                row.model_artifact_id,
                state,
                row.uncached_input_tokens,
                row.cache_read_tokens,
                row.cache_write_tokens,
                row.output_tokens,
                row.reasoning_output_tokens,
                row.wall_time_ms,
                row.time_to_first_token_ms,
                row.tokens_per_second,
                row.load_time_ms,
                row.peak_memory_mb,
                row.accelerator_utilization_pct,
                row.cpu_utilization_pct,
                row.retries,
                row.failure_kind,
                row.energy_wh,
                row.energy_measurement_provenance,
                Utc::now().to_rfc3339(),
                row.residency_state,
                row.resident_server_identity,
                row.cache_evidence,
            ],
        ).map_err(db)?;
        Ok(telemetry_id)
    }

    pub fn telemetry_for_execution(
        &self,
        execution_id: &str,
    ) -> familiar_ai_core::Result<Vec<(String, String, Option<u64>)>> {
        let mut statement = self.conn.prepare(
            "SELECT telemetry_id,attempt_id,wall_time_ms FROM local_worker_telemetry WHERE execution_id=?1 ORDER BY recorded_at",
        ).map_err(db)?;
        let rows = statement
            .query_map([execution_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    pub fn verification_state_for(
        &self,
        telemetry_id: &str,
    ) -> familiar_ai_core::Result<Option<LocalArtifactVerificationState>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT artifact_verification_state FROM local_worker_telemetry WHERE telemetry_id=?1",
                [telemetry_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        Ok(raw.and_then(|value| LocalArtifactVerificationState::parse(&value)))
    }

    /// Registers (or replays, if identical) an allocation policy. A policy
    /// is disabled by default and never enabled implicitly — the caller
    /// must set `enabled: true` explicitly to activate it.
    pub fn register_allocation_policy(
        &self,
        policy: &AllocationPolicy<'_>,
    ) -> familiar_ai_core::Result<()> {
        let existing: Option<(String, String, String, bool)> = self
            .conn
            .query_row(
                "SELECT kind,currency,declared_assumptions_json,enabled FROM local_allocation_policies WHERE policy_id=?1 AND policy_version=?2",
                params![policy.policy_id, policy.policy_version],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(db)?;
        if let Some(existing) = existing {
            let requested = (
                policy.kind.to_owned(),
                policy.currency.to_owned(),
                policy.declared_assumptions_json.to_owned(),
                policy.enabled,
            );
            if existing != requested {
                return Err(FamiliarError::Config(format!(
                    "allocation policy {:?} version {:?} is already registered with a different body; register a new version, or call set_allocation_policy_enabled to change only activation",
                    policy.policy_id, policy.policy_version
                )));
            }
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO local_allocation_policies(policy_id,policy_version,kind,currency,declared_assumptions_json,enabled,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                policy.policy_id,
                policy.policy_version,
                policy.kind,
                policy.currency,
                policy.declared_assumptions_json,
                policy.enabled,
                Utc::now().to_rfc3339(),
            ],
        ).map_err(db)?;
        Ok(())
    }

    /// The only supported in-place policy mutation. Policy meaning remains
    /// immutable under `(policy_id, policy_version)`; activation is an
    /// explicit operator switch and may be turned off without inventing a
    /// replacement version solely to stop producing estimates.
    pub fn set_allocation_policy_enabled(
        &self,
        policy_id: &str,
        policy_version: &str,
        enabled: bool,
    ) -> familiar_ai_core::Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE local_allocation_policies SET enabled=?1 WHERE policy_id=?2 AND policy_version=?3",
                params![enabled, policy_id, policy_version],
            )
            .map_err(db)?;
        if changed == 0 {
            return Err(FamiliarError::Config(format!(
                "allocation policy {policy_id:?} version {policy_version:?} is not registered"
            )));
        }
        Ok(())
    }

    /// Records one operator-allocation cost estimate against a telemetry
    /// row. Fails closed when the named policy is missing or disabled —
    /// allocation estimates are never produced from an implicitly-enabled
    /// policy.
    pub fn record_allocation_estimate(
        &self,
        telemetry_id: &str,
        estimate: &AllocationEstimateInput<'_>,
    ) -> familiar_ai_core::Result<String> {
        let enabled: Option<bool> = self
            .conn
            .query_row(
                "SELECT enabled FROM local_allocation_policies WHERE policy_id=?1 AND policy_version=?2",
                params![estimate.policy_id, estimate.policy_version],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        match enabled {
            Some(true) => {}
            Some(false) => {
                return Err(FamiliarError::Config(
                    "operator-allocation policy is registered but not enabled; estimates are never produced implicitly".into(),
                ))
            }
            None => {
                return Err(FamiliarError::Config(
                    "operator-allocation policy is not registered".into(),
                ))
            }
        }
        let estimate_id = format!("lest_{}", random_hex()?);
        self.conn.execute(
            "INSERT INTO local_allocation_estimates(
                estimate_id,telemetry_id,policy_id,policy_version,cost_category,amount_nanocurrency,currency,
                effective_period_start,effective_period_end,input_measurements_json,declared_assumptions_json,
                provenance,estimated_authority_label,created_at
            ) VALUES(?1,?2,?3,?4,'operator-allocation',?5,?6,?7,?8,?9,?10,?11,'estimated',?12)",
            params![
                estimate_id,
                telemetry_id,
                estimate.policy_id,
                estimate.policy_version,
                estimate.amount_nanocurrency,
                estimate.currency,
                estimate.effective_period_start,
                estimate.effective_period_end,
                estimate.input_measurements_json,
                estimate.declared_assumptions_json,
                estimate.provenance,
                Utc::now().to_rfc3339(),
            ],
        ).map_err(db)?;
        Ok(estimate_id)
    }

    pub fn allocation_estimates_for_telemetry(
        &self,
        telemetry_id: &str,
    ) -> familiar_ai_core::Result<Vec<(String, i64, String)>> {
        let mut statement = self.conn.prepare(
            "SELECT estimate_id,amount_nanocurrency,currency FROM local_allocation_estimates WHERE telemetry_id=?1 ORDER BY created_at",
        ).map_err(db)?;
        let rows = statement
            .query_map([telemetry_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> crate::Database {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn seed_execution(conn: &Connection, execution_id: &str) {
        conn.execute(
            "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES(?1,'now','local-worker','running','repo','wt','docs/prds/PRD-063.md','[]')",
            [execution_id],
        ).unwrap();
    }

    fn base_row<'a>(execution_id: &'a str) -> LocalTelemetryRow<'a> {
        LocalTelemetryRow {
            execution_id,
            attempt_id: "att_1",
            stage: "implementation",
            spec_identity: "wspec-sha256:test",
            empirical_version: "wver-sha256:test",
            worker_identity: "local-ollama-llama3",
            runtime_id: "ollama",
            model_artifact_id: Some("sha256:aaaa"),
            artifact_verification_state: LocalArtifactVerificationState::Verified.into(),
            uncached_input_tokens: Some(100),
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: Some(20),
            reasoning_output_tokens: None,
            wall_time_ms: Some(1200),
            time_to_first_token_ms: Some(80),
            tokens_per_second: Some(16.5),
            load_time_ms: Some(300),
            peak_memory_mb: Some(4096),
            accelerator_utilization_pct: Some(72.5),
            cpu_utilization_pct: Some(12.0),
            retries: 0,
            failure_kind: None,
            energy_wh: None,
            energy_measurement_provenance: None,
            residency_state: None,
            resident_server_identity: None,
            cache_evidence: None,
        }
    }

    #[test]
    fn records_and_reads_back_telemetry_with_no_invented_cost() {
        let db = database();
        seed_execution(db.conn(), "exec_1");
        let repo = LocalTelemetryRepository::new(db.conn());
        let telemetry_id = repo.record_telemetry(&base_row("exec_1")).unwrap();
        let rows = repo.telemetry_for_execution("exec_1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, telemetry_id);
        assert_eq!(rows[0].2, Some(1200));
        assert_eq!(
            repo.verification_state_for(&telemetry_id).unwrap(),
            Some(LocalArtifactVerificationState::Verified)
        );
    }

    #[test]
    fn degraded_and_mismatch_verification_states_round_trip() {
        let db = database();
        seed_execution(db.conn(), "exec_1");
        let repo = LocalTelemetryRepository::new(db.conn());
        let mut degraded = base_row("exec_1");
        degraded.artifact_verification_state =
            LocalArtifactVerificationState::DegradedUnverified.into();
        let telemetry_id = repo.record_telemetry(&degraded).unwrap();
        assert_eq!(
            repo.verification_state_for(&telemetry_id).unwrap(),
            Some(LocalArtifactVerificationState::DegradedUnverified)
        );

        let mut mismatch = base_row("exec_1");
        mismatch.attempt_id = "att_2";
        mismatch.artifact_verification_state = LocalArtifactVerificationState::Mismatch.into();
        let telemetry_id = repo.record_telemetry(&mismatch).unwrap();
        assert_eq!(
            repo.verification_state_for(&telemetry_id).unwrap(),
            Some(LocalArtifactVerificationState::Mismatch)
        );
    }

    #[test]
    fn allocation_estimate_requires_an_explicitly_enabled_policy() {
        let db = database();
        seed_execution(db.conn(), "exec_1");
        let repo = LocalTelemetryRepository::new(db.conn());
        let telemetry_id = repo.record_telemetry(&base_row("exec_1")).unwrap();

        let estimate = AllocationEstimateInput {
            policy_id: "electricity-home-office",
            policy_version: "v1",
            amount_nanocurrency: 1_500_000,
            currency: "USD",
            effective_period_start: "2026-09-01T00:00:00Z",
            effective_period_end: "2026-09-02T00:00:00Z",
            input_measurements_json: r#"{"kwh":0.05}"#,
            declared_assumptions_json: r#"{"rate_usd_per_kwh":0.15}"#,
            provenance: "operator-configured",
        };

        // No policy registered at all: fails closed.
        assert!(repo
            .record_allocation_estimate(&telemetry_id, &estimate)
            .is_err());

        // Registered but disabled (the default): still fails closed.
        repo.register_allocation_policy(&AllocationPolicy {
            policy_id: "electricity-home-office",
            policy_version: "v1",
            kind: "electricity",
            currency: "USD",
            declared_assumptions_json: r#"{"rate_usd_per_kwh":0.15}"#,
            enabled: false,
        })
        .unwrap();
        let error = repo
            .record_allocation_estimate(&telemetry_id, &estimate)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not enabled"), "{error}");

        // Explicitly enabled: succeeds and the row carries the closed
        // operator-allocation cost category and estimated-authority label.
        repo.set_allocation_policy_enabled("electricity-home-office", "v1", true)
            .unwrap();
        let estimate_id = repo
            .record_allocation_estimate(&telemetry_id, &estimate)
            .unwrap();
        let rows = repo
            .allocation_estimates_for_telemetry(&telemetry_id)
            .unwrap();
        assert_eq!(rows, vec![(estimate_id, 1_500_000, "USD".to_string())]);
    }

    #[test]
    fn policy_reregistration_is_idempotent_only_for_an_identical_body() {
        let db = database();
        let repo = LocalTelemetryRepository::new(db.conn());
        let policy = AllocationPolicy {
            policy_id: "power",
            policy_version: "v1",
            kind: "electricity",
            currency: "USD",
            declared_assumptions_json: r#"{"rate":1}"#,
            enabled: true,
        };
        repo.register_allocation_policy(&policy).unwrap();
        repo.register_allocation_policy(&policy).unwrap();

        let divergent = AllocationPolicy {
            currency: "EUR",
            ..policy
        };
        let error = repo.register_allocation_policy(&divergent).unwrap_err();
        assert!(error.to_string().contains("different body"), "{error}");

        repo.set_allocation_policy_enabled("power", "v1", false)
            .unwrap();
        let enabled: bool = db
            .conn()
            .query_row(
                "SELECT enabled FROM local_allocation_policies WHERE policy_id='power' AND policy_version='v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!enabled);
    }

    #[test]
    fn allocation_estimates_never_mix_with_provider_cost_estimates_table() {
        let db = database();
        let table_exists: i64 = db
            .conn()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('cost_estimates','local_allocation_estimates')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            table_exists, 2,
            "allocation estimates must be a distinct table from provider cost_estimates"
        );
    }

    #[test]
    fn telemetry_is_append_only() {
        let db = database();
        seed_execution(db.conn(), "exec_1");
        let repo = LocalTelemetryRepository::new(db.conn());
        let telemetry_id = repo.record_telemetry(&base_row("exec_1")).unwrap();
        assert!(db
            .conn()
            .execute(
                "UPDATE local_worker_telemetry SET wall_time_ms=1 WHERE telemetry_id=?1",
                [&telemetry_id],
            )
            .is_err());
        assert!(db
            .conn()
            .execute(
                "DELETE FROM local_worker_telemetry WHERE telemetry_id=?1",
                [&telemetry_id],
            )
            .is_err());
    }
}
