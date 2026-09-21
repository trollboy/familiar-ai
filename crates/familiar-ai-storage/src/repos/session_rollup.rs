use familiar_ai_core::models::{NewSessionRollup, SessionRollup};
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

use super::driver::{DriverAttempt, DriverRepository};
use super::{json_to_vec, now_rfc3339, parse_dt, vec_to_json};
use crate::sql;
use crate::Database;

pub trait SessionRollupRepository {
    fn create_session_rollup(
        &self,
        rollup: &NewSessionRollup,
    ) -> familiar_ai_core::Result<SessionRollup>;
    fn get_session_rollup_by_id(&self, id: i64) -> familiar_ai_core::Result<Option<SessionRollup>>;
    fn list_session_rollups_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<SessionRollup>>;
    fn delete_session_rollup(&self, id: i64) -> familiar_ai_core::Result<()>;
}

pub(crate) fn row_to_session_rollup(row: &rusqlite::Row) -> rusqlite::Result<SessionRollup> {
    let related_files_json: String = row.get("related_files_json")?;
    let next_steps_json: String = row.get("next_steps_json")?;
    let created_at_str: String = row.get("created_at")?;
    let updated_at_str: String = row.get("updated_at")?;

    Ok(SessionRollup {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        summary: row.get("summary")?,
        related_files: json_to_vec(&related_files_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        next_steps: json_to_vec(&next_steps_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        created_at: parse_dt(&created_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        updated_at: parse_dt(&updated_at_str).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
    })
}

impl SessionRollupRepository for Database {
    fn create_session_rollup(
        &self,
        rollup: &NewSessionRollup,
    ) -> familiar_ai_core::Result<SessionRollup> {
        let now = now_rfc3339();
        let related_json = vec_to_json(&rollup.related_files)?;
        let next_steps_json = vec_to_json(&rollup.next_steps)?;

        self.conn()
            .execute(
                sql::INSERT_SESSION_ROLLUP,
                params![
                    rollup.project_id,
                    rollup.summary,
                    related_json,
                    next_steps_json,
                    now,
                ],
            )
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let id = self.conn().last_insert_rowid();
        self.get_session_rollup_by_id(id)?.ok_or_else(|| {
            FamiliarError::Database("failed to read back created session rollup".into())
        })
    }

    fn get_session_rollup_by_id(&self, id: i64) -> familiar_ai_core::Result<Option<SessionRollup>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_SESSION_ROLLUP_BY_ID)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        stmt.query_row(params![id], row_to_session_rollup)
            .optional()
            .map_err(|e| FamiliarError::Database(e.to_string()))
    }

    fn list_session_rollups_by_project(
        &self,
        project_id: i64,
        limit: usize,
    ) -> familiar_ai_core::Result<Vec<SessionRollup>> {
        let mut stmt = self
            .conn()
            .prepare(sql::SELECT_SESSION_ROLLUPS_BY_PROJECT)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![project_id, limit as i64], row_to_session_rollup)
            .map_err(|e| FamiliarError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| FamiliarError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    fn delete_session_rollup(&self, id: i64) -> familiar_ai_core::Result<()> {
        self.conn()
            .execute(sql::DELETE_SESSION_ROLLUP, params![id])
            .map_err(|e| FamiliarError::Database(e.to_string()))?;
        Ok(())
    }
}

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

// ---------------------------------------------------------------------
// PRD-085: the closed stall vocabulary, human-intervention detection, and
// the autonomy rollup computed over both. Familiar previously had no
// record of which completions were assisted and no closed name for what a
// stall was; this section makes both durable facts instead of narration.
// ---------------------------------------------------------------------

/// A stall class's remedy: either the one Familiar command that resolves
/// it, or an explicit admission that no command can. A class with no
/// remedy is still named — never silently dropped from the vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum StallRecovery {
    Command(String),
    Unrecoverable(&'static str),
}

/// One entry of the closed stall vocabulary. `recovery` is a function
/// rather than a fixed string because several classes fold the specific
/// PRD's id into their one command (e.g. `resume <prd_id>`).
struct StallClassDef {
    name: &'static str,
    recovery: fn(&str, &str) -> StallRecovery,
}

fn resume_command(prd_id: &str, _prd_path: &str) -> StallRecovery {
    StallRecovery::Command(format!("familiar-ai resume {prd_id}"))
}

fn scope_decisions_command(_prd_id: &str, _prd_path: &str) -> StallRecovery {
    StallRecovery::Command("familiar-ai scope-decisions".to_string())
}

fn operator_rebind_command(prd_id: &str, _prd_path: &str) -> StallRecovery {
    StallRecovery::Command(format!(
        "familiar-ai operator rebind {prd_id} --actor human:<identity> --reason \"<why>\""
    ))
}

fn environment_denied_unrecoverable(_prd_id: &str, _prd_path: &str) -> StallRecovery {
    StallRecovery::Unrecoverable(
        "environment denial requires a human to repair sandbox or filesystem permissions; \
         no Familiar command can grant an OS-level permission on its behalf",
    )
}

/// PRD-085: the closed, configured vocabulary of every way a driver attempt
/// pauses or terminates, drawn from the reasons the driver and run loop
/// actually record (PRD-077's circuit-breaker reasons, PRD-083's scope
/// pause, PRD-084's operator repairs) plus one catch-all for a reason this
/// table has not yet named. Adding a stall without a recovery entry here is
/// caught by [`assert_stall_taxonomy_complete`], not discovered by an
/// operator staring at an unnamed detail string.
const STALL_TAXONOMY: &[StallClassDef] = &[
    StallClassDef {
        name: "scope_broadened",
        recovery: scope_decisions_command,
    },
    StallClassDef {
        name: "scope_ambiguous",
        recovery: scope_decisions_command,
    },
    StallClassDef {
        name: "environment_denied",
        recovery: environment_denied_unrecoverable,
    },
    StallClassDef {
        name: "checkpoint_failed",
        recovery: operator_rebind_command,
    },
    StallClassDef {
        name: "verification_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "human_review_required",
        recovery: resume_command,
    },
    StallClassDef {
        name: "interrupted",
        recovery: resume_command,
    },
    StallClassDef {
        name: "unclassified_result",
        recovery: resume_command,
    },
    StallClassDef {
        name: "integration_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "malformed_output",
        recovery: resume_command,
    },
    StallClassDef {
        name: "implementation_incomplete",
        recovery: resume_command,
    },
    StallClassDef {
        name: "preflight_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "review_disabled",
        recovery: resume_command,
    },
    StallClassDef {
        name: "review_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "worktree_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "workspace_evidence_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "warrant_reservation_refused",
        recovery: resume_command,
    },
    StallClassDef {
        name: "implementation_token_usage_unknown",
        recovery: resume_command,
    },
    StallClassDef {
        name: "implementation_token_budget_exceeded",
        recovery: resume_command,
    },
    StallClassDef {
        name: "implementation_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "missing_authoritative_input_reference",
        recovery: resume_command,
    },
    StallClassDef {
        name: "unreadable_reference",
        recovery: resume_command,
    },
    StallClassDef {
        name: "context_compilation_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "history_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "accounting_failed",
        recovery: resume_command,
    },
    StallClassDef {
        name: "completion_conflict",
        recovery: resume_command,
    },
    StallClassDef {
        name: "budget_exceeded",
        recovery: resume_command,
    },
    StallClassDef {
        name: "budget_stopped",
        recovery: resume_command,
    },
    StallClassDef {
        name: "budget_refused",
        recovery: resume_command,
    },
    StallClassDef {
        name: "unclassified",
        recovery: resume_command,
    },
];

/// PRD-085: naming this invariant makes a taxonomy defect (a class with no
/// recovery command) a durable, citable failure rather than an operator
/// discovering there is nothing to run.
pub const INVARIANT_STALL_TAXONOMY_COMPLETE: &str = "stall-taxonomy-recovery-command-complete";

/// Every class in the closed vocabulary must resolve to a non-empty command
/// or an explicit, non-empty unrecoverable reason. `Err` names the
/// offending class so a broken addition to the table fails loudly and
/// specifically rather than silently advertising nothing.
pub fn assert_stall_taxonomy_complete() -> Result<(), String> {
    for class in STALL_TAXONOMY {
        match (class.recovery)("PRD-000", "docs/prds/PRD-000.md") {
            StallRecovery::Command(command) if command.trim().is_empty() => {
                return Err(format!(
                    "{INVARIANT_STALL_TAXONOMY_COMPLETE} violated: stall class '{}' has no recovery command",
                    class.name
                ));
            }
            StallRecovery::Unrecoverable(reason) if reason.trim().is_empty() => {
                return Err(format!(
                    "{INVARIANT_STALL_TAXONOMY_COMPLETE} violated: stall class '{}' is marked unrecoverable with no reason",
                    class.name
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Every class name the taxonomy recognizes, for a regression to enumerate
/// against — the same list [`assert_stall_taxonomy_complete`] checks.
pub fn stall_taxonomy_classes() -> Vec<&'static str> {
    STALL_TAXONOMY.iter().map(|class| class.name).collect()
}

/// Classifies one driver attempt's stop into a closed vocabulary class.
/// Total: an attempt that never recorded a reason and never finished is
/// `interrupted`; any reason string the table has not (yet) named is
/// `unclassified` rather than silently dropped.
pub fn classify_attempt_stall(
    retained_reason: Option<&str>,
    outcome: Option<&str>,
) -> &'static str {
    let key = match retained_reason {
        Some(reason) => reason,
        None if outcome.is_none() => "interrupted",
        None => "unclassified",
    };
    named_class(key)
        // A reason that carried free-form detail still names its class in the
        // token before the first colon ("review_failed: configuration failed:
        // enabled review requires ..."). Without this the detail costs the row
        // both its class and its recovery command.
        .or_else(|| named_class(key.split(':').next().unwrap_or(key).trim()))
        .unwrap_or("unclassified")
}

/// The taxonomy class named exactly by `key`, if the vocabulary has one.
fn named_class(key: &str) -> Option<&'static str> {
    STALL_TAXONOMY
        .iter()
        .find(|class| class.name == key)
        .map(|class| class.name)
}

/// The one executable recovery command (or unrecoverable reason) for a
/// classified stall.
pub fn stall_recovery(class_name: &str, prd_id: &str, prd_path: &str) -> StallRecovery {
    let class = STALL_TAXONOMY
        .iter()
        .find(|class| class.name == class_name)
        .unwrap_or_else(|| {
            STALL_TAXONOMY
                .iter()
                .find(|class| class.name == "unclassified")
                .expect("unclassified is always present in the taxonomy")
        });
    (class.recovery)(prd_id, prd_path)
}

/// A durable, actor-tagged write inside a PRD's lifetime, together with the
/// exact command that produced it (PRD-085). Sourced only from writes that
/// already carry the `human:` actor prefix PRD-083 and PRD-084 both
/// require — never inferred, never narrated.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HumanIntervention {
    pub actor: String,
    pub command: String,
    pub occurred_at: String,
}

fn scope_decision_interventions(
    conn: &Connection,
    repository_key: &str,
    prd_id: &str,
    start: &str,
    end: &str,
) -> familiar_ai_core::Result<Vec<HumanIntervention>> {
    let mut stmt = conn
        .prepare(
            "SELECT actor,decision,decided_at FROM scope_decisions \
             WHERE repository_key=?1 AND prd_id=?2 AND actor LIKE 'human:%' \
               AND decided_at IS NOT NULL AND decided_at>=?3 AND decided_at<=?4",
        )
        .map_err(db)?;
    let rows = stmt
        .query_map(params![repository_key, prd_id, start, end], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(db)?;
    let mut out = Vec::new();
    for row in rows {
        let (actor, decision, decided_at) = row.map_err(db)?;
        let flag = match decision.as_deref() {
            Some("approved") => "--approve",
            _ => "--reject",
        };
        out.push(HumanIntervention {
            command: format!("familiar-ai scope-decisions {flag} --actor {actor}"),
            actor,
            occurred_at: decided_at,
        });
    }
    Ok(out)
}

fn backlog_interventions(
    conn: &Connection,
    repository_key: &str,
    prd_path: &str,
    start: &str,
    end: &str,
) -> familiar_ai_core::Result<Vec<HumanIntervention>> {
    let mut stmt = conn
        .prepare(
            "SELECT actor,new_status,changed_at FROM backlog_status_events \
             WHERE repository_key=?1 AND prd_path=?2 AND actor LIKE 'human:%' \
               AND changed_at>=?3 AND changed_at<=?4",
        )
        .map_err(db)?;
    let rows = stmt
        .query_map(params![repository_key, prd_path, start, end], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(db)?;
    let mut out = Vec::new();
    for row in rows {
        let (actor, new_status, changed_at) = row.map_err(db)?;
        let verb = if new_status == "completed" {
            "complete"
        } else {
            "release"
        };
        out.push(HumanIntervention {
            command: format!("familiar-ai backlog {verb} {prd_path} --actor {actor}"),
            actor,
            occurred_at: changed_at,
        });
    }
    Ok(out)
}

fn checkpoint_interventions(
    conn: &Connection,
    repository_key: &str,
    prd_id: &str,
    start: &str,
    end: &str,
) -> familiar_ai_core::Result<Vec<HumanIntervention>> {
    let mut stmt = conn
        .prepare(
            "SELECT ev.event_type,ev.detail,ev.recorded_at,ev.checkpoint_id,ev.resulting_phase \
             FROM execution_checkpoint_events ev \
             JOIN execution_checkpoints c ON c.checkpoint_id=ev.checkpoint_id \
             WHERE c.repository_key=?1 AND c.prd_id=?2 \
               AND ev.recorded_at>=?3 AND ev.recorded_at<=?4",
        )
        .map_err(db)?;
    let rows = stmt
        .query_map(params![repository_key, prd_id, start, end], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(db)?;
    let mut out = Vec::new();
    for row in rows {
        let (event_type, detail, recorded_at, checkpoint_id, resulting_phase) = row.map_err(db)?;
        if event_type == "candidate_rebound" {
            let parsed: serde_json::Value = serde_json::from_str(&detail).unwrap_or_default();
            let Some(actor) = parsed.get("actor").and_then(|v| v.as_str()) else {
                continue;
            };
            if !actor.starts_with("human:") {
                continue;
            }
            let reason = parsed.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            out.push(HumanIntervention {
                command: format!(
                    "familiar-ai operator rebind {prd_id} --actor {actor} --reason \"{reason}\""
                ),
                actor: actor.to_string(),
                occurred_at: recorded_at,
            });
        } else if event_type == "phase_transition" && detail.starts_with("human:") {
            let mut parts = detail.splitn(2, ": ");
            let actor = parts.next().unwrap_or_default().to_string();
            let reason = parts.next().unwrap_or_default();
            out.push(HumanIntervention {
                command: format!(
                    "familiar-ai operator set-phase {checkpoint_id} {resulting_phase} --actor {actor} --reason \"{reason}\""
                ),
                actor,
                occurred_at: recorded_at,
            });
        }
    }
    Ok(out)
}

/// Every durable human-actor write touching this PRD within `[start,end]`,
/// across every source PRD-083/084 made auditable: scope decisions, backlog
/// release/complete, and operator rebind/set-phase.
pub fn human_interventions_for_prd(
    conn: &Connection,
    repository_key: &str,
    prd_id: &str,
    prd_path: &str,
    start: &str,
    end: &str,
) -> familiar_ai_core::Result<Vec<HumanIntervention>> {
    let mut out = scope_decision_interventions(conn, repository_key, prd_id, start, end)?;
    out.extend(backlog_interventions(
        conn,
        repository_key,
        prd_path,
        start,
        end,
    )?);
    out.extend(checkpoint_interventions(
        conn,
        repository_key,
        prd_id,
        start,
        end,
    )?);
    out.sort_by(|a, b| a.occurred_at.cmp(&b.occurred_at));
    Ok(out)
}

/// One PRD's disposition within a session: it finished with nobody's hand
/// on it, it finished only after a named intervention, or it stalled.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum PrdAutonomyOutcome {
    Unattended,
    Assisted {
        commands: Vec<String>,
    },
    Stalled {
        stall_class: String,
        recovery: StallRecovery,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PrdAutonomy {
    pub prd_id: String,
    pub prd_path: String,
    pub outcome: PrdAutonomyOutcome,
}

/// PRD-085: one session's autonomy, computed from durable driver_attempts,
/// scope_decisions, backlog_status_events, and execution_checkpoint_events
/// rows alone — never from narration.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SessionAutonomy {
    pub prds: Vec<PrdAutonomy>,
}

impl SessionAutonomy {
    pub fn unattended_count(&self) -> usize {
        self.prds
            .iter()
            .filter(|prd| matches!(prd.outcome, PrdAutonomyOutcome::Unattended))
            .count()
    }

    pub fn assisted(&self) -> Vec<&PrdAutonomy> {
        self.prds
            .iter()
            .filter(|prd| matches!(prd.outcome, PrdAutonomyOutcome::Assisted { .. }))
            .collect()
    }

    pub fn stalled(&self) -> Vec<&PrdAutonomy> {
        self.prds
            .iter()
            .filter(|prd| matches!(prd.outcome, PrdAutonomyOutcome::Stalled { .. }))
            .collect()
    }

    /// `None` when the session attempted no PRDs at all — a fraction of zero
    /// would misreport a session that built nothing as a perfect autonomy
    /// failure. A stall lowers this fraction exactly as an assisted
    /// completion does, so an all-stalled session reports the worst possible
    /// autonomy outcome rather than `None` (PRD-085 F2).
    pub fn unattended_fraction(&self) -> Option<f64> {
        if self.prds.is_empty() {
            return None;
        }
        Some(self.unattended_count() as f64 / self.prds.len() as f64)
    }

    /// The most frequent stall class, for naming the dominant cause of an
    /// autonomy failure rather than reporting only its magnitude.
    pub fn dominant_stall_class(&self) -> Option<&str> {
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for prd in self.stalled() {
            if let PrdAutonomyOutcome::Stalled { stall_class, .. } = &prd.outcome {
                *counts.entry(stall_class.as_str()).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(class, _)| class)
    }

    /// `true` when this session's unattended fraction is below `floor` — an
    /// autonomy failure the report states plainly rather than narrating a
    /// stalled session as a success (PRD-085 AC6).
    pub fn is_autonomy_failure(&self, floor: f64) -> bool {
        self.unattended_fraction()
            .is_some_and(|fraction| fraction < floor)
    }
}

/// Computes one session's autonomy from its recorded attempts, folded to one
/// disposition per PRD. A session that does not exist reports as having
/// produced no PRDs, rather than an error the caller must special-case.
///
/// `DriverRepository::attempts` orders rows by `sequence`, so within each
/// PRD's group the last entry is its most recent attempt. A PRD whose most
/// recent attempt completed counts once, as unattended or assisted, over the
/// interventions recorded across its whole lifetime — from its first
/// attempt's start to its completing attempt's end — so a human write during
/// an earlier retry still counts even though the final attempt succeeded
/// alone. A PRD counts as stalled only when no attempt of it completed.
pub fn session_autonomy(
    conn: &Connection,
    session_id: &str,
) -> familiar_ai_core::Result<SessionAutonomy> {
    let sessions = DriverRepository::new(conn);
    let Some(session) = sessions.get_session(session_id)? else {
        return Ok(SessionAutonomy::default());
    };
    let attempts = sessions.attempts(session_id)?;

    let mut order: Vec<String> = Vec::new();
    let mut by_prd: std::collections::HashMap<String, Vec<&DriverAttempt>> =
        std::collections::HashMap::new();
    for attempt in &attempts {
        by_prd
            .entry(attempt.prd_id.clone())
            .or_insert_with(|| {
                order.push(attempt.prd_id.clone());
                Vec::new()
            })
            .push(attempt);
    }

    let mut prds = Vec::new();
    for prd_id in order {
        let group = &by_prd[&prd_id];
        let last = *group.last().expect("prd group is never empty");
        let prd_path = last.prd_path.clone();

        if last.outcome.as_deref() == Some("completed") {
            let first_started = &group.first().expect("prd group is never empty").started_at;
            let end = last
                .ended_at
                .clone()
                .unwrap_or_else(|| last.started_at.clone());
            let interventions = human_interventions_for_prd(
                conn,
                &session.repository_key,
                &prd_id,
                &prd_path,
                first_started,
                &end,
            )?;
            let outcome = if interventions.is_empty() {
                PrdAutonomyOutcome::Unattended
            } else {
                PrdAutonomyOutcome::Assisted {
                    commands: interventions.into_iter().map(|i| i.command).collect(),
                }
            };
            prds.push(PrdAutonomy {
                prd_id,
                prd_path,
                outcome,
            });
        } else {
            let class =
                classify_attempt_stall(last.retained_reason.as_deref(), last.outcome.as_deref());
            let recovery = stall_recovery(class, &prd_id, &prd_path);
            prds.push(PrdAutonomy {
                prd_id,
                prd_path,
                outcome: PrdAutonomyOutcome::Stalled {
                    stall_class: class.to_string(),
                    recovery,
                },
            });
        }
    }
    Ok(SessionAutonomy { prds })
}

/// PRD-085 AC4: autonomy grouped by repository, stall class, and
/// intervention command over a bounded window of sessions, so the most
/// frequent human step is identifiable without reading transcripts.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct AutonomyWindowSummary {
    pub repository_key: String,
    pub unattended_completions: usize,
    pub assisted_completions: usize,
    pub stalled: usize,
    pub by_stall_class: std::collections::BTreeMap<String, usize>,
    pub by_intervention_command: std::collections::BTreeMap<String, usize>,
}

pub fn autonomy_for_window(
    conn: &Connection,
    repository_key: &str,
    start: &str,
    end: &str,
) -> familiar_ai_core::Result<AutonomyWindowSummary> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id FROM driver_sessions \
             WHERE repository_key=?1 AND started_at>=?2 AND started_at<=?3",
        )
        .map_err(db)?;
    let session_ids: Vec<String> = stmt
        .query_map(params![repository_key, start, end], |row| row.get(0))
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    let mut summary = AutonomyWindowSummary {
        repository_key: repository_key.to_string(),
        ..Default::default()
    };
    for session_id in session_ids {
        let autonomy = session_autonomy(conn, &session_id)?;
        for prd in &autonomy.prds {
            match &prd.outcome {
                PrdAutonomyOutcome::Unattended => summary.unattended_completions += 1,
                PrdAutonomyOutcome::Assisted { commands } => {
                    summary.assisted_completions += 1;
                    for command in commands {
                        *summary
                            .by_intervention_command
                            .entry(command.clone())
                            .or_default() += 1;
                    }
                }
                PrdAutonomyOutcome::Stalled { stall_class, .. } => {
                    summary.stalled += 1;
                    *summary
                        .by_stall_class
                        .entry(stall_class.clone())
                        .or_default() += 1;
                }
            }
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {

    /// Six real attempts recorded `review_failed: configuration failed: ...`.
    /// Exact-match classification dropped every one of them to `unclassified`,
    /// costing them both their class and the recovery command keyed to it.
    /// Detail in the reason string must not cost the row its classification.
    #[test]
    fn a_reason_carrying_detail_still_names_its_class() {
        assert_eq!(
            classify_attempt_stall(
                Some(
                    "review_failed: configuration failed: enabled review requires an \
                     explicit PRD Acceptance Criteria section"
                ),
                Some("retained"),
            ),
            "review_failed",
        );
        // The recovery command follows the class, which is the whole point.
        assert_eq!(
            stall_recovery("review_failed", "PRD-177a", "docs/prds/PRD-177a.md"),
            stall_recovery(
                classify_attempt_stall(Some("review_failed: anything at all"), Some("retained")),
                "PRD-177a",
                "docs/prds/PRD-177a.md",
            ),
        );
        // A bare token is unaffected, and a genuinely unknown class still
        // lands in `unclassified` rather than being invented.
        assert_eq!(
            classify_attempt_stall(Some("scope_broadened"), Some("retained")),
            "scope_broadened",
        );
        assert_eq!(
            classify_attempt_stall(Some("nonsense_class: with detail"), Some("retained")),
            "unclassified",
        );
    }

    use super::*;
    use crate::repos::checkpoint::{CheckpointRepository, ExecutionCheckpoint};
    use crate::repos::project::ProjectRepository;
    use familiar_ai_core::models::NewProject;

    fn test_db() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn create_test_project(db: &Database) -> i64 {
        db.create_project(&NewProject {
            name: "test".into(),
            repo_root: "/test/project".into(),
            ignored_paths: vec![],
            token_budget: None,
        })
        .unwrap()
        .id
    }

    #[test]
    fn create_and_get_by_id() {
        let db = test_db();
        let pid = create_test_project(&db);
        let rollup = db
            .create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: "Implemented auth token rotation".into(),
                related_files: vec!["src/auth/token.rs".into()],
                next_steps: vec!["Add integration tests".into(), "Update docs".into()],
            })
            .unwrap();

        assert_eq!(rollup.summary, "Implemented auth token rotation");
        assert_eq!(rollup.related_files, vec!["src/auth/token.rs"]);
        assert_eq!(
            rollup.next_steps,
            vec!["Add integration tests", "Update docs"]
        );

        let fetched = db.get_session_rollup_by_id(rollup.id).unwrap().unwrap();
        assert_eq!(fetched.id, rollup.id);
    }

    #[test]
    fn list_ordered_by_created_desc() {
        let db = test_db();
        let pid = create_test_project(&db);

        for i in 0..3 {
            db.create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: format!("Rollup {i}"),
                related_files: vec![],
                next_steps: vec![],
            })
            .unwrap();
        }

        let rollups = db.list_session_rollups_by_project(pid, 100).unwrap();
        assert_eq!(rollups.len(), 3);
        assert_eq!(rollups[0].summary, "Rollup 2");
        assert_eq!(rollups[2].summary, "Rollup 0");
    }

    #[test]
    fn delete_rollup() {
        let db = test_db();
        let pid = create_test_project(&db);
        let rollup = db
            .create_session_rollup(&NewSessionRollup {
                project_id: pid,
                summary: "Test".into(),
                related_files: vec![],
                next_steps: vec![],
            })
            .unwrap();
        db.delete_session_rollup(rollup.id).unwrap();
        assert!(db.get_session_rollup_by_id(rollup.id).unwrap().is_none());
    }

    #[test]
    fn every_stall_class_in_the_taxonomy_has_a_recovery() {
        assert_stall_taxonomy_complete().unwrap();
        assert!(stall_taxonomy_classes().contains(&"unclassified"));
    }

    #[test]
    fn an_unnamed_reason_classifies_as_unclassified_not_dropped() {
        assert_eq!(
            classify_attempt_stall(
                Some("some_future_reason_nobody_named_yet"),
                Some("retained")
            ),
            "unclassified"
        );
        assert_eq!(classify_attempt_stall(None, None), "interrupted");
    }

    #[test]
    fn scope_classes_recover_through_the_picker_not_release_or_complete() {
        let recovery = stall_recovery("scope_broadened", "PRD-1", "docs/prds/PRD-001.md");
        assert_eq!(
            recovery,
            StallRecovery::Command("familiar-ai scope-decisions".into())
        );
    }

    #[test]
    fn environment_denied_is_named_unrecoverable_rather_than_omitted() {
        let recovery = stall_recovery("environment_denied", "PRD-1", "docs/prds/PRD-001.md");
        assert!(matches!(recovery, StallRecovery::Unrecoverable(reason) if !reason.is_empty()));
    }

    fn seed_repo_session(db: &Database, session_id: &str, repository_key: &str) {
        DriverRepository::new(db.conn())
            .open_session(session_id, repository_key, "{}")
            .unwrap();
    }

    /// PRD-085 AC3's pinned regression: one session containing one PRD of
    /// each disposition — unattended, assisted, and stalled — computed
    /// purely from durable rows.
    #[test]
    fn session_autonomy_distinguishes_unattended_assisted_and_stalled() {
        let db = test_db();
        let repository_key = "/repo/.git";
        seed_repo_session(&db, "drive-autonomy", repository_key);
        let driver = DriverRepository::new(db.conn());

        // Unattended: completes with no human-actor row anywhere near it.
        let unattended_seq = driver
            .record_attempt_started("drive-autonomy", "PRD-1", "docs/prds/PRD-001.md", None)
            .unwrap();
        driver
            .record_attempt_finished(
                "drive-autonomy",
                unattended_seq,
                "completed",
                None,
                None,
                None,
            )
            .unwrap();

        // Assisted: completes, but a human approved a scope decision inside
        // its window.
        let assisted_seq = driver
            .record_attempt_started("drive-autonomy", "PRD-2", "docs/prds/PRD-002.md", None)
            .unwrap();
        let attempts = driver.attempts("drive-autonomy").unwrap();
        let assisted_started_at = attempts
            .iter()
            .find(|a| a.sequence == assisted_seq)
            .unwrap()
            .started_at
            .clone();
        CheckpointRepository::new(db.conn())
            .put(&ExecutionCheckpoint {
                checkpoint_id: "cp-2".into(),
                repository_key: repository_key.into(),
                prd_id: "PRD-2".into(),
                prd_path: "docs/prds/PRD-002.md".into(),
                execution_id: None,
                phase: "implemented".into(),
                base_revision: "deadbeef".into(),
                worktree_path: "/state/worktrees/PRD-2".into(),
                branch_name: None,
                diff_hash: "sha256:abc".into(),
                changed_files_json: "[]".into(),
                agent_identity: "claude-code".into(),
                usage_json: "{}".into(),
                test_evidence_json: "{}".into(),
                invalid_reason: None,
            })
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO scope_decisions(finding_hash,repository_key,checkpoint_id,prd_id,candidate_hash,finding_json,decision,actor,reason,created_at,decided_at) \
                 VALUES('sha256:f','/repo/.git','cp-2','PRD-2','sha256:c','{}','approved','human:alice','looked fine',?1,?1)",
                params![assisted_started_at],
            )
            .unwrap();
        driver
            .record_attempt_finished(
                "drive-autonomy",
                assisted_seq,
                "completed",
                None,
                None,
                None,
            )
            .unwrap();

        // Stalled: never completes, retained on a named class.
        let stalled_seq = driver
            .record_attempt_started("drive-autonomy", "PRD-3", "docs/prds/PRD-003.md", None)
            .unwrap();
        driver
            .record_attempt_finished(
                "drive-autonomy",
                stalled_seq,
                "retained",
                Some("verification_failed"),
                None,
                None,
            )
            .unwrap();

        let autonomy = session_autonomy(db.conn(), "drive-autonomy").unwrap();
        assert_eq!(autonomy.unattended_count(), 1);
        assert_eq!(autonomy.assisted().len(), 1);
        assert_eq!(autonomy.stalled().len(), 1);

        let assisted = &autonomy.assisted()[0];
        assert_eq!(assisted.prd_id, "PRD-2");
        assert!(matches!(
            &assisted.outcome,
            PrdAutonomyOutcome::Assisted { commands }
                if commands.iter().any(|c| c.contains("scope-decisions --approve"))
        ));

        let stalled = &autonomy.stalled()[0];
        assert_eq!(stalled.prd_id, "PRD-3");
        assert!(matches!(
            &stalled.outcome,
            PrdAutonomyOutcome::Stalled { stall_class, .. } if stall_class == "verification_failed"
        ));

        assert_eq!(autonomy.unattended_fraction(), Some(1.0 / 3.0));
        assert!(autonomy.is_autonomy_failure(0.75));
        assert!(!autonomy.is_autonomy_failure(0.25));
        assert_eq!(autonomy.dominant_stall_class(), Some("verification_failed"));
    }

    #[test]
    fn unattended_fraction_counts_stalls_in_the_denominator() {
        let mut autonomy = SessionAutonomy::default();
        autonomy.prds.push(PrdAutonomy {
            prd_id: "PRD-1".to_string(),
            prd_path: "docs/prds/PRD-001.md".to_string(),
            outcome: PrdAutonomyOutcome::Unattended,
        });
        for n in 2..=10 {
            autonomy.prds.push(PrdAutonomy {
                prd_id: format!("PRD-{n}"),
                prd_path: format!("docs/prds/PRD-{n:03}.md"),
                outcome: PrdAutonomyOutcome::Stalled {
                    stall_class: "verification_failed".to_string(),
                    recovery: StallRecovery::Command("familiar-ai driver retry".to_string()),
                },
            });
        }

        // Nine of ten PRDs stalled; the fraction must reflect that rather
        // than reporting a perfect 1.0 by dividing over completions alone.
        assert_eq!(autonomy.unattended_fraction(), Some(1.0 / 10.0));
        assert!(autonomy.is_autonomy_failure(0.5));
    }

    #[test]
    fn all_stalled_session_is_an_autonomy_failure_not_none() {
        let mut autonomy = SessionAutonomy::default();
        for n in 1..=3 {
            autonomy.prds.push(PrdAutonomy {
                prd_id: format!("PRD-{n}"),
                prd_path: format!("docs/prds/PRD-{n:03}.md"),
                outcome: PrdAutonomyOutcome::Stalled {
                    stall_class: "verification_failed".to_string(),
                    recovery: StallRecovery::Command("familiar-ai driver retry".to_string()),
                },
            });
        }

        assert_eq!(autonomy.unattended_fraction(), Some(0.0));
        assert!(autonomy.is_autonomy_failure(0.01));
        assert_eq!(autonomy.dominant_stall_class(), Some("verification_failed"));
    }

    #[test]
    fn session_with_no_prds_reports_no_autonomy_fraction() {
        let autonomy = SessionAutonomy::default();
        assert_eq!(autonomy.unattended_fraction(), None);
        assert!(!autonomy.is_autonomy_failure(0.5));
    }

    #[test]
    fn autonomy_for_window_groups_by_repository_stall_class_and_intervention_command() {
        let db = test_db();
        let repository_key = "/repo/.git";
        seed_repo_session(&db, "drive-window", repository_key);
        let driver = DriverRepository::new(db.conn());

        let unattended_seq = driver
            .record_attempt_started("drive-window", "PRD-1", "docs/prds/PRD-001.md", None)
            .unwrap();
        driver
            .record_attempt_finished(
                "drive-window",
                unattended_seq,
                "completed",
                None,
                None,
                None,
            )
            .unwrap();

        let stalled_seq = driver
            .record_attempt_started("drive-window", "PRD-2", "docs/prds/PRD-002.md", None)
            .unwrap();
        driver
            .record_attempt_finished(
                "drive-window",
                stalled_seq,
                "retained",
                Some("scope_broadened"),
                None,
                None,
            )
            .unwrap();

        let summary = autonomy_for_window(
            db.conn(),
            repository_key,
            "0000-01-01T00:00:00Z",
            "9999-01-01T00:00:00Z",
        )
        .unwrap();
        assert_eq!(summary.repository_key, repository_key);
        assert_eq!(summary.unattended_completions, 1);
        assert_eq!(summary.assisted_completions, 0);
        assert_eq!(summary.stalled, 1);
        assert_eq!(summary.by_stall_class.get("scope_broadened"), Some(&1));
    }
}
