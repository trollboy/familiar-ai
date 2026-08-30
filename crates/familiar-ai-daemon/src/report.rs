//! The morning report: one screen answering what got built, what stopped and
//! exactly why, what it cost, and what needs human judgment.
//!
//! Pure deterministic rendering of records Familiar already holds — driver
//! sessions and attempts, execution history, and persisted review findings.
//! It computes nothing, invokes no model, and mutates nothing. Identical
//! database state yields byte-identical output, so only stored timestamps are
//! rendered: never "now", never elapsed-since.

use std::fmt::Write as _;

use familiar_ai_review::ScopeDecision;
use familiar_ai_storage::{
    CheckpointRepository, Database, DeliveryRepository, DriverAttempt, ExecutionHistoryRepository,
    ReviewRepository,
};
use familiar_ai_storage::{DriverRepository, DriverSession};

/// Attempts listed per section before the remainder is summarized.
const MAX_LISTED_ATTEMPTS: usize = 20;
/// Scope findings shown per stopped attempt before the remainder is summarized.
const MAX_LISTED_FINDINGS: usize = 5;

#[derive(Debug)]
pub enum ReportError {
    NoSession,
    UnknownSession(String),
    Storage(String),
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSession => write!(
                f,
                "no driver session has been recorded yet; run `familiar-ai drive` first"
            ),
            Self::UnknownSession(id) => write!(f, "no driver session with id {id}"),
            Self::Storage(message) => write!(f, "report storage failed: {message}"),
        }
    }
}
impl std::error::Error for ReportError {}

/// Render one driver session: the named one, or the most recent.
pub fn render(db: &Database, session_id: Option<&str>) -> Result<String, ReportError> {
    let sessions = DriverRepository::new(db.conn());
    let session = match session_id {
        Some(id) => sessions
            .get_session(id)
            .map_err(storage)?
            .ok_or_else(|| ReportError::UnknownSession(id.to_owned()))?,
        None => sessions
            .latest_session()
            .map_err(storage)?
            .ok_or(ReportError::NoSession)?,
    };
    let attempts = sessions.attempts(&session.session_id).map_err(storage)?;

    let mut out = String::new();
    render_header(&mut out, &session);
    let (built, stopped): (Vec<_>, Vec<_>) = attempts
        .iter()
        .partition(|attempt| attempt.outcome.as_deref() == Some("completed"));
    render_built(db, &mut out, &built);
    render_escalations(&mut out, &attempts);
    render_stopped(db, &mut out, &stopped);
    render_authority(db, &mut out, &session.session_id)?;
    render_recovery(db, &mut out, &session.repository_key)?;
    render_cost(db, &mut out, &attempts)?;
    render_judgment(&mut out, &session, &stopped);
    Ok(familiar_ai_agent::redact_sensitive(out))
}

fn render_escalations(out: &mut String, attempts: &[DriverAttempt]) {
    let escalations: Vec<_> = attempts
        .iter()
        .filter(|attempt| attempt.escalated_from_sequence.is_some())
        .collect();
    let _ = writeln!(out, "\nESCALATIONS ({})", escalations.len());
    if escalations.is_empty() {
        let _ = writeln!(out, "  (none)");
        return;
    }
    for attempt in escalations.iter().take(MAX_LISTED_ATTEMPTS) {
        let _ = writeln!(
            out,
            "  {}  attempt={} from={} reason={} worker={} model={} outcome={}",
            attempt.prd_id,
            attempt.sequence,
            attempt.escalated_from_sequence.unwrap(),
            attempt.escalation_reason.as_deref().unwrap_or("unknown"),
            attempt.adapter_id.as_deref().unwrap_or("unknown"),
            attempt.model.as_deref().unwrap_or("unknown"),
            attempt.outcome.as_deref().unwrap_or("interrupted"),
        );
    }
    render_omitted(out, escalations.len(), MAX_LISTED_ATTEMPTS);
}

fn render_authority(db: &Database, out: &mut String, session_id: &str) -> Result<(), ReportError> {
    let decisions = DeliveryRepository::new(db.conn())
        .decisions_for_session(session_id)
        .map_err(storage)?;
    if decisions.is_empty() {
        return Ok(());
    }
    let _ = writeln!(out, "\nAUTHORITY DECISIONS ({})", decisions.len());
    for decision in decisions {
        let _ = writeln!(
            out,
            "  {} mode={} actor={} decision={} assurance={} warrant_consumed={}",
            decision.prd_id,
            decision.mode,
            decision.actor,
            decision.decision,
            decision.assurance_label.as_deref().unwrap_or("standard"),
            decision.warrant_consumed
        );
        let _ = writeln!(out, "      stop_reasons={}", decision.stop_reasons_json);
        let _ = writeln!(out, "      findings={}", decision.findings_json);
    }
    Ok(())
}

fn render_recovery(
    db: &Database,
    out: &mut String,
    repository_key: &str,
) -> Result<(), ReportError> {
    let checkpoints = CheckpointRepository::new(db.conn())
        .all(repository_key)
        .map_err(storage)?;
    let mut resumed = 0;
    let mut completed = 0;
    let mut blocked = 0;
    let mut invalid = 0;
    for checkpoint in &checkpoints {
        match checkpoint.phase.as_str() {
            "completed" | "integrated" => completed += 1,
            "blocked" => blocked += 1,
            "invalid_checkpoint" => invalid += 1,
            _ => resumed += 1,
        }
    }
    let _ = writeln!(out, "\nRECOVERY");
    let _ = writeln!(
        out,
        "  resumable={resumed} completed={completed} blocked={blocked} skipped=0 invalid={invalid}"
    );
    for checkpoint in checkpoints.iter().take(MAX_LISTED_ATTEMPTS) {
        let _ = writeln!(
            out,
            "  {}  phase={}  checkpoint={}{}",
            checkpoint.prd_id,
            checkpoint.phase,
            checkpoint.checkpoint_id,
            checkpoint
                .invalid_reason
                .as_ref()
                .map(|reason| format!("  reason={reason}"))
                .unwrap_or_default()
        );
    }
    render_omitted(out, checkpoints.len(), MAX_LISTED_ATTEMPTS);
    Ok(())
}

fn storage(error: impl std::fmt::Display) -> ReportError {
    ReportError::Storage(error.to_string())
}

fn render_header(out: &mut String, session: &DriverSession) {
    let _ = writeln!(out, "Familiar morning report");
    let _ = writeln!(out, "session:     {}", session.session_id);
    let _ = writeln!(out, "started:     {}", session.started_at);
    match (&session.ended_at, &session.termination_reason) {
        (Some(ended), Some(reason)) => {
            let _ = writeln!(out, "ended:       {ended}");
            let _ = writeln!(out, "termination: {reason}");
        }
        // A session without an end row died mid-flight; say so rather than
        // rendering a blank where a reason belongs.
        _ => {
            let _ = writeln!(out, "ended:       — (interrupted)");
            let _ = writeln!(out, "termination: interrupted (no termination recorded)");
        }
    }
    let _ = writeln!(out, "warrant:     {}", session.warrant_json);
    if let Some(detail) = &session.termination_detail {
        let _ = writeln!(out, "detail:      {detail}");
    }
}

fn render_built(db: &Database, out: &mut String, built: &[&DriverAttempt]) {
    let _ = writeln!(out, "\nBUILT ({})", built.len());
    if built.is_empty() {
        let _ = writeln!(out, "  (nothing completed)");
        return;
    }
    for attempt in built.iter().take(MAX_LISTED_ATTEMPTS) {
        let _ = writeln!(
            out,
            "  {}  {}  duration={}  cost={}  phase={}",
            attempt.prd_id,
            attempt.prd_path,
            optional_ms(attempt.duration_ms),
            optional_cost(attempt.known_cost_microusd),
            attempt.last_durable_phase.as_deref().unwrap_or("unknown")
        );
        let _ = writeln!(
            out,
            "      configuration: review={} execution_context={}",
            attempt.review_configuration_source, attempt.execution_context_configuration_source
        );
        render_workspace(out, attempt);
        render_scope_detail(db, out, attempt);
    }
    render_omitted(out, built.len(), MAX_LISTED_ATTEMPTS);
}

fn render_stopped(db: &Database, out: &mut String, stopped: &[&DriverAttempt]) {
    let _ = writeln!(out, "\nSTOPPED ({})", stopped.len());
    if stopped.is_empty() {
        let _ = writeln!(out, "  (nothing stopped)");
        return;
    }
    for attempt in stopped.iter().take(MAX_LISTED_ATTEMPTS) {
        let reason =
            attempt
                .retained_reason
                .as_deref()
                .unwrap_or(match attempt.outcome.as_deref() {
                    // An attempt with no outcome row never finished.
                    None => "interrupted (attempt did not finish)",
                    Some(_) => "unrecorded",
                });
        let _ = writeln!(
            out,
            "  {}  {}  reason={reason}",
            attempt.prd_id, attempt.prd_path
        );
        render_workspace(out, attempt);
        let _ = writeln!(
            out,
            "      configuration: review={} execution_context={}",
            attempt.review_configuration_source, attempt.execution_context_configuration_source
        );
        if attempt.adapter_id.is_some()
            || attempt.model.is_some()
            || attempt.exit_code.is_some()
            || attempt.signal.is_some()
            || attempt.last_durable_phase.is_some()
        {
            let _ = writeln!(
                out,
                "      attempt={} adapter={} model={} exit={:?} signal={:?} phase={}",
                attempt.sequence,
                attempt.adapter_id.as_deref().unwrap_or("unknown"),
                attempt.model.as_deref().unwrap_or("unknown"),
                attempt.exit_code,
                attempt.signal,
                attempt.last_durable_phase.as_deref().unwrap_or("unknown")
            );
        }
        render_scope_detail(db, out, attempt);
    }
    render_omitted(out, stopped.len(), MAX_LISTED_ATTEMPTS);
}

fn render_workspace(out: &mut String, attempt: &DriverAttempt) {
    if attempt.component_id.is_some() || attempt.worktree_path.is_some() || attempt.branch.is_some()
    {
        let _ = writeln!(
            out,
            "      component={} worktree={} branch={}",
            attempt.component_id.as_deref().unwrap_or("serial"),
            attempt.worktree_path.as_deref().unwrap_or("primary"),
            attempt.branch.as_deref().unwrap_or("primary")
        );
    }
}

/// The exact file and rule that stopped a scope-broadened attempt, as
/// persisted by the review cycle. Absent review is the normal case when
/// review is disabled and is reported as such, never as an error.
fn render_scope_detail(db: &Database, out: &mut String, attempt: &DriverAttempt) {
    let Some(execution_id) = attempt.execution_id.as_deref() else {
        return;
    };
    let cycle = ReviewRepository::new(db.conn())
        .get_cycle(&format!("{execution_id}-cycle"))
        .ok()
        .flatten();
    let Some(cycle) = cycle else {
        return;
    };
    if let Some(selection) = &cycle.tier_selection {
        let usage = cycle.review_attempts.iter().fold(0_u64, |total, stage| {
            total.saturating_add(stage.usage.total_tokens.unwrap_or(0))
        });
        let reviewer = cycle
            .reviewer
            .as_ref()
            .map(|value| value.assignment.agent_id.as_str())
            .unwrap_or("none");
        let _ = writeln!(
            out,
            "      review-tier: {:?} rule={} files={} bytes={} reviewer={} review_tokens={}",
            selection.tier,
            selection
                .selecting_rule
                .as_deref()
                .unwrap_or("default-full"),
            selection.footprint.changed_files,
            selection.footprint.changed_bytes,
            reviewer,
            if cycle
                .review_attempts
                .iter()
                .all(|stage| stage.usage.total_tokens.is_some())
            {
                usage.to_string()
            } else if cycle.review_attempts.is_empty() {
                "0".into()
            } else {
                "unknown".into()
            }
        );
    }
    let Some(evaluation) = cycle.scope_evaluations.last() else {
        return;
    };
    let blocking: Vec<_> = evaluation
        .findings
        .iter()
        .filter(|finding| {
            !matches!(
                finding.decision,
                ScopeDecision::AllowedChange | ScopeDecision::JustifiedExpectedFileChange
            )
        })
        .collect();
    for finding in blocking.iter().take(MAX_LISTED_FINDINGS) {
        let _ = writeln!(
            out,
            "      scope: {} {:?} {:?} rule={}",
            finding.path, finding.change_kind, finding.decision, finding.rule_id
        );
    }
    if blocking.len() > MAX_LISTED_FINDINGS {
        let _ = writeln!(
            out,
            "      … {} more scope findings omitted (all persisted under policy {})",
            blocking.len() - MAX_LISTED_FINDINGS,
            evaluation.policy_snapshot_hash
        );
    }
}

/// Known cost and unknown-cost attempts are reported separately: an
/// unmeasurable attempt is never summed as zero.
fn render_cost(
    db: &Database,
    out: &mut String,
    attempts: &[DriverAttempt],
) -> Result<(), ReportError> {
    let known: u64 = attempts
        .iter()
        .filter_map(|attempt| attempt.known_cost_microusd)
        .sum();
    let known_count = attempts
        .iter()
        .filter(|attempt| attempt.known_cost_microusd.is_some())
        .count();
    let unknown_count = attempts.len() - known_count;
    let _ = writeln!(out, "\nCOST");
    let _ = writeln!(
        out,
        "  known:   {known} micro-USD across {known_count} attempt(s)"
    );
    let _ = writeln!(
        out,
        "  unknown: {unknown_count} attempt(s) with no measurable cost"
    );
    let history = ExecutionHistoryRepository::new(db.conn());
    let mut measured_input = 0_u64;
    let mut cached = 0_u64;
    let mut measured = 0_u64;
    let mut unmeasured = 0_u64;
    let mut savings = 0_u64;
    let mut priced = 0_u64;
    let mut unpriced = 0_u64;
    for attempt in attempts {
        let record = match attempt.execution_id.as_deref() {
            Some(id) => history.get(id).map_err(storage)?,
            None => None,
        };
        match record {
            Some(record) => match (record.input_tokens, record.cached_tokens) {
                (Some(input), Some(hit)) if hit <= input => {
                    measured = measured.saturating_add(1);
                    measured_input = measured_input.saturating_add(input);
                    cached = cached.saturating_add(hit);
                    match (record.input_rate, record.cached_input_rate) {
                        (Some(full), Some(cache)) if cache <= full => {
                            priced = priced.saturating_add(1);
                            savings = savings.saturating_add(
                                hit.saturating_mul(full - cache).saturating_add(500_000)
                                    / 1_000_000,
                            );
                        }
                        _ => unpriced = unpriced.saturating_add(1),
                    }
                }
                _ => unmeasured = unmeasured.saturating_add(1),
            },
            None => unmeasured = unmeasured.saturating_add(1),
        }
    }
    let _ = writeln!(out, "\nCACHE");
    if measured_input == 0 {
        let _ = writeln!(out, "  cached share: — (no measured input/cache pairs)");
    } else {
        let _ = writeln!(
            out,
            "  cached share: {:.2}% ({measured} measured attempt(s))",
            cached as f64 * 100.0 / measured_input as f64
        );
    }
    let _ = writeln!(out, "  unmeasured:   {unmeasured} attempt(s)");
    let _ = writeln!(out, "  known savings: {savings} micro-USD across {priced} attempt(s) (persisted execution-history pricing)");
    let _ = writeln!(
        out,
        "  savings without pricing provenance: {unpriced} attempt(s)"
    );
    Ok(())
}

fn render_judgment(out: &mut String, session: &DriverSession, stopped: &[&DriverAttempt]) {
    let _ = writeln!(out, "\nNEEDS HUMAN JUDGMENT ({})", stopped.len());
    match session.termination_reason.as_deref() {
        Some("cost_unknown") => {
            let _ = writeln!(
                out,
                "  ! session ended because an attempt's cost could not be measured while a"
            );
            let _ = writeln!(
                out,
                "    cost ceiling was in force; configure [execution_history.pricing] or drop"
            );
            let _ = writeln!(out, "    the cost ceiling to continue unattended.");
        }
        Some("storage_failure") => {
            let _ = writeln!(
                out,
                "  ! session ended on a storage failure; the account above may be incomplete."
            );
        }
        Some("delivery_blocked") => {
            let _ = writeln!(
                out,
                "  ! a clean implementation could not cross the delivery boundary; inspect its"
            );
            let _ = writeln!(
                out,
                "    adjacent .delivery.json journal for the exact publish, merge, staging, or rollback blocker."
            );
        }
        Some("budget_deliveries_exhausted") => {
            let _ = writeln!(
                out,
                "  ! the finite delivery warrant was exhausted; remaining reviewed worktrees stay"
            );
            let _ = writeln!(
                out,
                "    ready_for_delivery and require a later bounded delivery session."
            );
        }
        Some("budget_tokens_exhausted") => {
            let _ = writeln!(
                out,
                "  ! the cumulative session token ceiling was reached; remaining work was not launched."
            );
        }
        _ => {}
    }
    if stopped.is_empty() {
        let _ = writeln!(out, "  (nothing awaiting a decision)");
        return;
    }
    for attempt in stopped.iter().take(MAX_LISTED_ATTEMPTS) {
        let _ = writeln!(out, "  {} {}", attempt.prd_id, attempt.prd_path);
        let _ = writeln!(
            out,
            "    familiar-ai backlog release {} --actor human:<you> --reason \"<why>\"",
            attempt.prd_path
        );
        let _ = writeln!(
            out,
            "    familiar-ai backlog complete {} --actor human:<you> --reason \"<why>\"",
            attempt.prd_path
        );
    }
    render_omitted(out, stopped.len(), MAX_LISTED_ATTEMPTS);
}

fn render_omitted(out: &mut String, total: usize, cap: usize) {
    if total > cap {
        let _ = writeln!(out, "  … {} more omitted", total - cap);
    }
}

fn optional_ms(value: Option<u64>) -> String {
    value
        .map(|v| format!("{v}ms"))
        .unwrap_or_else(|| "—".into())
}

fn optional_cost(value: Option<u64>) -> String {
    value
        .map(|v| format!("{v} micro-USD"))
        .unwrap_or_else(|| "— (unknown)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    /// A session whose attempts are recorded exactly as the driver records
    /// them: started, then finished with an outcome.
    fn seed<'a>(db: &'a Database, session_id: &str, warrant: &str) -> DriverRepository<'a> {
        let repository = DriverRepository::new(db.conn());
        repository
            .open_session(session_id, "/repo/.git", warrant)
            .unwrap();
        repository
    }

    #[test]
    fn absent_sessions_error_rather_than_rendering_emptiness() {
        let db = database();
        assert!(matches!(
            render(&db, None).unwrap_err(),
            ReportError::NoSession
        ));
        assert!(matches!(
            render(&db, Some("nope")).unwrap_err(),
            ReportError::UnknownSession(_)
        ));
    }

    #[test]
    fn mixed_session_renders_every_section_byte_exactly() {
        let db = database();
        let repository = seed(&db, "drive-1", r#"{"max_prds":3}"#);
        let first = repository
            .record_attempt_started_with_sources(
                "drive-1",
                "PRD-17",
                "docs/prds/PRD-017.md",
                Some("exec-1"),
                "repository",
                "global",
            )
            .unwrap();
        repository
            .record_attempt_diagnostics(
                "drive-1",
                first,
                Some("exec-1"),
                None,
                None,
                None,
                None,
                "completed",
            )
            .unwrap();
        repository
            .record_attempt_finished(
                "drive-1",
                first,
                "completed",
                None,
                Some(2_500),
                Some(1_200),
            )
            .unwrap();
        let second = repository
            .record_attempt_started("drive-1", "PRD-18", "docs/prds/PRD-018.md", Some("exec-2"))
            .unwrap();
        repository
            .record_attempt_finished(
                "drive-1",
                second,
                "retained",
                Some("review_disabled"),
                None,
                Some(900),
            )
            .unwrap();
        repository
            .finish_session("drive-1", "nothing_eligible")
            .unwrap();

        // Timestamps are the only non-constant content; take them from the
        // stored rows so the rest of the screen is pinned byte-exactly.
        let session = repository.get_session("drive-1").unwrap().unwrap();
        let expected = format!(
            "Familiar morning report\n\
             session:     drive-1\n\
             started:     {started}\n\
             ended:       {ended}\n\
             termination: nothing_eligible\n\
             warrant:     {{\"max_prds\":3}}\n\
             \n\
             BUILT (1)\n  \
             PRD-17  docs/prds/PRD-017.md  duration=1200ms  cost=2500 micro-USD  phase=completed\n\
             \x20\x20\x20\x20\x20\x20configuration: review=repository execution_context=global\n\
             \n\
             ESCALATIONS (0)\n  \
             (none)\n\
             \n\
             STOPPED (1)\n  \
             PRD-18  docs/prds/PRD-018.md  reason=review_disabled\n\
             \x20\x20\x20\x20\x20\x20configuration: review=global execution_context=global\n\
             \n\
             RECOVERY\n  \
             resumable=0 completed=0 blocked=0 skipped=0 invalid=0\n\
             \n\
             COST\n  \
             known:   2500 micro-USD across 1 attempt(s)\n  \
             unknown: 1 attempt(s) with no measurable cost\n\
             \n\
             CACHE\n  \
             cached share: — (no measured input/cache pairs)\n  \
             unmeasured:   2 attempt(s)\n  \
             known savings: 0 micro-USD across 0 attempt(s) (persisted execution-history pricing)\n  \
             savings without pricing provenance: 0 attempt(s)\n\
             \n\
             NEEDS HUMAN JUDGMENT (1)\n  \
             PRD-18 docs/prds/PRD-018.md\n    \
             familiar-ai backlog release docs/prds/PRD-018.md --actor human:<you> --reason \"<why>\"\n    \
             familiar-ai backlog complete docs/prds/PRD-018.md --actor human:<you> --reason \"<why>\"\n",
            started = session.started_at,
            ended = session.ended_at.unwrap(),
        );
        assert_eq!(render(&db, None).unwrap(), expected);
    }

    #[test]
    fn interrupted_session_is_labelled_not_blank() {
        let db = database();
        let repository = seed(&db, "drive-2", "{}");
        repository
            .record_attempt_started("drive-2", "PRD-17", "docs/prds/PRD-017.md", Some("exec-9"))
            .unwrap();
        // No finish_session, no attempt outcome: the process died here.
        let report = render(&db, Some("drive-2")).unwrap();
        assert!(report.contains("ended:       — (interrupted)"));
        assert!(report.contains("termination: interrupted (no termination recorded)"));
        assert!(report.contains("reason=interrupted (attempt did not finish)"));
        assert!(report.contains("BUILT (0)"));
        assert!(report.contains("(nothing completed)"));
    }

    #[test]
    fn unknown_cost_is_counted_separately_and_never_summed_as_zero() {
        let db = database();
        let repository = seed(&db, "drive-3", "{}");
        for (index, cost) in [Some(1_000_u64), None, None].into_iter().enumerate() {
            let sequence = repository
                .record_attempt_started(
                    "drive-3",
                    &format!("PRD-{index}"),
                    &format!("docs/prds/PRD-{index}.md"),
                    Some(&format!("exec-{index}")),
                )
                .unwrap();
            repository
                .record_attempt_finished("drive-3", sequence, "completed", None, cost, Some(10))
                .unwrap();
        }
        repository
            .finish_session("drive-3", "backlog_empty")
            .unwrap();
        let report = render(&db, None).unwrap();
        assert!(report.contains("known:   1000 micro-USD across 1 attempt(s)"));
        assert!(report.contains("unknown: 2 attempt(s) with no measurable cost"));
    }

    #[test]
    fn safety_and_delivery_terminations_are_called_out() {
        for (reason, marker) in [
            ("cost_unknown", "cost could not be measured"),
            ("storage_failure", "storage failure"),
            ("delivery_blocked", ".delivery.json"),
            ("budget_deliveries_exhausted", "finite delivery warrant"),
        ] {
            let db = database();
            let repository = seed(&db, "drive-x", "{}");
            repository.finish_session("drive-x", reason).unwrap();
            let report = render(&db, None).unwrap();
            assert!(report.contains(marker), "missing call-out for {reason}");
        }
    }

    #[test]
    fn rendering_is_deterministic_and_mutates_nothing() {
        let db = database();
        let repository = seed(&db, "drive-4", "{}");
        let sequence = repository
            .record_attempt_started("drive-4", "PRD-17", "docs/prds/PRD-017.md", Some("exec-1"))
            .unwrap();
        repository
            .record_attempt_finished(
                "drive-4",
                sequence,
                "retained",
                Some("scope_broadened"),
                None,
                Some(5),
            )
            .unwrap();
        repository
            .finish_session("drive-4", "backlog_empty")
            .unwrap();

        let before = repository.attempts("drive-4").unwrap();
        let first = render(&db, None).unwrap();
        let second = render(&db, None).unwrap();
        assert_eq!(first, second);
        assert_eq!(repository.attempts("drive-4").unwrap(), before);
        assert_eq!(
            repository
                .get_session("drive-4")
                .unwrap()
                .unwrap()
                .termination_reason,
            Some("backlog_empty".into())
        );
    }

    #[test]
    fn explicit_session_id_selects_that_session_over_the_latest() {
        let db = database();
        let older = seed(&db, "drive-old", "{}");
        older.finish_session("drive-old", "backlog_empty").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let newer = seed(&db, "drive-new", "{}");
        newer
            .finish_session("drive-new", "nothing_eligible")
            .unwrap();

        assert!(render(&db, None)
            .unwrap()
            .contains("session:     drive-new"));
        assert!(render(&db, Some("drive-old"))
            .unwrap()
            .contains("session:     drive-old"));
    }

    #[test]
    fn missing_review_cycle_is_normal_and_renders_without_scope_detail() {
        // Review disabled is the common path: no cycle exists for the
        // execution, and the report must degrade rather than fail.
        let db = database();
        let repository = seed(&db, "drive-5", "{}");
        let sequence = repository
            .record_attempt_started(
                "drive-5",
                "PRD-17",
                "docs/prds/PRD-017.md",
                Some("exec-none"),
            )
            .unwrap();
        repository
            .record_attempt_finished(
                "drive-5",
                sequence,
                "retained",
                Some("review_disabled"),
                None,
                Some(1),
            )
            .unwrap();
        repository
            .finish_session("drive-5", "nothing_eligible")
            .unwrap();
        let report = render(&db, None).unwrap();
        assert!(report.contains("reason=review_disabled"));
        assert!(!report.contains("scope:"));
    }
}
