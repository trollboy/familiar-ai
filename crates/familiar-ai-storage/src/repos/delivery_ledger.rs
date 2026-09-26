use familiar_ai_core::metrics::north_star::{MetricScope, WindowMetrics};
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

use super::driver::DriverRepository;
use super::session_rollup::autonomy_for_window;

/// Identifies the one physical ledger being queried. A SQLite file is never
/// silently promoted to project-wide evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLedgerScope {
    pub host_id: String,
    pub repository_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandingPath<'a> {
    DriveMergeQueue {
        session_id: &'a str,
        sequence: i64,
    },
    Resume {
        repository_key: &'a str,
        prd_id: &'a str,
    },
}

pub struct DeliveryLedgerRepository<'a> {
    conn: &'a Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryClaimFacts {
    pub accepted_prds: u64,
    pub unattended_prds: u64,
    pub measured_cost_executions: u64,
    pub total_cost_executions: u64,
}

impl<'a> DeliveryLedgerRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn window_metrics(
        &self,
        scope: &HostLedgerScope,
        start: &str,
        end: &str,
    ) -> familiar_ai_core::Result<WindowMetrics> {
        if scope.host_id.trim().is_empty() || scope.repository_key.trim().is_empty() {
            return Err(FamiliarError::Database(
                "delivery metric requires explicit host and repository scope".into(),
            ));
        }
        let (accepted, known_cost, unknown_cost): (u64,u64,u64) = self.conn.query_row(
            "SELECT count(CASE WHEN a.integrated_at IS NOT NULL THEN 1 END),coalesce(sum(a.known_cost_microusd),0),count(CASE WHEN a.known_cost_microusd IS NULL THEN 1 END) FROM driver_attempts a JOIN driver_sessions s ON s.session_id=a.session_id WHERE s.repository_key=?1 AND a.started_at>=?2 AND a.started_at<?3",
            params![scope.repository_key,start,end], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).map_err(db)?;
        let autonomy = autonomy_for_window(self.conn, &scope.repository_key, start, end)?;
        let touches = autonomy
            .by_intervention_command
            .values()
            .copied()
            .sum::<usize>() as u64;
        Ok(WindowMetrics::new(
            MetricScope {
                repository_key: scope.repository_key.clone(),
                hosts: vec![scope.host_id.clone()],
                window_start: start.into(),
                window_end: end.into(),
            },
            accepted,
            known_cost,
            unknown_cost,
            touches,
        ))
    }

    pub fn claim_facts(
        &self,
        repository_key: &str,
        start: &str,
        end: &str,
    ) -> familiar_ai_core::Result<DeliveryClaimFacts> {
        let (accepted,total,measured):(u64,u64,u64)=self.conn.query_row(
            "SELECT count(CASE WHEN a.integrated_at IS NOT NULL THEN 1 END),count(*),count(a.known_cost_microusd) FROM driver_attempts a JOIN driver_sessions s ON s.session_id=a.session_id WHERE s.repository_key=?1 AND a.started_at>=?2 AND a.started_at<?3",
            params![repository_key,start,end], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).map_err(db)?;
        let autonomy = autonomy_for_window(self.conn, repository_key, start, end)?;
        Ok(DeliveryClaimFacts {
            accepted_prds: accepted,
            unattended_prds: autonomy.unattended_completions as u64,
            measured_cost_executions: measured,
            total_cost_executions: total,
        })
    }

    /// One integration authority for both landing paths. The drive path
    /// names its claimed attempt; resume resolves the latest matching
    /// unintegrated attempt using the same repository-scoped rule as today.
    pub fn mark_integrated(
        &self,
        path: LandingPath<'_>,
        revision: &str,
    ) -> familiar_ai_core::Result<Option<(String, i64)>> {
        match path {
            LandingPath::Resume {
                repository_key,
                prd_id,
            } => DriverRepository::new(self.conn).mark_latest_attempt_integrated(
                repository_key,
                prd_id,
                revision,
            ),
            LandingPath::DriveMergeQueue {
                session_id,
                sequence,
            } => {
                let changed = self.conn.execute(
                    "UPDATE driver_attempts SET candidate_revision=?1,integrated_at=?2,last_durable_phase='integrated' WHERE session_id=?3 AND sequence=?4 AND integrated_at IS NULL",
                    params![revision, chrono::Utc::now().to_rfc3339(), session_id, sequence],
                ).map_err(db)?;
                Ok((changed == 1).then(|| (session_id.to_owned(), sequence)))
            }
        }
    }

    pub fn integration(
        &self,
        session: &str,
        sequence: i64,
    ) -> familiar_ai_core::Result<Option<(String, String)>> {
        self.conn.query_row("SELECT candidate_revision,integrated_at FROM driver_attempts WHERE session_id=?1 AND sequence=?2 AND integrated_at IS NOT NULL", params![session,sequence], |row| Ok((row.get(0)?,row.get(1)?))).optional().map_err(db)
    }
}

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DriverRepository;

    fn attempts() -> crate::Database {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let repo = DriverRepository::new(db.conn());
        for (session, repository, prd, cost) in [
            ("drive", "/familiar/.git", "PRD-1", Some(100)),
            ("resume", "/familiar/.git", "PRD-2", None),
            ("other", "/spectra/.git", "PRD-9", Some(9999)),
        ] {
            repo.open_session(session, repository, "{}").unwrap();
            let sequence = repo
                .record_attempt_started(session, prd, &format!("docs/prds/{prd}.md"), None)
                .unwrap();
            repo.record_attempt_finished(session, sequence, "completed", None, cost, None)
                .unwrap();
        }
        db
    }

    #[test]
    fn metrics_are_host_and_repository_scoped() {
        let db = attempts();
        let ledger = DeliveryLedgerRepository::new(db.conn());
        ledger
            .mark_integrated(
                LandingPath::DriveMergeQueue {
                    session_id: "drive",
                    sequence: 1,
                },
                "drive-sha",
            )
            .unwrap();
        let metrics = ledger
            .window_metrics(
                &HostLedgerScope {
                    host_id: "linux".into(),
                    repository_key: "/familiar/.git".into(),
                },
                "0000",
                "9999",
            )
            .unwrap();
        assert_eq!(metrics.scope.hosts, ["linux"]);
        assert_eq!(metrics.scope.repository_key, "/familiar/.git");
        assert_eq!(metrics.accepted_prds, 1);
        assert_eq!(metrics.known_cost_microusd, 100);
        assert_eq!(metrics.unknown_cost_attempts, 1);
    }

    #[test]
    fn drive_and_resume_landing_both_stamp_integrated_at() {
        let db = attempts();
        let ledger = DeliveryLedgerRepository::new(db.conn());
        assert_eq!(
            ledger
                .mark_integrated(
                    LandingPath::DriveMergeQueue {
                        session_id: "drive",
                        sequence: 1
                    },
                    "drive-sha"
                )
                .unwrap(),
            Some(("drive".into(), 1))
        );
        assert_eq!(
            ledger
                .mark_integrated(
                    LandingPath::Resume {
                        repository_key: "/familiar/.git",
                        prd_id: "PRD-2"
                    },
                    "resume-sha"
                )
                .unwrap(),
            Some(("resume".into(), 1))
        );
        assert!(ledger.integration("drive", 1).unwrap().is_some());
        assert!(ledger.integration("resume", 1).unwrap().is_some());
        assert!(ledger.integration("other", 1).unwrap().is_none());
    }
}
