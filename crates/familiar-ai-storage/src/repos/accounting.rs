use chrono::Utc;
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection, OptionalExtension};

use familiar_ai_core::FamiliarError;

#[derive(Debug, Clone)]
pub struct UsageObservation<'a> {
    pub execution_id: &'a str,
    pub attempt_id: &'a str,
    pub stage: &'a str,
    pub session_id: Option<&'a str>,
    pub worker_identity: &'a str,
    pub adapter: &'a str,
    pub cli_version: Option<&'a str>,
    pub model_identity: Option<&'a str>,
    pub service_tier: Option<&'a str>,
    pub provider_request_id: Option<&'a str>,
    pub uncached_input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub unknown_reason: Option<&'a str>,
    pub period_start: &'a str,
    pub period_end: &'a str,
    pub terminal_status: &'a str,
    pub source_event_hash: &'a str,
    pub provider_cost_lexical: Option<&'a str>,
    /// Canonical Git common-directory evidence; absent is explicitly degraded.
    pub project_resolution_evidence: Option<&'a str>,
}

pub struct AccountingRepository<'a> {
    conn: &'a Connection,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LedgerUsageSummary {
    pub observations: u64,
    pub unknown_observations: u64,
    pub uncached_input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub known_nanousd: u64,
    pub vendor_reported_estimates: u64,
    pub configured_rate_estimates: u64,
    pub known_zero_estimates: u64,
}

impl<'a> AccountingRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn project_id(&self, evidence: &str) -> familiar_ai_core::Result<String> {
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT project_id FROM project_identities WHERE resolution_evidence=?1",
                [evidence],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?
        {
            return Ok(id);
        }
        let id = format!("prj_{}", random_hex()?);
        self.conn.execute("INSERT OR IGNORE INTO project_identities(resolution_evidence,project_id,issued_at) VALUES(?1,?2,?3)", params![evidence,id,Utc::now().to_rfc3339()]).map_err(db)?;
        self.conn
            .query_row(
                "SELECT project_id FROM project_identities WHERE resolution_evidence=?1",
                [evidence],
                |r| r.get(0),
            )
            .map_err(db)
    }

    /// Idempotency is keyed by the sanitized source-event hash. Evidence and
    /// its observation commit atomically; replay returns the existing
    /// observation so a missing idempotent cost fact can still be repaired.
    pub fn append_observation(
        &self,
        value: &UsageObservation<'_>,
    ) -> familiar_ai_core::Result<Option<String>> {
        if let Some(existing) = self
            .conn
            .query_row(
                "SELECT observation_id FROM usage_observations observation JOIN accounting_evidence evidence ON evidence.evidence_id=observation.evidence_id WHERE evidence.source_event_hash=?1",
                [value.source_event_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?
        {
            return Ok(Some(existing));
        }
        let evidence_id = format!("evi_{}", random_hex()?);
        let observation_id = format!("obs_{}", random_hex()?);
        let now = Utc::now().to_rfc3339();
        let project_id = value
            .project_resolution_evidence
            .map(|e| self.project_id(e))
            .transpose()?;
        let degraded = project_id
            .is_none()
            .then_some("git-common-directory-unavailable");
        let usage_json = serde_json::json!({"uncached_input_tokens":value.uncached_input_tokens,"cache_read_tokens":value.cache_read_tokens,"cache_write_tokens":value.cache_write_tokens,"output_tokens":value.output_tokens,"reasoning_output_tokens":value.reasoning_output_tokens}).to_string();
        let tx = self.conn.unchecked_transaction().map_err(db)?;
        tx.execute("INSERT INTO accounting_evidence(evidence_id,execution_id,adapter,cli_version,model_identity,provider_session_id,provider_request_id,usage_json,provider_cost_lexical,observed_at,terminal_status,source_event_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", params![evidence_id,value.execution_id,value.adapter,value.cli_version,value.model_identity,value.session_id,value.provider_request_id,usage_json,value.provider_cost_lexical,value.period_end,value.terminal_status,value.source_event_hash]).map_err(db)?;
        tx.execute("INSERT INTO usage_observations(observation_id,evidence_id,project_id,degraded_identity,execution_id,attempt_id,stage,session_id,worker_identity,adapter,model_identity,service_tier,provider_request_id,uncached_input_tokens,cache_read_tokens,cache_write_tokens,output_tokens,reasoning_output_tokens,unknown_reason,period_start,period_end,observed_at,ingested_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?21,?22)", params![observation_id,evidence_id,project_id,degraded,value.execution_id,value.attempt_id,value.stage,value.session_id,value.worker_identity,value.adapter,value.model_identity,value.service_tier,value.provider_request_id,value.uncached_input_tokens,value.cache_read_tokens,value.cache_write_tokens,value.output_tokens,value.reasoning_output_tokens,value.unknown_reason,value.period_start,value.period_end,now]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(Some(observation_id))
    }

    pub fn append_vendor_estimate(
        &self,
        observation_id: &str,
        lexical_usd: &str,
    ) -> familiar_ai_core::Result<()> {
        let amount = decimal_nanousd(lexical_usd)
            .ok_or_else(|| FamiliarError::Database("invalid provider USD lexical amount".into()))?;
        self.conn.execute("INSERT OR IGNORE INTO cost_estimates(estimate_id,observation_id,billing_mode,provenance,unit,amount,lexical_amount,created_at) VALUES(?1,?2,'local-estimate','vendor-reported','nanoUSD',?3,?4,?5)", params![format!("est_{}",random_hex()?),observation_id,amount,lexical_usd,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    pub fn append_legacy_configured_estimate(
        &self,
        observation_id: &str,
        model: &str,
        amount_microusd: u64,
        rates_json: &str,
    ) -> familiar_ai_core::Result<()> {
        let schedule_id = "legacy-execution-history-pricing";
        let now = Utc::now().to_rfc3339();
        self.conn.execute("INSERT OR IGNORE INTO price_schedules(schedule_id,effective_at,currency,calculation_version,rates_json,created_at) VALUES(?1,'legacy','USD','legacy-microusd-v1',?2,?3)", params![schedule_id,rates_json,now]).map_err(db)?;
        let amount = amount_microusd
            .checked_mul(1000)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or_else(|| FamiliarError::Database("nanoUSD arithmetic overflow".into()))?;
        self.conn.execute("INSERT OR IGNORE INTO cost_estimates(estimate_id,observation_id,billing_mode,provenance,unit,amount,schedule_id,calculation_version,lexical_amount,created_at) VALUES(?1,?2,'local-estimate','configured-rate','nanoUSD',?3,?4,'legacy-microusd-v1',?5,?6)", params![format!("est_{}",random_hex()?),observation_id,amount,schedule_id,format!("model={model};microusd={amount_microusd}"),now]).map_err(db)?;
        Ok(())
    }

    pub fn usage(&self) -> familiar_ai_core::Result<LedgerUsageSummary> {
        let tokens = self.conn.query_row("SELECT count(*),coalesce(sum(CASE WHEN unknown_reason IS NOT NULL THEN 1 ELSE 0 END),0),coalesce(sum(uncached_input_tokens),0),coalesce(sum(cache_read_tokens),0),coalesce(sum(cache_write_tokens),0),coalesce(sum(output_tokens),0),coalesce(sum(reasoning_output_tokens),0) FROM usage_observations", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).map_err(db)?;
        let costs = self.conn.query_row("SELECT coalesce(sum(CASE WHEN unit='nanoUSD' THEN amount ELSE 0 END),0),coalesce(sum(CASE WHEN provenance='vendor-reported' THEN 1 ELSE 0 END),0),coalesce(sum(CASE WHEN provenance='configured-rate' THEN 1 ELSE 0 END),0),coalesce(sum(CASE WHEN provenance='known-zero' THEN 1 ELSE 0 END),0) FROM cost_estimates", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(db)?;
        Ok(LedgerUsageSummary {
            observations: tokens.0,
            unknown_observations: tokens.1,
            uncached_input_tokens: tokens.2,
            cache_read_tokens: tokens.3,
            cache_write_tokens: tokens.4,
            output_tokens: tokens.5,
            reasoning_output_tokens: tokens.6,
            known_nanousd: costs.0,
            vendor_reported_estimates: costs.1,
            configured_rate_estimates: costs.2,
            known_zero_estimates: costs.3,
        })
    }
}

fn random_hex() -> familiar_ai_core::Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| FamiliarError::Database("secure project-id generation failed".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn decimal_nanousd(value: &str) -> Option<u64> {
    let (whole, frac) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut n = whole.parse::<u64>().ok()?.checked_mul(1_000_000_000)?;
    let kept = &frac[..frac.len().min(9)];
    if !kept.is_empty() {
        n = n.checked_add(
            kept.parse::<u64>()
                .ok()?
                .checked_mul(10_u64.pow((9 - kept.len()) as u32))?,
        )?;
    }
    let round = match frac.as_bytes().get(9) {
        None | Some(b'0'..=b'4') => false,
        Some(b'6'..=b'9') => true,
        Some(b'5') => {
            frac.as_bytes()
                .get(10..)
                .is_some_and(|x| x.iter().any(|b| *b != b'0'))
                || n % 2 == 1
        }
        _ => return None,
    };
    n.checked_add(u64::from(round))
        .filter(|v| *v <= i64::MAX as u64)
}
fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionHistoryRepository, ExecutionStart};
    use std::collections::BTreeMap;

    #[test]
    fn observations_are_distinct_idempotent_exact_and_append_only() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let schema: String = db
            .conn()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='execution_history'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(schema.contains("execution_id TEXT PRIMARY KEY"));
        assert!(schema.contains("started_at TEXT NOT NULL"));
        assert!(schema.contains("outcome IN"));
        assert!(!schema.contains("agent = 'codex'"));
        ExecutionHistoryRepository::new(db.conn())
            .insert_running(&ExecutionStart {
                execution_id: "execution".into(),
                started_at: "2026-08-30T00:00:00Z".into(),
                repository: "/repo".into(),
                worktree: "/repo".into(),
                git_commit: None,
                prd_path: "docs/prds/PRD-051.md".into(),
                unavailable_fields: BTreeMap::new(),
            })
            .unwrap();
        let repo = AccountingRepository::new(db.conn());
        let value = UsageObservation {
            execution_id: "execution",
            attempt_id: "attempt-1",
            stage: "implementation",
            session_id: Some("safe-session"),
            worker_identity: "anthropic/claude",
            adapter: "claude-code",
            cli_version: Some("claude 1"),
            model_identity: Some("claude"),
            service_tier: None,
            provider_request_id: None,
            uncached_input_tokens: Some(1),
            cache_read_tokens: Some(2),
            cache_write_tokens: Some(3),
            output_tokens: Some(4),
            reasoning_output_tokens: Some(5),
            unknown_reason: None,
            period_start: "2026-08-30T00:00:00Z",
            period_end: "2026-08-30T00:00:01Z",
            terminal_status: "timed_out",
            source_event_hash: "sha256:fixture",
            provider_cost_lexical: Some("0.0000000015"),
            project_resolution_evidence: Some("/machine/git/common"),
        };
        let observation = repo.append_observation(&value).unwrap().unwrap();
        assert_eq!(
            repo.append_observation(&value).unwrap().as_deref(),
            Some(observation.as_str())
        );
        repo.append_vendor_estimate(&observation, "0.0000000015")
            .unwrap();
        repo.append_vendor_estimate(&observation, "0.0000000015")
            .unwrap();
        let summary = repo.usage().unwrap();
        assert_eq!(summary.uncached_input_tokens, 1);
        assert_eq!(summary.cache_read_tokens, 2);
        assert_eq!(summary.cache_write_tokens, 3);
        assert_eq!(summary.reasoning_output_tokens, 5);
        assert_eq!(summary.known_nanousd, 2);
        assert!(db
            .conn()
            .execute(
                "UPDATE usage_observations SET output_tokens=0 WHERE observation_id=?1",
                [&observation],
            )
            .is_err());
        assert_eq!(
            repo.project_id("/machine/git/common").unwrap(),
            repo.project_id("/machine/git/common").unwrap()
        );
    }
}
