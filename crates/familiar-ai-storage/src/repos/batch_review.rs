//! Durable batch-tier review lifecycle (PRD-071). One row per review attempt
//! submitted to a provider batch interface, keyed by `review_id` so a
//! re-driven review cycle is idempotent: it never resubmits a still-pending
//! or already-resolved batch, and a completed result is consumed exactly
//! once.

use chrono::Utc;
use familiar_ai_core::FamiliarError;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchReviewState {
    Submitted,
    Completed,
    Applied,
    ExpiredFallback,
}

impl std::fmt::Display for BatchReviewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Submitted => "submitted",
            Self::Completed => "completed",
            Self::Applied => "applied",
            Self::ExpiredFallback => "expired_fallback",
        })
    }
}

impl BatchReviewState {
    fn parse(value: &str) -> familiar_ai_core::Result<Self> {
        Ok(match value {
            "submitted" => Self::Submitted,
            "completed" => Self::Completed,
            "applied" => Self::Applied,
            "expired_fallback" => Self::ExpiredFallback,
            other => {
                return Err(FamiliarError::Database(format!(
                    "unknown batch_reviews.state '{other}'"
                )))
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewBatchReview<'a> {
    pub review_id: &'a str,
    pub cycle_id: &'a str,
    pub repository_key: &'a str,
    pub prd_id: &'a str,
    pub risk_class: &'a str,
    pub provider: &'a str,
    pub provider_batch_id: &'a str,
    pub provider_request_id: Option<&'a str>,
    pub max_wait_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReviewRow {
    pub batch_review_id: String,
    pub review_id: String,
    pub cycle_id: String,
    pub repository_key: String,
    pub prd_id: String,
    pub risk_class: String,
    pub provider: String,
    pub provider_batch_id: String,
    pub provider_request_id: Option<String>,
    pub state: String,
    pub max_wait_ms: u64,
    pub submitted_at: String,
    pub deadline_at: String,
    pub polled_at: Option<String>,
    pub completed_at: Option<String>,
    pub result_json: Option<String>,
    pub provider_cost_lexical: Option<String>,
    pub fallback_reason: Option<String>,
}

pub struct BatchReviewRepository<'a> {
    conn: &'a Connection,
}

impl<'a> BatchReviewRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Idempotent by `review_id`: a resumed review cycle that reaches the
    /// same attempt again is a no-op here, never a duplicate submission.
    /// Returns `true` when this call actually created the row.
    #[allow(clippy::too_many_arguments)]
    pub fn submit(&self, value: &NewBatchReview<'_>) -> familiar_ai_core::Result<bool> {
        if value.max_wait_ms == 0 {
            return Err(FamiliarError::Database(
                "batch review submission requires a positive max_wait_ms".into(),
            ));
        }
        let now = Utc::now();
        let deadline = now
            + chrono::Duration::milliseconds(i64::try_from(value.max_wait_ms).unwrap_or(i64::MAX));
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO batch_reviews(batch_review_id,review_id,cycle_id,repository_key,prd_id,risk_class,provider,provider_batch_id,provider_request_id,state,max_wait_ms,submitted_at,deadline_at,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'submitted',?10,?11,?12,?11,?11)",
            params![
                format!("bre_{}", value.review_id),
                value.review_id,
                value.cycle_id,
                value.repository_key,
                value.prd_id,
                value.risk_class,
                value.provider,
                value.provider_batch_id,
                value.provider_request_id,
                value.max_wait_ms,
                now.to_rfc3339(),
                deadline.to_rfc3339(),
            ],
        ).map_err(db)?;
        Ok(changed == 1)
    }

    pub fn find_by_review_id(
        &self,
        review_id: &str,
    ) -> familiar_ai_core::Result<Option<BatchReviewRow>> {
        self.conn
            .query_row(
                &format!("{SELECT_ROW} WHERE review_id=?1"),
                [review_id],
                map_row,
            )
            .optional()
            .map_err(db)
    }

    /// All rows still awaiting a provider result, across every repository —
    /// the daemon's poll set, and the source of "resume polling from durable
    /// state" after a crash or restart.
    pub fn submitted(&self) -> familiar_ai_core::Result<Vec<BatchReviewRow>> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "{SELECT_ROW} WHERE state='submitted' ORDER BY submitted_at"
            ))
            .map_err(db)?;
        let rows = statement.query_map([], map_row).map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)
    }

    pub fn mark_polled(&self, review_id: &str) -> familiar_ai_core::Result<()> {
        self.conn
            .execute(
                "UPDATE batch_reviews SET polled_at=?2,updated_at=?2 WHERE review_id=?1 AND state='submitted'",
                params![review_id, Utc::now().to_rfc3339()],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Records a provider-completed batch result. Only takes effect from
    /// `submitted` state, so a late-arriving duplicate poll after expiry
    /// fallback already recorded a different terminal state is a no-op.
    pub fn mark_completed(
        &self,
        review_id: &str,
        result_json: &str,
        provider_cost_lexical: Option<&str>,
    ) -> familiar_ai_core::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE batch_reviews SET state='completed',result_json=?2,provider_cost_lexical=?3,completed_at=?4,updated_at=?4 WHERE review_id=?1 AND state='submitted'",
            params![review_id, result_json, provider_cost_lexical, now],
        ).map_err(db)?;
        Ok(changed == 1)
    }

    /// Reads a completed result without transitioning state. Callers must
    /// parse the payload and record its accounting observation *before*
    /// calling [`mark_applied`](Self::mark_applied) — never transition to
    /// `applied` on the strength of a read alone, or a failure between the
    /// two permanently strands the row with no consumer.
    pub fn peek_completed(&self, review_id: &str) -> familiar_ai_core::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT result_json FROM batch_reviews WHERE review_id=?1 AND state='completed'",
                [review_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)
    }

    /// Atomically transitions `completed` -> `applied`. Callers must have
    /// already parsed the result and recorded its accounting observation
    /// (both idempotent, safe to repeat) before calling this — it is the
    /// single point that commits a batch result to disposition. Returns
    /// `false` when a concurrent caller (a duplicate resume racing the
    /// daemon's own poller) already won this transition, so the loser never
    /// applies the same batch result twice.
    pub fn mark_applied(&self, review_id: &str) -> familiar_ai_core::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE batch_reviews SET state='applied',updated_at=?2 WHERE review_id=?1 AND state='completed'",
            params![review_id, Utc::now().to_rfc3339()],
        ).map_err(db)?;
        Ok(changed == 1)
    }

    /// Recovery for an `applied` row that re-enters `review()` with no
    /// disposition on record — e.g. a crash between returning the parsed
    /// result and the coordinator persisting it. Durably records why the
    /// already-applied batch result could not be trusted as still pending
    /// and hands the attempt to the interactive tier instead of hard-erroring
    /// forever. Returns `false` when the row already moved on (a concurrent
    /// caller got here first), so callers should not treat that as fatal.
    pub fn mark_applied_reentry_fallback(
        &self,
        review_id: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE batch_reviews SET state='expired_fallback',fallback_reason=?2,updated_at=?3 WHERE review_id=?1 AND state='applied'",
            params![review_id, reason, now],
        ).map_err(db)?;
        Ok(changed == 1)
    }

    /// Records that the configured maximum batch wait expired before the
    /// provider resolved this attempt, and why: a durable, reasoned fact
    /// distinct from a normal completion. Only takes effect from
    /// `submitted`, so a result that arrives concurrently with an expiry
    /// check is never overwritten by the fallback.
    pub fn mark_expired_fallback(
        &self,
        review_id: &str,
        reason: &str,
    ) -> familiar_ai_core::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE batch_reviews SET state='expired_fallback',fallback_reason=?2,updated_at=?3 WHERE review_id=?1 AND state='submitted'",
            params![review_id, reason, now],
        ).map_err(db)?;
        Ok(changed == 1)
    }
}

const SELECT_ROW: &str = "SELECT batch_review_id,review_id,cycle_id,repository_key,prd_id,risk_class,provider,provider_batch_id,provider_request_id,state,max_wait_ms,submitted_at,deadline_at,polled_at,completed_at,result_json,provider_cost_lexical,fallback_reason FROM batch_reviews";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BatchReviewRow> {
    Ok(BatchReviewRow {
        batch_review_id: row.get(0)?,
        review_id: row.get(1)?,
        cycle_id: row.get(2)?,
        repository_key: row.get(3)?,
        prd_id: row.get(4)?,
        risk_class: row.get(5)?,
        provider: row.get(6)?,
        provider_batch_id: row.get(7)?,
        provider_request_id: row.get(8)?,
        state: row.get(9)?,
        max_wait_ms: row.get(10)?,
        submitted_at: row.get(11)?,
        deadline_at: row.get(12)?,
        polled_at: row.get(13)?,
        completed_at: row.get(14)?,
        result_json: row.get(15)?,
        provider_cost_lexical: row.get(16)?,
        fallback_reason: row.get(17)?,
    })
}

impl BatchReviewRow {
    pub fn state(&self) -> familiar_ai_core::Result<BatchReviewState> {
        BatchReviewState::parse(&self.state)
    }
}

fn db(e: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn open() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn new_row<'a>(review_id: &'a str) -> NewBatchReview<'a> {
        NewBatchReview {
            review_id,
            cycle_id: "cycle-1",
            repository_key: "repo/.git",
            prd_id: "PRD-001",
            risk_class: "low-risk-docs",
            provider: "anthropic-api",
            provider_batch_id: "batch_abc",
            provider_request_id: None,
            max_wait_ms: 3_600_000,
        }
    }

    #[test]
    fn submit_is_idempotent_by_review_id() {
        let db = open();
        let repository = BatchReviewRepository::new(db.conn());
        assert!(repository.submit(&new_row("cycle-1-review-1")).unwrap());
        assert!(!repository.submit(&new_row("cycle-1-review-1")).unwrap());
        let row = repository
            .find_by_review_id("cycle-1-review-1")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "submitted");
    }

    #[test]
    fn completed_result_is_consumed_exactly_once() {
        let db = open();
        let repository = BatchReviewRepository::new(db.conn());
        repository.submit(&new_row("review-1")).unwrap();
        assert!(repository
            .mark_completed("review-1", "{\"ok\":true}", Some("$0.01"))
            .unwrap());
        // A second completion report after the first is a no-op, not a
        // double-write of a different result.
        assert!(!repository
            .mark_completed("review-1", "{\"ok\":false}", None)
            .unwrap());
        // Peeking never transitions state — safe to call repeatedly while
        // parsing/accounting run.
        let peeked = repository.peek_completed("review-1").unwrap();
        assert_eq!(peeked.as_deref(), Some("{\"ok\":true}"));
        assert_eq!(
            repository.peek_completed("review-1").unwrap().as_deref(),
            Some("{\"ok\":true}")
        );
        assert!(repository.mark_applied("review-1").unwrap());
        // Applied once; a second concurrent consumer never re-applies it.
        assert!(!repository.mark_applied("review-1").unwrap());
        assert_eq!(repository.peek_completed("review-1").unwrap(), None);
        let row = repository.find_by_review_id("review-1").unwrap().unwrap();
        assert_eq!(row.state, "applied");
    }

    #[test]
    fn applied_reentry_recovers_to_interactive_fallback_with_a_durable_reason() {
        let db = open();
        let repository = BatchReviewRepository::new(db.conn());
        repository.submit(&new_row("review-1")).unwrap();
        repository
            .mark_completed("review-1", "{\"ok\":true}", None)
            .unwrap();
        assert!(repository.mark_applied("review-1").unwrap());
        assert!(repository
            .mark_applied_reentry_fallback(
                "review-1",
                "resumed_after_applied_with_no_recorded_disposition"
            )
            .unwrap());
        let row = repository.find_by_review_id("review-1").unwrap().unwrap();
        assert_eq!(row.state, "expired_fallback");
        assert_eq!(
            row.fallback_reason.as_deref(),
            Some("resumed_after_applied_with_no_recorded_disposition")
        );
        // Already moved on: a second reentry recovery is a no-op, not a
        // second fallback reason overwrite.
        assert!(!repository
            .mark_applied_reentry_fallback("review-1", "different_reason")
            .unwrap());
    }

    #[test]
    fn expiry_fallback_is_recorded_with_reason_and_never_overwrites_a_completion() {
        let db = open();
        let repository = BatchReviewRepository::new(db.conn());
        repository.submit(&new_row("review-1")).unwrap();
        assert!(repository
            .mark_expired_fallback("review-1", "max_batch_wait_exceeded")
            .unwrap());
        let row = repository.find_by_review_id("review-1").unwrap().unwrap();
        assert_eq!(row.state, "expired_fallback");
        assert_eq!(
            row.fallback_reason.as_deref(),
            Some("max_batch_wait_exceeded")
        );
        // Already-terminal: a completion arriving after expiry never
        // resurrects it.
        assert!(!repository
            .mark_completed("review-1", "{\"ok\":true}", None)
            .unwrap());
    }

    #[test]
    fn submitted_lists_only_still_pending_rows() {
        let db = open();
        let repository = BatchReviewRepository::new(db.conn());
        repository.submit(&new_row("review-1")).unwrap();
        repository.submit(&new_row("review-2")).unwrap();
        repository
            .mark_expired_fallback("review-2", "max_batch_wait_exceeded")
            .unwrap();
        let pending = repository.submitted().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].review_id, "review-1");
    }
}
