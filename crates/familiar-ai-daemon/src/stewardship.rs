//! Shared, repository-scoped stewardship query rendering (PRD-035), used by
//! both the `familiar-ai stewardship` CLI subcommand and the dashboard's
//! `/stewardship/*` endpoints so they read the same boundary and agree on
//! the same facts. Every function is read-only and takes the exact
//! repository identity the caller resolved, so cross-repository reads
//! cannot leak state.

use serde_json::{json, Value};

use familiar_ai_core::RepositoryIdentity;
use familiar_ai_storage::{
    budget_summary, list_backlog_entries, list_recovery_events as repo_list_recovery_events,
    list_repository_keys, pending_human_gates, review_findings_for_session, AccountingRepository,
    CheckpointRepository, Database, DeliveryRepository, DriverRepository, OrchestrationRepository,
};

#[derive(Debug)]
pub enum StewardshipError {
    NotFound(String),
    Storage(String),
}

impl std::fmt::Display for StewardshipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(what) => write!(f, "{what}"),
            Self::Storage(message) => write!(f, "stewardship storage failed: {message}"),
        }
    }
}
impl std::error::Error for StewardshipError {}

fn storage(error: impl std::fmt::Display) -> StewardshipError {
    StewardshipError::Storage(error.to_string())
}

/// Free-text JSON-blob fields (usage/test-evidence/findings) are bounded and
/// scanned for secret markers before disclosure. Mirrors the equivalent
/// helper in `familiar-ai-mcp`; kept as an independent, deliberately small
/// duplicate rather than a new shared crate dependency.
const MAX_FIELD_BYTES: usize = 8 * 1024;
const SECRET_MARKERS: &[&str] = &[
    "-----begin private key-----",
    "-----begin rsa private key-----",
    "aws_secret_access_key",
    "authorization: bearer ",
    "github_pat_",
    "sk-proj-",
];

fn redact(raw: &str) -> Value {
    let lower = raw.to_ascii_lowercase();
    if SECRET_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return json!({"redacted": true, "reason": "secret marker detected"});
    }
    if raw.len() > MAX_FIELD_BYTES {
        let cut = floor_char_boundary(raw, MAX_FIELD_BYTES);
        return json!({
            "truncated": true,
            "original_bytes": raw.len(),
            "preview": &raw[..cut],
        });
    }
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Largest byte index `<= max` that lands on a UTF-8 char boundary of `s`.
/// Slicing a `&str` at a non-boundary index panics, and multi-byte
/// characters routinely straddle a fixed byte offset in model/execution
/// output, so the cut point must be found rather than assumed.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    let mut cut = max.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// The repositories this database knows about. Deliberately the one
/// stewardship read that is NOT repository-scoped: every other endpoint
/// requires a `repo` the caller must already have, and this is how a caller
/// gets one. It discloses only repository paths, never their contents.
pub fn list_repositories(db: &Database) -> Result<Value, StewardshipError> {
    let keys = list_repository_keys(db.conn()).map_err(storage)?;
    let items: Vec<Value> = keys
        .into_iter()
        .map(|key| {
            // Keys are stored as the git directory; the `repo` parameter
            // resolves from a working-tree path, so hand back the form the
            // caller can actually pass straight back to us.
            let path = key.strip_suffix("/.git").unwrap_or(&key).to_string();
            json!({"repository_key": key, "path": path})
        })
        .collect();
    Ok(json!({"repositories": items}))
}

pub fn list_backlog(
    db: &Database,
    repository: &RepositoryIdentity,
    status: Option<&str>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let items =
        list_backlog_entries(db.conn(), &repository.key, status, cursor, limit).map_err(storage)?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|item| item.prd_path.clone()))
        .flatten();
    Ok(json!({
        "repository_key": repository.key,
        "items": items,
        "next_cursor": next_cursor,
    }))
}

pub fn list_sessions(
    db: &Database,
    repository: &RepositoryIdentity,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let items = DriverRepository::new(db.conn())
        .list_sessions_by_repository(&repository.key, cursor, limit)
        .map_err(storage)?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|item| item.session_id.clone()))
        .flatten();
    Ok(json!({
        "repository_key": repository.key,
        "items": items,
        "next_cursor": next_cursor,
    }))
}

fn owned_session(
    db: &Database,
    repository: &RepositoryIdentity,
    session_id: &str,
) -> Result<familiar_ai_storage::DriverSession, StewardshipError> {
    DriverRepository::new(db.conn())
        .get_session(session_id)
        .map_err(storage)?
        .filter(|session| session.repository_key == repository.key)
        .ok_or_else(|| StewardshipError::NotFound("no such session in this repository".into()))
}

pub fn list_attempts(
    db: &Database,
    repository: &RepositoryIdentity,
    session_id: &str,
    cursor: Option<i64>,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let session = owned_session(db, repository, session_id)?;
    let items = DriverRepository::new(db.conn())
        .attempts_page(&session.session_id, cursor, limit)
        .map_err(storage)?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|item| item.sequence))
        .flatten();
    Ok(json!({
        "repository_key": repository.key,
        "session_id": session.session_id,
        "items": items,
        "next_cursor": next_cursor,
    }))
}

/// The rounds the driver has run in this repository, and which PRD each one
/// touched.
///
/// A "round" is a driver session: the unit the driver actually works in, one
/// warrant at a time. Calendar days are the wrong axis for a backlog that
/// advances in bursts and then sits still for a week.
///
/// Sessions come back oldest first — a waterfall reads left to right — while
/// the `limit` selects the most *recent* ones, because a chart truncated at
/// the far end should drop ancient history rather than today's work.
pub fn list_rounds(
    db: &Database,
    repository: &RepositoryIdentity,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let driver = DriverRepository::new(db.conn());
    let mut sessions = driver
        .list_sessions_by_repository(&repository.key, None, limit)
        .map_err(storage)?;
    sessions.reverse();
    let attempts = driver.rounds(&repository.key, limit).map_err(storage)?;
    let total = driver.count_sessions(&repository.key).map_err(storage)?;
    Ok(json!({
        "repository_key": repository.key,
        "sessions": sessions,
        "attempts": attempts,
        // So the view can say "showing the last 20 of 43" rather than
        // implying the driver has only ever run 20 times.
        "total_sessions": total,
    }))
}

pub fn list_checkpoints(
    db: &Database,
    repository: &RepositoryIdentity,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let items = CheckpointRepository::new(db.conn())
        .page(&repository.key, cursor, limit)
        .map_err(storage)?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|item| item.prd_id.clone()))
        .flatten();
    let items: Vec<Value> = items
        .into_iter()
        .map(|checkpoint| {
            json!({
                "checkpoint_id": checkpoint.checkpoint_id,
                "prd_id": checkpoint.prd_id,
                "prd_path": checkpoint.prd_path,
                "execution_id": checkpoint.execution_id,
                "phase": checkpoint.phase,
                "base_revision": checkpoint.base_revision,
                "worktree_path": checkpoint.worktree_path,
                "branch_name": checkpoint.branch_name,
                "diff_hash": checkpoint.diff_hash,
                "changed_files": redact(&checkpoint.changed_files_json),
                "agent_identity": checkpoint.agent_identity,
                "usage": redact(&checkpoint.usage_json),
                "test_evidence": redact(&checkpoint.test_evidence_json),
                "invalid_reason": checkpoint.invalid_reason,
            })
        })
        .collect();
    Ok(json!({
        "repository_key": repository.key,
        "items": items,
        "next_cursor": next_cursor,
    }))
}

pub fn list_recovery_events(
    db: &Database,
    repository: &RepositoryIdentity,
    cursor: Option<i64>,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let items =
        repo_list_recovery_events(db.conn(), &repository.key, cursor, limit).map_err(storage)?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|item| item.event_id))
        .flatten();
    Ok(json!({
        "repository_key": repository.key,
        "items": items,
        "next_cursor": next_cursor,
    }))
}

pub fn list_delivery_decisions(
    db: &Database,
    repository: &RepositoryIdentity,
    cursor: Option<&str>,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let items = DeliveryRepository::new(db.conn())
        .list_decisions(&repository.key, cursor, limit)
        .map_err(storage)?;
    let next_cursor = (items.len() == limit)
        .then(|| items.last().map(|item| item.decision_id.clone()))
        .flatten();
    let items: Vec<Value> = items
        .into_iter()
        .map(|decision| {
            json!({
                "decision_id": decision.decision_id,
                "session_id": decision.session_id,
                "prd_id": decision.prd_id,
                "mode": decision.mode,
                "actor": decision.actor,
                "decision": decision.decision,
                "assurance_label": decision.assurance_label,
                "findings": redact(&decision.findings_json),
                "stop_reasons": redact(&decision.stop_reasons_json),
                "warrant": decision.warrant_json.as_deref().map(redact),
                "warrant_consumed": decision.warrant_consumed,
                "created_at": decision.created_at,
            })
        })
        .collect();
    Ok(json!({
        "repository_key": repository.key,
        "items": items,
        "next_cursor": next_cursor,
    }))
}

pub fn get_budget(
    db: &Database,
    repository: &RepositoryIdentity,
    session_id: &str,
) -> Result<Value, StewardshipError> {
    let summary = budget_summary(db.conn(), session_id)
        .map_err(storage)?
        .filter(|summary| summary.repository_key == repository.key)
        .ok_or_else(|| StewardshipError::NotFound("no such session in this repository".into()))?;
    Ok(json!({
        "session_id": summary.session_id,
        "repository_key": summary.repository_key,
        "warrant": redact(&summary.warrant_json),
        "known_cost_microusd": summary.known_cost_microusd,
        "known_cost_attempts": summary.known_cost_attempts,
        "unknown_cost_attempts": summary.unknown_cost_attempts,
        "delivery_warrant_consumed": summary.delivery_warrant_consumed,
    }))
}

pub fn list_review_findings(
    db: &Database,
    repository: &RepositoryIdentity,
    session_id: &str,
) -> Result<Value, StewardshipError> {
    let items =
        review_findings_for_session(db.conn(), &repository.key, session_id).map_err(storage)?;
    Ok(json!({
        "repository_key": repository.key,
        "session_id": session_id,
        "items": items,
    }))
}

/// Current-effective reconciliation rows (PRD-053) for the durable project
/// this repository resolves to. Cached-only, read-only — never contacts a
/// provider. `NotFound` when the repository has no durable project binding
/// (never a degraded-identity leak of another repository's rows).
pub fn get_reconciliation(
    db: &Database,
    repository: &RepositoryIdentity,
    start: &str,
    end: &str,
) -> Result<Value, StewardshipError> {
    let accounting = AccountingRepository::new(db.conn());
    let project_id = accounting.project_id(&repository.key).map_err(|_| {
        StewardshipError::NotFound("repository is not bound to a durable project".into())
    })?;
    let start_time = chrono::DateTime::parse_from_rfc3339(start)
        .map_err(|e| StewardshipError::Storage(format!("invalid start timestamp: {e}")))?
        .with_timezone(&chrono::Utc);
    let end_time = chrono::DateTime::parse_from_rfc3339(end)
        .map_err(|e| StewardshipError::Storage(format!("invalid end timestamp: {e}")))?
        .with_timezone(&chrono::Utc);
    let rows = accounting
        .reconciliation_for_project(&project_id, start_time, end_time)
        .map_err(storage)?;
    let by_source = reconciliation_by_source(&rows);
    Ok(json!({
        "repository_key": repository.key,
        "project_id": project_id,
        "range_start": start,
        "range_end": end,
        "rows": rows,
        "by_source": by_source,
    }))
}

/// Sums `rows` per billing source within the caller's range (e.g.
/// month-to-date), preserving each amount's authority label — never mixing
/// estimated and authoritative into one number.
fn reconciliation_by_source(
    rows: &[familiar_ai_storage::ReconciliationRow],
) -> std::collections::BTreeMap<String, Value> {
    let mut by_source: std::collections::BTreeMap<String, (Option<i64>, Option<i64>)> =
        std::collections::BTreeMap::new();
    for row in rows {
        let entry = by_source
            .entry(row.billing_source.clone())
            .or_insert((None, None));
        if let Some(value) = row.local_estimate_nanousd {
            entry.0 = Some(entry.0.unwrap_or(0) + value);
        }
        if let Some(value) = row.authoritative_nanousd {
            entry.1 = Some(entry.1.unwrap_or(0) + value);
        }
    }
    by_source
        .into_iter()
        .map(|(source, (local, authoritative))| {
            (
                source,
                json!({"local_estimate_nanousd": local, "authoritative_nanousd": authoritative}),
            )
        })
        .collect()
}

/// The exact paste-runnable approve and reject commands for one hash-bound
/// pending scope decision, with an actor placeholder (PRD-083). Release and
/// complete never answer a scope decision, so wherever a scope pause is the
/// actual blocker these precede any release/complete continuation rather
/// than leaving it as the only one offered.
pub fn scope_decision_commands(finding_hash: &str, candidate_hash: &str) -> Vec<String> {
    vec![
        format!(
            "familiar-ai scope-decisions --finding-hash {finding_hash} --candidate-hash {candidate_hash} --approve --actor human:<identity> --reason \"<why>\""
        ),
        format!(
            "familiar-ai scope-decisions --finding-hash {finding_hash} --candidate-hash {candidate_hash} --reject --actor human:<identity> --reason \"<why>\""
        ),
    ]
}

/// PRD-083: one printable line per pending scope decision's exact approve
/// and reject remedy, prefixed for whichever surface is asking. Lets a
/// surface that has nothing else to say about a stalled backlog (e.g.
/// `familiar-ai next` finding no eligible PRD) say why instead of reporting
/// silence.
pub fn scope_pause_notices(
    pending_scope: &[familiar_ai_storage::ScopeDecision],
    prefix: &str,
) -> Vec<String> {
    pending_scope
        .iter()
        .flat_map(|decision| {
            let prd_id = decision.prd_id.clone();
            scope_decision_commands(&decision.finding_hash, &decision.candidate_hash)
                .into_iter()
                .map(move |command| {
                    format!("{prefix}: scope decision pending for {prd_id}: {command}")
                })
        })
        .collect()
}

pub fn list_pending_human_gates(
    db: &Database,
    repository: &RepositoryIdentity,
    limit: usize,
) -> Result<Value, StewardshipError> {
    let gates = pending_human_gates(db.conn(), &repository.key, limit).map_err(storage)?;
    let pending_scope = OrchestrationRepository::new(db.conn())
        .pending_scope_decisions(&repository.key)
        .map_err(storage)?;
    let items: Vec<Value> = gates
        .into_iter()
        .map(|gate| {
            let scope_commands: Vec<String> = pending_scope
                .iter()
                .filter(|decision| decision.prd_id == gate.prd_id)
                .flat_map(|decision| {
                    scope_decision_commands(&decision.finding_hash, &decision.candidate_hash)
                })
                .collect();
            let recovery_commands: Vec<String> = scope_commands
                .into_iter()
                .chain(gate.recovery_commands)
                .collect();
            json!({
                "kind": gate.kind,
                "session_id": gate.session_id,
                "prd_id": gate.prd_id,
                "prd_path": gate.prd_path,
                "detail": gate.detail,
                "recovery_commands": recovery_commands,
            })
        })
        .collect();
    Ok(json!({
        "repository_key": repository.key,
        "items": items,
    }))
}

/// PRD-085 AC4: autonomy over a bounded window of sessions, grouped by
/// repository, stall class, and intervention command, so the most frequent
/// human step is identifiable without reading transcripts.
pub fn get_autonomy(
    db: &Database,
    repository: &RepositoryIdentity,
    start: &str,
    end: &str,
) -> Result<Value, StewardshipError> {
    let summary = familiar_ai_storage::autonomy_for_window(db.conn(), &repository.key, start, end)
        .map_err(storage)?;
    Ok(json!({
        "repository_key": summary.repository_key,
        "range_start": start,
        "range_end": end,
        "unattended_completions": summary.unattended_completions,
        "assisted_completions": summary.assisted_completions,
        "stalled": summary.stalled,
        "by_stall_class": summary.by_stall_class,
        "by_intervention_command": summary.by_intervention_command,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_bounds_oversized_values_with_multibyte_boundary() {
        // A 3-byte UTF-8 character ('€') straddling the MAX_FIELD_BYTES cut
        // point must not panic; the cut must land before the character.
        let mut raw = "x".repeat(MAX_FIELD_BYTES - 1);
        raw.push('€');
        raw.push_str("more text to exceed the bound");
        let value = redact(&raw);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["original_bytes"], raw.len());
        let preview = value["preview"].as_str().unwrap();
        assert!(preview.len() <= MAX_FIELD_BYTES);
        assert_eq!(preview, &raw[..MAX_FIELD_BYTES - 1]);
    }
}
