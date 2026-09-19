//! PRD-099 gate override records (migration 074).
//!
//! A merge past a red or absent gate is allowed; doing it without a record is
//! not. The schema enforces a non-empty actor and reason, so an override that
//! names nobody cannot be written at all — "an unrecorded override is itself
//! refused" is a database constraint here, not a convention.

use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::params;

use familiar_ai_core::FamiliarError;

use super::now_rfc3339;
use crate::Database;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOverride {
    pub override_id: String,
    pub commit_sha: String,
    pub verdict: String,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
}

pub struct GateOverrideRepository<'a> {
    db: &'a Database,
}

impl<'a> GateOverrideRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Record an override of a non-green verdict. `verdict` is the verdict
    /// being overridden, kept so the record still means something once the
    /// forge has aged out the run it refers to.
    pub fn record(
        &self,
        commit_sha: &str,
        verdict: &str,
        actor: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<GateOverride> {
        let mut bytes = [0u8; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| FamiliarError::Database("secure override id generation failed".into()))?;
        let override_id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let created_at = now_rfc3339();
        self.db
            .conn()
            .execute(
                "INSERT INTO gate_overrides(override_id,commit_sha,verdict,actor,reason,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![override_id, commit_sha, verdict, actor, reason, created_at],
            )
            .map_err(|error| FamiliarError::Database(error.to_string()))?;
        Ok(GateOverride {
            override_id,
            commit_sha: commit_sha.to_string(),
            verdict: verdict.to_string(),
            actor: actor.to_string(),
            reason: reason.to_string(),
            created_at,
        })
    }

    /// The most recent override for a commit, if any.
    pub fn for_commit(&self, commit_sha: &str) -> familiar_ai_core::Result<Option<GateOverride>> {
        self.db
            .conn()
            .query_row(
                "SELECT override_id,commit_sha,verdict,actor,reason,created_at FROM gate_overrides WHERE commit_sha=?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![commit_sha],
                |row| {
                    Ok(GateOverride {
                        override_id: row.get(0)?,
                        commit_sha: row.get(1)?,
                        verdict: row.get(2)?,
                        actor: row.get(3)?,
                        reason: row.get(4)?,
                        created_at: row.get(5)?,
                    })
                },
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(FamiliarError::Database(other.to_string())),
            })
    }
}
