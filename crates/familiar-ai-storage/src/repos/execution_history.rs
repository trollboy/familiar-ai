use std::collections::BTreeMap;

use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct ExecutionStart {
    pub execution_id: String,
    pub started_at: String,
    pub repository: String,
    pub worktree: String,
    pub git_commit: Option<String>,
    pub prd_path: String,
    pub unavailable_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionFinalization {
    pub ended_at: String,
    pub duration_ms: u64,
    pub agent_version: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost_microusd: Option<u64>,
    pub input_rate: Option<u64>,
    pub cached_input_rate: Option<u64>,
    pub output_rate: Option<u64>,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub unavailable_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionRecord {
    pub execution_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub agent: String,
    pub agent_version: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost_microusd: Option<u64>,
    pub input_rate: Option<u64>,
    pub cached_input_rate: Option<u64>,
    pub output_rate: Option<u64>,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub repository: String,
    pub worktree: String,
    pub git_commit: Option<String>,
    pub prd_path: String,
    pub unavailable_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageSummary {
    pub execution_count: u64,
    pub complete_usage: u64,
    pub unknown_usage: u64,
    pub known_input_tokens: u64,
    pub known_output_tokens: u64,
    pub known_cached_tokens: u64,
    pub known_total_tokens: u64,
    pub known_cost_executions: u64,
    pub unknown_cost_executions: u64,
    pub known_cost_microusd: u64,
    pub cache_measured_executions: u64,
    pub cache_unmeasured_executions: u64,
    pub cache_measured_input_tokens: u64,
    pub known_cache_savings_microusd: u64,
    pub cache_savings_priced_executions: u64,
    pub cache_savings_unpriced_executions: u64,
}

pub struct ExecutionHistoryRepository<'a> {
    conn: &'a Connection,
}

impl<'a> ExecutionHistoryRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert_running(&self, value: &ExecutionStart) -> familiar_ai_core::Result<()> {
        let unavailable = json(&value.unavailable_fields)?;
        self.conn.execute(
            "INSERT INTO execution_history (execution_id,started_at,agent,outcome,repository,worktree,git_commit,prd_path,unavailable_fields) VALUES (?1,?2,'codex','running',?3,?4,?5,?6,?7)",
            params![value.execution_id, value.started_at, value.repository, value.worktree, value.git_commit, value.prd_path, unavailable],
        ).map_err(db)?;
        Ok(())
    }

    pub fn finalize(
        &self,
        id: &str,
        value: &ExecutionFinalization,
    ) -> familiar_ai_core::Result<()> {
        let unavailable = json(&value.unavailable_fields)?;
        let changed = self.conn.execute(
            "UPDATE execution_history SET ended_at=?2,duration_ms=?3,agent_version=?4,model=?5,input_tokens=?6,output_tokens=?7,cached_tokens=?8,total_tokens=?9,estimated_cost_microusd=?10,input_rate_microusd_per_million=?11,cached_input_rate_microusd_per_million=?12,output_rate_microusd_per_million=?13,outcome=?14,exit_code=?15,signal=?16,unavailable_fields=?17 WHERE execution_id=?1 AND outcome='running'",
            params![id, value.ended_at, value.duration_ms, value.agent_version, value.model, value.input_tokens, value.output_tokens, value.cached_tokens, value.total_tokens, value.estimated_cost_microusd, value.input_rate, value.cached_input_rate, value.output_rate, value.outcome, value.exit_code, value.signal, unavailable],
        ).map_err(db)?;
        if changed != 1 {
            return Err(FamiliarError::Database(format!(
                "execution {id} was not running"
            )));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> familiar_ai_core::Result<Option<ExecutionRecord>> {
        self.conn.query_row("SELECT execution_id,started_at,ended_at,duration_ms,agent,agent_version,model,input_tokens,output_tokens,cached_tokens,total_tokens,estimated_cost_microusd,input_rate_microusd_per_million,cached_input_rate_microusd_per_million,output_rate_microusd_per_million,outcome,exit_code,signal,repository,worktree,git_commit,prd_path,unavailable_fields FROM execution_history WHERE execution_id=?1", [id], row).optional().map_err(db)
    }

    pub fn recent(&self, limit: u8) -> familiar_ai_core::Result<Vec<ExecutionRecord>> {
        let mut stmt = self.conn.prepare("SELECT execution_id,started_at,ended_at,duration_ms,agent,agent_version,model,input_tokens,output_tokens,cached_tokens,total_tokens,estimated_cost_microusd,input_rate_microusd_per_million,cached_input_rate_microusd_per_million,output_rate_microusd_per_million,outcome,exit_code,signal,repository,worktree,git_commit,prd_path,unavailable_fields FROM execution_history ORDER BY started_at DESC, execution_id DESC LIMIT ?1").map_err(db)?;
        let records = stmt
            .query_map([limit], row)
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(records)
    }

    pub fn usage(&self) -> familiar_ai_core::Result<UsageSummary> {
        let mut out = UsageSummary::default();
        let mut stmt = self.conn.prepare("SELECT input_tokens,output_tokens,cached_tokens,total_tokens,estimated_cost_microusd,input_rate_microusd_per_million,cached_input_rate_microusd_per_million FROM execution_history").map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<u64>>(0)?,
                    r.get::<_, Option<u64>>(1)?,
                    r.get::<_, Option<u64>>(2)?,
                    r.get::<_, Option<u64>>(3)?,
                    r.get::<_, Option<u64>>(4)?,
                    r.get::<_, Option<u64>>(5)?,
                    r.get::<_, Option<u64>>(6)?,
                ))
            })
            .map_err(db)?;
        for item in rows {
            let (input, output, cached, total, cost, input_rate, cached_rate) = item.map_err(db)?;
            out.execution_count = checked(out.execution_count, 1)?;
            if input.is_some() && output.is_some() && cached.is_some() {
                out.complete_usage += 1;
            } else {
                out.unknown_usage += 1;
            }
            out.known_input_tokens = checked(out.known_input_tokens, input.unwrap_or(0))?;
            out.known_output_tokens = checked(out.known_output_tokens, output.unwrap_or(0))?;
            out.known_cached_tokens = checked(out.known_cached_tokens, cached.unwrap_or(0))?;
            out.known_total_tokens = checked(out.known_total_tokens, total.unwrap_or(0))?;
            match (input, cached) {
                (Some(input), Some(cached)) if cached <= input => {
                    out.cache_measured_executions += 1;
                    out.cache_measured_input_tokens =
                        checked(out.cache_measured_input_tokens, input)?;
                    match (input_rate, cached_rate) {
                        (Some(uncached), Some(cached_price)) if cached_price <= uncached => {
                            out.cache_savings_priced_executions += 1;
                            let numerator =
                                cached.checked_mul(uncached - cached_price).ok_or_else(|| {
                                    FamiliarError::Database(
                                        "cache savings arithmetic overflow".into(),
                                    )
                                })?;
                            out.known_cache_savings_microusd = checked(
                                out.known_cache_savings_microusd,
                                numerator.checked_add(500_000).ok_or_else(|| {
                                    FamiliarError::Database(
                                        "cache savings arithmetic overflow".into(),
                                    )
                                })? / 1_000_000,
                            )?;
                        }
                        _ => out.cache_savings_unpriced_executions += 1,
                    }
                }
                _ => out.cache_unmeasured_executions += 1,
            }
            if let Some(value) = cost {
                out.known_cost_executions += 1;
                out.known_cost_microusd = checked(out.known_cost_microusd, value)?;
            } else {
                out.unknown_cost_executions += 1;
            }
        }
        Ok(out)
    }
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionRecord> {
    let unavailable: String = r.get(22)?;
    Ok(ExecutionRecord {
        execution_id: r.get(0)?,
        started_at: r.get(1)?,
        ended_at: r.get(2)?,
        duration_ms: r.get(3)?,
        agent: r.get(4)?,
        agent_version: r.get(5)?,
        model: r.get(6)?,
        input_tokens: r.get(7)?,
        output_tokens: r.get(8)?,
        cached_tokens: r.get(9)?,
        total_tokens: r.get(10)?,
        estimated_cost_microusd: r.get(11)?,
        input_rate: r.get(12)?,
        cached_input_rate: r.get(13)?,
        output_rate: r.get(14)?,
        outcome: r.get(15)?,
        exit_code: r.get(16)?,
        signal: r.get(17)?,
        repository: r.get(18)?,
        worktree: r.get(19)?,
        git_commit: r.get(20)?,
        prd_path: r.get(21)?,
        unavailable_fields: serde_json::from_str(&unavailable).unwrap_or_default(),
    })
}
fn json(v: &BTreeMap<String, String>) -> familiar_ai_core::Result<String> {
    serde_json::to_string(v).map_err(|e| FamiliarError::Database(e.to_string()))
}
fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}
fn checked(a: u64, b: u64) -> familiar_ai_core::Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| FamiliarError::Database("usage aggregate arithmetic overflow".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> (crate::Database, ExecutionStart) {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let start = ExecutionStart {
            execution_id: "execution-1".into(),
            started_at: "2026-08-03T12:00:00Z".into(),
            repository: "/repo/.git".into(),
            worktree: "/repo".into(),
            git_commit: None,
            prd_path: "docs/prds/PRD-004.md".into(),
            unavailable_fields: BTreeMap::from([
                ("git_commit".into(), "git_unavailable".into()),
                ("cached_tokens".into(), "runner_interrupted".into()),
            ]),
        };
        (db, start)
    }

    #[test]
    fn running_history_preserves_nulls_and_reasons() {
        let (db, start) = repository();
        let repo = ExecutionHistoryRepository::new(db.conn());
        repo.insert_running(&start).unwrap();
        let record = repo.get(&start.execution_id).unwrap().unwrap();
        assert_eq!(record.outcome, "running");
        assert_eq!(record.cached_tokens, None);
        assert_eq!(
            record
                .unavailable_fields
                .get("cached_tokens")
                .map(String::as_str),
            Some("runner_interrupted")
        );
    }

    #[test]
    fn finalization_is_guarded_and_usage_excludes_unknowns() {
        let (db, start) = repository();
        let repo = ExecutionHistoryRepository::new(db.conn());
        repo.insert_running(&start).unwrap();
        let finalization = ExecutionFinalization {
            ended_at: "2026-08-03T12:00:01Z".into(),
            duration_ms: 10,
            input_tokens: Some(10),
            output_tokens: Some(4),
            cached_tokens: None,
            total_tokens: Some(14),
            outcome: "succeeded".into(),
            exit_code: Some(0),
            unavailable_fields: BTreeMap::from([
                ("cached_tokens".into(), "usage_not_reported".into()),
                ("estimated_cost_microusd".into(), "usage_incomplete".into()),
            ]),
            ..Default::default()
        };
        repo.finalize(&start.execution_id, &finalization).unwrap();
        assert!(repo.finalize(&start.execution_id, &finalization).is_err());
        assert_eq!(
            repo.usage().unwrap(),
            UsageSummary {
                execution_count: 1,
                complete_usage: 0,
                unknown_usage: 1,
                known_input_tokens: 10,
                known_output_tokens: 4,
                known_cached_tokens: 0,
                known_total_tokens: 14,
                known_cost_executions: 0,
                unknown_cost_executions: 1,
                known_cost_microusd: 0,
                cache_unmeasured_executions: 1,
                cache_savings_unpriced_executions: 0,
                ..UsageSummary::default()
            }
        );
    }

    #[test]
    fn recent_order_has_stable_tie_breaker() {
        let (db, mut start) = repository();
        let repo = ExecutionHistoryRepository::new(db.conn());
        start.execution_id = "a".into();
        repo.insert_running(&start).unwrap();
        start.execution_id = "b".into();
        repo.insert_running(&start).unwrap();
        let rows = repo.recent(1).unwrap();
        assert_eq!(rows[0].execution_id, "b");
    }
}
