use chrono::Utc;
use familiar_ai_core::{
    probation::{score, EmpiricalMetrics, EmpiricalScore, ProbationPolicy, WorkerStanding},
    FamiliarError,
};
use rusqlite::{params, Connection, OptionalExtension};

pub struct ProbationRepository<'a> {
    conn: &'a Connection,
}

pub struct ProbationObservation<'a> {
    pub observation_id: &'a str,
    pub spec_identity: &'a str,
    pub empirical_version: &'a str,
    pub execution_id: Option<&'a str>,
    pub accepted: bool,
    pub verification_passed: bool,
    pub independent_review_passed: Option<bool>,
    pub remediation_required: bool,
    pub failed: bool,
    pub latency_ms: u64,
    pub evidence_json: &'a str,
}

impl<'a> ProbationRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn record_policy(&self, policy: &ProbationPolicy) -> familiar_ai_core::Result<()> {
        let json =
            serde_json::to_string(policy).map_err(|e| FamiliarError::Database(e.to_string()))?;
        self.conn.execute("INSERT OR IGNORE INTO probation_policies(policy_id,policy_version,policy_json,created_at) VALUES(?1,?2,?3,?4)", params![policy.policy_id,policy.version,json,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    pub fn append_observation(
        &self,
        value: &ProbationObservation<'_>,
    ) -> familiar_ai_core::Result<()> {
        self.conn.execute("INSERT INTO worker_probation_observations(observation_id,spec_identity,empirical_version,execution_id,accepted,verification_passed,independent_review_passed,remediation_required,failed,latency_ms,evidence_json,observed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)", params![value.observation_id,value.spec_identity,value.empirical_version,value.execution_id,value.accepted,value.verification_passed,value.independent_review_passed,value.remediation_required,value.failed,value.latency_ms,value.evidence_json,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    /// Rebuilds a score exclusively from immutable observations. Dollar and
    /// credit facts are deliberately separated; mixed units become unknown.
    pub fn snapshot(
        &self,
        score_id: &str,
        spec: &str,
        version: &str,
        policy: &ProbationPolicy,
    ) -> familiar_ai_core::Result<EmpiricalScore> {
        self.record_policy(policy)?;
        let (completed,accepted,reviews,review_attempts,remediated,failed,latency): (u64,u64,u64,u64,u64,u64,u64) = self.conn.query_row("SELECT count(*),coalesce(sum(accepted),0),coalesce(sum(independent_review_passed=1),0),coalesce(sum(independent_review_passed IS NOT NULL),0),coalesce(sum(remediation_required),0),coalesce(sum(failed OR verification_passed=0 OR independent_review_passed=0),0),coalesce(sum(latency_ms),0) FROM worker_probation_observations WHERE spec_identity=?1 AND empirical_version=?2", params![spec,version], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).map_err(db)?;
        let (cache_read, cache_eligible): (u64,u64) = self.conn.query_row("SELECT coalesce(sum(cache_read_tokens),0),coalesce(sum(coalesce(uncached_input_tokens,0)+coalesce(cache_read_tokens,0)),0) FROM usage_observations WHERE spec_identity=?1 AND empirical_version=?2", params![spec,version], |r| Ok((r.get(0)?,r.get(1)?))).map_err(db)?;
        let mut costs = self.conn.prepare("SELECT c.unit,sum(c.amount),group_concat(DISTINCT c.provenance) FROM cost_estimates c JOIN usage_observations u ON u.observation_id=c.observation_id WHERE u.spec_identity=?1 AND u.empirical_version=?2 AND c.amount IS NOT NULL AND c.billing_mode!='subscription-declaration' GROUP BY c.unit ORDER BY c.unit").map_err(db)?;
        let mut cost_rows: Vec<(String, u64, String)> = costs
            .query_map(params![spec, version], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(db)?
            .collect::<Result<_, _>>()
            .map_err(db)?;
        let declared_subscription_price: Option<u64> = self.conn.query_row("SELECT price_nanousd FROM subscription_declarations WHERE worker_identity=?1 AND available=1 AND price_nanousd>0 ORDER BY declared_at DESC,declaration_id DESC LIMIT 1",[spec],|r|r.get(0)).optional().map_err(db)?.flatten();
        if let Some(amount) = declared_subscription_price {
            cost_rows.push((
                "nanoUSD".into(),
                amount,
                "operator-declared-subscription-terms".into(),
            ));
        }
        let known_cost = match cost_rows.as_slice() {
            [(unit, amount, authority)] => Some((unit.clone(), *amount, authority.clone())),
            _ => None,
        };
        let metrics = EmpiricalMetrics {
            completed_prds: completed,
            accepted_prds: accepted,
            review_passes: reviews,
            review_attempts,
            remediated_prds: remediated,
            failed_prds: failed,
            latency_ms: latency,
            cache_read_tokens: cache_read,
            cache_eligible_tokens: cache_eligible,
            cost_amount: known_cost.as_ref().map(|v| v.1),
            cost_unit: known_cost.as_ref().map(|v| v.0.clone()),
            cost_authority: known_cost.as_ref().map(|v| v.2.clone()),
        };
        let trusted = !spec.contains("unknown") && !version.contains("unknown") && self.conn.query_row("SELECT EXISTS(SELECT 1 FROM worker_specs s JOIN worker_spec_versions v ON v.spec_identity=s.spec_identity WHERE s.spec_identity=?1 AND v.empirical_version=?2 AND s.model_state='known')",params![spec,version],|r|r.get::<_,bool>(0)).map_err(db)?;
        let result = score(policy, &metrics, trusted);
        let observation_ids: Vec<String> = self.list_strings("SELECT observation_id FROM worker_probation_observations WHERE spec_identity=?1 AND empirical_version=?2 ORDER BY observation_id",spec,version)?;
        let cost_ids: Vec<String> = self.list_strings("SELECT c.estimate_id FROM cost_estimates c JOIN usage_observations u ON u.observation_id=c.observation_id WHERE u.spec_identity=?1 AND u.empirical_version=?2 ORDER BY c.estimate_id",spec,version)?;
        self.conn.execute("INSERT INTO worker_score_snapshots(score_id,spec_identity,empirical_version,policy_id,policy_version,metrics_json,score_json,observation_ids_json,cost_observation_ids_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![score_id,spec,version,policy.policy_id,policy.version,serde_json::to_string(&metrics).unwrap(),serde_json::to_string(&result).unwrap(),serde_json::to_string(&observation_ids).unwrap(),serde_json::to_string(&cost_ids).unwrap(),Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(result)
    }

    /// Applies the closed policy to a freshly persisted snapshot. The event
    /// references that exact score, so promotion and demotion replay exactly.
    pub fn apply_policy(
        &self,
        event_id: &str,
        score_id: &str,
        spec: &str,
        version: &str,
        policy: &ProbationPolicy,
    ) -> familiar_ai_core::Result<EmpiricalScore> {
        let result = self.snapshot(score_id, spec, version, policy)?;
        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT standing FROM worker_standing_events WHERE spec_identity=?1 AND empirical_version=?2 ORDER BY occurred_at DESC,event_id DESC LIMIT 1",
                params![spec, version],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        // Quarantine and retirement are operator controls. Policy evaluation
        // may continue to persist reproducible scores, but only another
        // explicit standing event may replace either control.
        if matches!(current.as_deref(), Some("quarantined" | "retired")) {
            return Ok(result);
        }
        let standing = if result.promotion_eligible {
            WorkerStanding::Promoted
        } else {
            WorkerStanding::Probation
        };
        self.set_standing(
            event_id,
            spec,
            version,
            standing,
            "policy",
            Some(score_id),
            "deterministic-policy",
            &format!(
                "{}@{} promotion_eligible={}",
                policy.policy_id, policy.version, result.promotion_eligible
            ),
        )?;
        Ok(result)
    }

    fn list_strings(
        &self,
        sql: &str,
        spec: &str,
        version: &str,
    ) -> familiar_ai_core::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(sql).map_err(db)?;
        let rows = stmt
            .query_map(params![spec, version], |r| r.get(0))
            .map_err(db)?
            .collect::<Result<_, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_standing(
        &self,
        event_id: &str,
        spec: &str,
        version: &str,
        standing: WorkerStanding,
        source: &str,
        score_id: Option<&str>,
        actor: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<()> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(FamiliarError::Database(
                "standing actor and reason are required".into(),
            ));
        }
        self.conn.execute("INSERT INTO worker_standing_events(event_id,spec_identity,empirical_version,standing,source,score_id,actor,reason,occurred_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![event_id,spec,version,standing.as_str(),source,score_id,actor,reason,Utc::now().to_rfc3339()]).map_err(db)?;
        Ok(())
    }

    /// Applies a repository routing pin without overriding an explicit
    /// operator quarantine or retirement.
    pub fn set_routing_pin(
        &self,
        event_id: &str,
        spec: &str,
        version: &str,
        actor: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<bool> {
        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT standing FROM worker_standing_events WHERE spec_identity=?1 AND empirical_version=?2 ORDER BY occurred_at DESC,event_id DESC LIMIT 1",
                params![spec, version],
                |row| row.get(0),
            )
            .optional()
            .map_err(db)?;
        if matches!(current.as_deref(), Some("quarantined" | "retired")) {
            return Ok(false);
        }
        self.set_standing(
            event_id,
            spec,
            version,
            WorkerStanding::Promoted,
            "operator-pin",
            None,
            actor,
            reason,
        )?;
        Ok(true)
    }

    pub fn authorize(
        &self,
        spec: &str,
        version: &str,
        policy: &ProbationPolicy,
        expected_files: u64,
        has_risk: bool,
        independent_review: bool,
    ) -> familiar_ai_core::Result<()> {
        let standing: Option<String>=self.conn.query_row("SELECT standing FROM worker_standing_events WHERE spec_identity=?1 AND empirical_version=?2 ORDER BY occurred_at DESC,event_id DESC LIMIT 1",params![spec,version],|r|r.get(0)).optional().map_err(db)?;
        match standing.as_deref() {
            Some("quarantined") | Some("retired") => Err(FamiliarError::Database(format!(
                "worker {spec} is {}",
                standing.unwrap()
            ))),
            Some("promoted") => Ok(()),
            _ if !has_risk
                && expected_files <= policy.probation_max_expected_files
                && (!policy.require_independent_review || independent_review) =>
            {
                Ok(())
            }
            _ => Err(FamiliarError::Database(format!(
                "worker {spec} is limited to a probation warrant until policy {}@{} promotes it",
                policy.policy_id, policy.version
            ))),
        }
    }
}

fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ProbationPolicy {
        ProbationPolicy {
            policy_id: "test".into(),
            version: "1".into(),
            minimum_accepted_prds: 1,
            minimum_review_pass_basis_points: 10_000,
            maximum_remediation_basis_points: 0,
            maximum_failure_basis_points: 0,
            probation_max_expected_files: 2,
            require_independent_review: true,
        }
    }

    #[test]
    fn failures_remain_evidence_and_reproduce_the_score() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        crate::WorkerSpecRepository::new(db.conn())
            .record_spec(
                "spec", "version", "worker", "provider", "runtime", "model", None, None, "profile",
                "{}",
            )
            .unwrap();
        let repo = ProbationRepository::new(db.conn());
        repo.append_observation(&ProbationObservation {
            observation_id: "failed",
            spec_identity: "spec",
            empirical_version: "version",
            execution_id: None,
            accepted: false,
            verification_passed: false,
            independent_review_passed: Some(false),
            remediation_required: true,
            failed: true,
            latency_ms: 12,
            evidence_json: "{\"result\":\"failed\"}",
        })
        .unwrap();
        let first = repo
            .snapshot("score-1", "spec", "version", &policy())
            .unwrap();
        let second = repo
            .snapshot("score-2", "spec", "version", &policy())
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.failure_basis_points, Some(10_000));
        let evidence:String=db.conn().query_row("SELECT evidence_json FROM worker_probation_observations WHERE observation_id='failed'",[],|r|r.get(0)).unwrap();
        assert!(evidence.contains("failed"));
    }

    #[test]
    fn probation_is_bounded_and_operator_controls_override_it() {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        crate::WorkerSpecRepository::new(db.conn())
            .record_spec(
                "spec", "version", "worker", "provider", "runtime", "model", None, None, "profile",
                "{}",
            )
            .unwrap();
        let repo = ProbationRepository::new(db.conn());
        let p = policy();
        assert!(repo
            .authorize("spec", "version", &p, 2, false, true)
            .is_ok());
        assert!(repo
            .authorize("spec", "version", &p, 3, false, true)
            .is_err());
        repo.set_standing(
            "pin",
            "spec",
            "version",
            WorkerStanding::Promoted,
            "operator-pin",
            None,
            "operator",
            "pinned",
        )
        .unwrap();
        assert!(repo
            .authorize("spec", "version", &p, 99, true, false)
            .is_ok());
        repo.set_standing(
            "quarantine",
            "spec",
            "version",
            WorkerStanding::Quarantined,
            "operator-quarantine",
            None,
            "operator",
            "unsafe",
        )
        .unwrap();
        assert!(repo
            .authorize("spec", "version", &p, 1, false, true)
            .is_err());
    }

    #[test]
    fn policy_cannot_reactivate_quarantined_or_retired_workers() {
        for (standing, event, source) in [
            (
                WorkerStanding::Quarantined,
                "quarantine",
                "operator-quarantine",
            ),
            (WorkerStanding::Retired, "retire", "operator-retire"),
        ] {
            let db = crate::Database::open_in_memory().unwrap();
            db.run_migrations().unwrap();
            crate::WorkerSpecRepository::new(db.conn())
                .record_spec(
                    "spec", "version", "worker", "provider", "runtime", "model", None, None,
                    "profile", "{}",
                )
                .unwrap();
            let repo = ProbationRepository::new(db.conn());
            repo.append_observation(&ProbationObservation {
                observation_id: "accepted",
                spec_identity: "spec",
                empirical_version: "version",
                execution_id: None,
                accepted: true,
                verification_passed: true,
                independent_review_passed: Some(true),
                remediation_required: false,
                failed: false,
                latency_ms: 1,
                evidence_json: "{}",
            })
            .unwrap();
            repo.set_standing(
                event,
                "spec",
                "version",
                standing,
                source,
                None,
                "operator",
                "explicit control",
            )
            .unwrap();

            let score = repo
                .apply_policy("policy-event", "score", "spec", "version", &policy())
                .unwrap();
            assert!(score.promotion_eligible);
            assert!(repo
                .authorize("spec", "version", &policy(), 1, false, true)
                .is_err());
            let policy_events: u64 = db
                .conn()
                .query_row(
                    "SELECT count(*) FROM worker_standing_events WHERE event_id='policy-event'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(policy_events, 0);
        }
    }

    #[test]
    fn routing_pin_cannot_reactivate_quarantined_or_retired_workers() {
        for (standing, source) in [
            (WorkerStanding::Quarantined, "operator-quarantine"),
            (WorkerStanding::Retired, "operator-retire"),
        ] {
            let db = crate::Database::open_in_memory().unwrap();
            db.run_migrations().unwrap();
            crate::WorkerSpecRepository::new(db.conn())
                .record_spec(
                    "spec", "version", "worker", "provider", "runtime", "model", None, None,
                    "profile", "{}",
                )
                .unwrap();
            let repo = ProbationRepository::new(db.conn());
            repo.set_standing(
                "control",
                "spec",
                "version",
                standing,
                source,
                None,
                "operator",
                "explicit control",
            )
            .unwrap();

            assert!(!repo
                .set_routing_pin(
                    "routing-pin",
                    "spec",
                    "version",
                    "repository-config",
                    "explicit worker routing pin",
                )
                .unwrap());
            assert!(repo
                .authorize("spec", "version", &policy(), 1, false, true)
                .is_err());
            let pin_events: u64 = db
                .conn()
                .query_row(
                    "SELECT count(*) FROM worker_standing_events WHERE event_id='routing-pin'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(pin_events, 0);
        }
    }
}
