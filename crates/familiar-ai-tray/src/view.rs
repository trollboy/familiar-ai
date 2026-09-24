//! Pure view models for the tray's windows.
//!
//! Follows the same split as [`crate::menu`]: the shape of what the user sees
//! is computed here, as data, testable without a display; the GTK code in
//! [`crate::windows`] only lays it out. The grouping and formatting rules
//! below were previously expressed once in the dashboard's JavaScript and
//! nowhere else, which made them untestable.

use serde_json::Value;

pub(crate) fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn items(v: &Value) -> &[Value] {
    v.get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Microdollars to a displayable amount. Cost is reported in microdollars
/// everywhere in the ledger and is meaningless to a reader in that unit.
pub fn format_usd(micro: Option<i64>) -> String {
    match micro {
        Some(m) => format!("${:.2}", m as f64 / 1_000_000.0),
        None => "—".to_string(),
    }
}

pub fn format_duration(ms: Option<i64>) -> String {
    match ms {
        Some(ms) if ms > 0 => {
            let total_min = ms / 60_000;
            if total_min >= 60 {
                format!("{}h {}m", total_min / 60, total_min % 60)
            } else {
                format!("{total_min}m")
            }
        }
        _ => "—".to_string(),
    }
}

/// Escapes text for a GTK markup label. Every string the windows display is
/// data — repository paths, agent-written reasons, review details — and one
/// stray `&` in any of them makes GTK reject the whole label and render
/// nothing. Kept here, next to the strings it protects, so it can be tested
/// without a display.
pub fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------- settings

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum BackendState {
    Healthy,
    Degraded,
    NotLoaded,
}

impl BackendState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::NotLoaded => "not loaded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BackendRow {
    /// Identifier the test action sends back, e.g. `text_primary`.
    pub target: String,
    pub caption: String,
    pub state: BackendState,
    pub backend_name: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SettingsView {
    pub text_mode: String,
    pub text: Vec<BackendRow>,
    pub embedding: Vec<BackendRow>,
}

fn backend_row(target: &str, caption: &str, v: Option<&Value>) -> Option<BackendRow> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    let loaded = v.get("loaded").and_then(Value::as_bool).unwrap_or(false);
    let healthy = v.get("healthy").and_then(Value::as_bool).unwrap_or(false);
    let state = match (loaded, healthy) {
        (true, true) => BackendState::Healthy,
        (true, false) => BackendState::Degraded,
        (false, _) => BackendState::NotLoaded,
    };
    let last_error = v
        .get("last_error")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(BackendRow {
        target: target.to_string(),
        caption: caption.to_string(),
        state,
        backend_name: str_at(v, "backend_name").to_string(),
        last_error,
    })
}

pub fn build_settings_view(status: &Value) -> SettingsView {
    let mut text = Vec::new();
    if let Some(row) = backend_row("text_primary", "Primary", status.get("text_primary")) {
        text.push(row);
    }
    if let Some(row) = backend_row("text_fallback", "Fallback", status.get("text_fallback")) {
        text.push(row);
    }
    let mut embedding = Vec::new();
    if let Some(row) = backend_row("embed_primary", "Primary", status.get("embedding_primary")) {
        embedding.push(row);
    }
    if let Some(row) = backend_row(
        "embed_fallback",
        "Fallback",
        status.get("embedding_fallback"),
    ) {
        embedding.push(row);
    }
    SettingsView {
        text_mode: str_at(status, "text_mode").to_string(),
        text,
        embedding,
    }
}

// ------------------------------------------------------------------- gates

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GateGroup {
    pub prd_id: String,
    pub prd_path: String,
    /// Every distinct reason this PRD stopped, in first-seen order.
    pub reasons: Vec<String>,
    pub recovery_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GatesView {
    pub groups: Vec<GateGroup>,
    /// Stopped attempts before grouping; one PRD can stop many times.
    pub stopped_attempts: usize,
}

/// What the tray should do about the current gate set, given what it has
/// already announced.
///
/// Pure, because "announce once" is the property that decides whether the
/// surface stays worth reading, and it should be provable without a desktop
/// session. The count is what the badge draws; `to_announce` is what the
/// notifier is handed; `seen` is what the caller remembers for next time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Escalation {
    pub count: usize,
    pub to_announce: Vec<GateGroup>,
    pub seen: std::collections::BTreeSet<String>,
}

/// A gate's identity for announcement purposes: the PRD plus the distinct
/// reasons it stopped. A PRD that stops again for a *new* reason is new
/// information and is announced; the same PRD stopped the same way is not.
pub fn announcement_key(group: &GateGroup) -> String {
    format!("{}|{}", group.prd_id, group.reasons.join(","))
}

/// Decide what to say. `already_seen` is the set of keys announced before.
pub fn escalation(
    view: &GatesView,
    already_seen: &std::collections::BTreeSet<String>,
) -> Escalation {
    let mut seen = std::collections::BTreeSet::new();
    let mut to_announce = Vec::new();
    for group in &view.groups {
        let key = announcement_key(group);
        if !already_seen.contains(&key) {
            to_announce.push(group.clone());
        }
        seen.insert(key);
    }
    // Keys are rebuilt from the live set rather than accumulated, so a gate
    // that is decided and later recurs announces again instead of being
    // permanently suppressed by a memory of the old stop.
    Escalation {
        count: view.groups.len(),
        to_announce,
        seen,
    }
}

/// The one-line summary a notification carries.
pub fn escalation_message(group: &GateGroup) -> (String, String) {
    let title = format!("{} needs a decision", group.prd_id);
    let body = if group.reasons.is_empty() {
        group.prd_path.clone()
    } else {
        group.reasons.join("; ")
    };
    (title, body)
}

/// Groups stopped attempts by PRD. A PRD that stopped four different ways is
/// one thing to decide about, not four, and the ungrouped list ran to several
/// screens for a backlog this size.
pub fn build_gates_view(gates: &Value) -> GatesView {
    let rows = items(gates);
    let mut groups: Vec<GateGroup> = Vec::new();
    for row in rows {
        let path = str_at(row, "prd_path").to_string();
        let key = if path.is_empty() {
            str_at(row, "prd_id").to_string()
        } else {
            path.clone()
        };
        let reason = {
            let detail = str_at(row, "detail");
            if detail.is_empty() {
                str_at(row, "kind")
            } else {
                detail
            }
        }
        .to_string();
        let commands: Vec<String> = row
            .get("recovery_commands")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        match groups.iter_mut().find(|g| {
            if path.is_empty() {
                g.prd_id == key
            } else {
                g.prd_path == key
            }
        }) {
            Some(group) => {
                if !reason.is_empty() && !group.reasons.contains(&reason) {
                    group.reasons.push(reason);
                }
                for c in commands {
                    if !group.recovery_commands.contains(&c) {
                        group.recovery_commands.push(c);
                    }
                }
            }
            None => groups.push(GateGroup {
                prd_id: str_at(row, "prd_id").to_string(),
                prd_path: path,
                reasons: if reason.is_empty() {
                    vec![]
                } else {
                    vec![reason]
                },
                recovery_commands: commands,
            }),
        }
    }
    GatesView {
        stopped_attempts: rows.len(),
        groups,
    }
}

// ----------------------------------------------------------------- backlog

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BacklogView {
    /// Status counts in descending count order, so the shape of the backlog
    /// reads at a glance.
    pub counts: Vec<(String, usize)>,
    pub open: Vec<BacklogRow>,
    /// Every row, completed ones included. The waterfall shows finished work
    /// as well as outstanding work — a chart of only what is left says
    /// nothing about what the repository has actually done.
    pub all: Vec<BacklogRow>,
    /// True when the page fetched was full, i.e. these are not all of them.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BacklogRow {
    pub prd_path: String,
    pub status: String,
    /// PRD-109: the single derived lifecycle state (`draft`, `ready`,
    /// `implementing`, `testing`, `reviewed`, `approved`, `completed`,
    /// `blocked`, `failed`, `awaiting_feedback`). Falls back to the raw
    /// ledger status when a daemon predating PRD-109 answers.
    pub lifecycle: String,
    pub lifecycle_divergence: Option<String>,
    pub updated_at: String,
    /// Set when the file behind this entry has disappeared from the working
    /// tree. The row survives so the state is visible, but nothing can be run
    /// from it.
    pub missing_since: Option<String>,
}

pub fn build_backlog_view(backlog: &Value) -> BacklogView {
    let rows = items(backlog);
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut open = Vec::new();
    let mut all = Vec::new();
    for row in rows {
        let status = str_at(row, "status").to_string();
        match counts.iter_mut().find(|(s, _)| *s == status) {
            Some(entry) => entry.1 += 1,
            None => counts.push((status.clone(), 1)),
        }
        let lifecycle = match str_at(row, "lifecycle") {
            "" => status.clone(),
            lifecycle => lifecycle.to_string(),
        };
        let entry = BacklogRow {
            prd_path: str_at(row, "prd_path").to_string(),
            status: status.clone(),
            lifecycle,
            lifecycle_divergence: row
                .get("lifecycle_divergence")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            updated_at: str_at(row, "updated_at").to_string(),
            missing_since: row
                .get("missing_since")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        };
        if status != "completed" {
            open.push(entry.clone());
        }
        all.push(entry);
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    BacklogView {
        counts,
        open,
        all,
        truncated: backlog
            .get("next_cursor")
            .map(|c| !c.is_null())
            .unwrap_or(false),
    }
}

/// What the backlog looks like as work rather than as rows.
///
/// A count of "pending" invites you to add another pending item. The number
/// that matters is how much can actually be started right now, which on a
/// dependency graph with unfinished roots is a much smaller number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BacklogSummary {
    /// Open, unblocked, and its file still exists.
    pub startable: usize,
    /// Waiting on another PRD that is not finished.
    pub blocked: usize,
    /// Claimed, with work retained against it.
    pub in_progress: usize,
    /// Rows whose file has gone; nothing can be run from them.
    pub missing: usize,
}

impl BacklogSummary {
    /// The headline, ordered so the actionable number is read first.
    pub fn headline(&self) -> String {
        let mut parts = vec![format!("<b>{} startable</b>", self.startable)];
        if self.blocked > 0 {
            parts.push(format!("{} blocked", self.blocked));
        }
        if self.in_progress > 0 {
            parts.push(format!("{} in progress", self.in_progress));
        }
        if self.missing > 0 {
            parts.push(format!("{} missing", self.missing));
        }
        parts.join("  ·  ")
    }
}

pub fn build_backlog_summary(
    view: &BacklogView,
    blockers: &[(String, Vec<Blocker>)],
) -> BacklogSummary {
    let mut summary = BacklogSummary {
        startable: 0,
        blocked: 0,
        in_progress: 0,
        missing: 0,
    };
    for row in &view.open {
        if row.missing_since.is_some() {
            summary.missing += 1;
            continue;
        }
        if row.status == "in_progress" {
            summary.in_progress += 1;
            continue;
        }
        let waiting = blockers
            .iter()
            .any(|(path, list)| *path == row.prd_path && !list.is_empty());
        if waiting {
            summary.blocked += 1;
        } else {
            summary.startable += 1;
        }
    }
    summary
}

// ---------------------------------------------------------------- sessions

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionRow {
    pub session_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
}

impl SessionRow {
    pub fn is_running(&self) -> bool {
        self.ended_at.is_none()
    }
}

pub fn build_sessions_view(sessions: &Value) -> Vec<SessionRow> {
    items(sessions)
        .iter()
        .map(|row| SessionRow {
            session_id: str_at(row, "session_id").to_string(),
            started_at: str_at(row, "started_at").to_string(),
            ended_at: row
                .get("ended_at")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AttemptRow {
    pub sequence: i64,
    pub prd_id: String,
    pub model: String,
    pub outcome: String,
    pub retained_reason: Option<String>,
    pub review_disposition: Option<String>,
    pub blocking_findings: usize,
    pub cost: String,
    pub duration: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FindingRow {
    pub prd_id: String,
    pub path: String,
    pub rule: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionDetailView {
    pub spent: String,
    pub priced_attempts: i64,
    pub unpriced_attempts: i64,
    pub warrant: String,
    pub attempts: Vec<AttemptRow>,
    pub findings: Vec<FindingRow>,
}

/// A lockfile finding can enumerate every new crate in the dependency graph.
/// Left whole it is hundreds of names in one cell.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let kept: String = text.chars().take(max).collect();
        format!("{kept}…")
    } else {
        text.to_string()
    }
}

pub fn build_session_detail(budget: &Value, attempts: &Value, review: &Value) -> SessionDetailView {
    let warrant = budget.get("warrant").cloned().unwrap_or(Value::Null);
    let max_prds = warrant.get("max_prds").and_then(Value::as_i64);
    let max_duration = warrant.get("max_duration_ms").and_then(Value::as_i64);
    let warrant_text = match (max_prds, max_duration) {
        (Some(p), Some(d)) => format!("{} PRDs / {}", p, format_duration(Some(d))),
        (Some(p), None) => format!("{p} PRDs"),
        (None, Some(d)) => format_duration(Some(d)),
        (None, None) => "unbounded".to_string(),
    };

    let reviews = items(review);
    let mut findings = Vec::new();
    for r in reviews {
        let prd_id = str_at(r, "prd_id").to_string();
        for f in r
            .get("blocking_findings")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let rule = {
                let id = str_at(f, "rule_id");
                if id.is_empty() {
                    str_at(f, "decision")
                } else {
                    id
                }
            };
            findings.push(FindingRow {
                prd_id: prd_id.clone(),
                path: str_at(f, "path").to_string(),
                rule: rule.to_string(),
                detail: clip(str_at(f, "rule_detail"), 200),
            });
        }
    }

    let attempts = items(attempts)
        .iter()
        .map(|a| {
            let prd_id = str_at(a, "prd_id").to_string();
            let review_for_prd = reviews.iter().find(|r| str_at(r, "prd_id") == prd_id);
            AttemptRow {
                sequence: a.get("sequence").and_then(Value::as_i64).unwrap_or(0),
                model: str_at(a, "model").to_string(),
                outcome: str_at(a, "outcome").to_string(),
                retained_reason: a
                    .get("retained_reason")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                review_disposition: review_for_prd
                    .map(|r| str_at(r, "disposition").to_string())
                    .filter(|s| !s.is_empty()),
                blocking_findings: review_for_prd
                    .and_then(|r| r.get("blocking_findings"))
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
                cost: format_usd(a.get("known_cost_microusd").and_then(Value::as_i64)),
                duration: format_duration(a.get("duration_ms").and_then(Value::as_i64)),
                prd_id,
            }
        })
        .collect();

    SessionDetailView {
        spent: format_usd(budget.get("known_cost_microusd").and_then(Value::as_i64)),
        priced_attempts: budget
            .get("known_cost_attempts")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        unpriced_attempts: budget
            .get("unknown_cost_attempts")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        warrant: warrant_text,
        attempts,
        findings,
    }
}

/// The markup for one gate row: which PRD, why it stopped, where it lives.
pub fn gate_label_markup(group: &GateGroup) -> String {
    format!(
        "<b>{}</b>  <span size=\"small\">{}</span>\n<tt><small>{}</small></tt>",
        escape_markup(&group.prd_id),
        escape_markup(&group.reasons.join(", ")),
        escape_markup(&group.prd_path)
    )
}

/// The markup for one session row. A session with no end is still running,
/// and saying so beats an empty cell.
pub fn session_label_markup(row: &SessionRow) -> String {
    let ended = match &row.ended_at {
        Some(ended) => ended.clone(),
        None => "running".to_string(),
    };
    format!(
        "<tt>{}</tt>  <small>{} → {}</small>",
        escape_markup(&row.session_id),
        escape_markup(&row.started_at),
        escape_markup(&ended)
    )
}

/// Headline for a session's spend against the warrant it ran under.
pub fn budget_markup(view: &SessionDetailView) -> String {
    let unpriced = if view.unpriced_attempts > 0 {
        format!(", {} unpriced", view.unpriced_attempts)
    } else {
        String::new()
    };
    format!(
        "<b>Spent {}</b>  <small>{} priced attempt{}{} · warrant {}</small>",
        escape_markup(&view.spent),
        view.priced_attempts,
        if view.priced_attempts == 1 { "" } else { "s" },
        escape_markup(&unpriced),
        escape_markup(&view.warrant)
    )
}

/// The outcome cell: the outcome, and underneath it why the work was retained.
pub fn attempt_outcome_markup(attempt: &AttemptRow) -> String {
    match &attempt.retained_reason {
        Some(reason) => format!(
            "{}\n<small>{}</small>",
            escape_markup(&attempt.outcome),
            escape_markup(reason)
        ),
        None => escape_markup(&attempt.outcome),
    }
}

/// The review cell: the disposition, and how much is blocking underneath it.
pub fn attempt_review_markup(attempt: &AttemptRow) -> String {
    match &attempt.review_disposition {
        Some(d) if attempt.blocking_findings > 0 => format!(
            "{}\n<small>{} blocking</small>",
            escape_markup(d),
            attempt.blocking_findings
        ),
        Some(d) => escape_markup(d),
        None => "—".to_string(),
    }
}

// -------------------------------------------------------------- executions

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExecutionRowView {
    pub execution_id: String,
    pub state: String,
    pub mode: String,
    /// The command as a reader can judge it, not as JSON.
    pub command: String,
    pub created_at: String,
}

impl ExecutionRowView {
    /// Whether stopping it would mean anything. Terminal states are listed so
    /// the operator can see what just happened, but offering Stop on them
    /// would be a button that does nothing.
    pub fn is_stoppable(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "running" | "paused")
    }
}

/// Renders `command_json` for a human. It holds either a `{argv, timeout_ms}`
/// object or a bare argv array — both shapes are accepted by the worker, so
/// both have to be readable here.
pub fn command_summary(command_json: &str) -> String {
    let parsed: Value = match serde_json::from_str(command_json) {
        Ok(v) => v,
        Err(_) => return command_json.to_string(),
    };
    let argv = parsed
        .get("argv")
        .and_then(Value::as_array)
        .or_else(|| parsed.as_array());
    match argv {
        Some(argv) => argv
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        None => command_json.to_string(),
    }
}

pub fn build_executions_view(executions: &Value) -> Vec<ExecutionRowView> {
    items(executions)
        .iter()
        .map(|e| ExecutionRowView {
            execution_id: str_at(e, "execution_id").to_string(),
            state: str_at(e, "state").to_string(),
            mode: str_at(e, "mode").to_string(),
            command: command_summary(str_at(e, "command_json")),
            created_at: str_at(e, "created_at").to_string(),
        })
        .collect()
}

pub fn execution_label_markup(row: &ExecutionRowView) -> String {
    format!(
        "<b>{}</b>  <small>{} · {}</small>\n<tt><small>{}</small></tt>",
        escape_markup(&row.state),
        escape_markup(&row.mode),
        escape_markup(&row.created_at),
        escape_markup(&row.command)
    )
}

/// How the project's control-plane registration reads in the toolbar. A
/// project that was never registered is not the same as one that is active,
/// and saying "active" for it would be a lie.
pub fn project_state_label(state: &Value) -> String {
    match state.get("state").and_then(Value::as_str) {
        Some("paused") => "paused".to_string(),
        Some(other) => other.to_string(),
        None => "not registered".to_string(),
    }
}

// ----------------------------------------------------------------- choices

/// One option a setting may take.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Choice {
    pub value: String,
    /// What the operator reads. Carries the reason an option cannot be used,
    /// because a silently missing option looks like a bug in the form.
    pub label: String,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ChoiceSet {
    /// The value must be one of these.
    Closed(Vec<Choice>),
    /// These are the known values, but anything is allowed — model names are
    /// open-ended and a new one must not be unenterable.
    Open(Vec<Choice>),
}

fn choices_from(list: &Value, key: &str) -> Vec<Choice> {
    list.get(key)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| match entry {
                    Value::String(value) => Choice {
                        value: value.clone(),
                        label: value.clone(),
                        available: true,
                    },
                    object => {
                        let value = str_at(object, "value").to_string();
                        let available = object
                            .get("available")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        let detail = str_at(object, "detail");
                        let label = if available || detail.is_empty() {
                            value.clone()
                        } else {
                            format!("{value} — {detail}")
                        };
                        Choice {
                            value,
                            label,
                            available,
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The options for one setting, or `None` when it is free text.
///
/// Matched on the field's own name rather than its full path, so the same
/// setting gets the same dropdown wherever it appears — global `[agents.x]`
/// and a repository's `review.implementation_agent` name the same things.
pub fn choices_for(path: &[String], catalogue: &Value) -> Option<ChoiceSet> {
    let name = path.last()?.as_str();
    let set = match name {
        "adapter" | "adapter_id" => ChoiceSet::Closed(choices_from(catalogue, "adapters")),
        "permission_mode" => {
            let mut modes = choices_from(catalogue, "permission_modes");
            // The config refuses bypassPermissions for a reviewer, so the form
            // must not offer it: a choice that will be rejected on save is a
            // trap, not a choice.
            if path.iter().any(|segment| segment.contains("review")) {
                modes.retain(|choice| choice.value != "bypassPermissions");
            }
            ChoiceSet::Closed(modes)
        }
        "provider" => ChoiceSet::Closed(choices_from(catalogue, "providers")),
        "mode" => ChoiceSet::Closed(choices_from(catalogue, "inference_modes")),
        "prd_metadata_policy" => {
            ChoiceSet::Closed(choices_from(catalogue, "prd_metadata_policies"))
        }
        // Always a dropdown, even with nothing known yet: live discovery
        // fills it in after the form is on screen, and an editable combo with
        // no items still behaves exactly like a text entry until then.
        "model" => return Some(ChoiceSet::Open(choices_from(catalogue, "models"))),
        _ => return None,
    };
    // An empty catalogue means nothing is known, and an empty dropdown would
    // make a setting uneditable. Fall back to free text.
    match &set {
        ChoiceSet::Closed(list) | ChoiceSet::Open(list) if list.is_empty() => None,
        _ => Some(set),
    }
}

// ------------------------------------------------------------ dependencies

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Blocker {
    pub prd_id: String,
    pub status: String,
}

/// Unmet dependencies per PRD path.
pub fn build_blockers(dependencies: &Value) -> Vec<(String, Vec<Blocker>)> {
    items(dependencies)
        .iter()
        .map(|entry| {
            let blockers = entry
                .get("blocked_by")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .map(|b| Blocker {
                            prd_id: str_at(b, "prd_id").to_string(),
                            status: str_at(b, "status").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            (str_at(entry, "prd_path").to_string(), blockers)
        })
        .filter(|(_, blockers): &(String, Vec<Blocker>)| !blockers.is_empty())
        .collect()
}

/// A dependency DAG arranged as execution waves. Wave zero can be worked now;
/// each later wave becomes eligible only after all of its incoming edges have
/// completed. This is a planning chart, not a claim that durations are known.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DependencyGanttNode {
    pub prd_id: String,
    pub prd_path: String,
    pub status: String,
    /// PRD-109: the derived lifecycle from the backlog row, falling back to
    /// the raw status when the row is missing or the daemon predates it.
    pub lifecycle: String,
    pub lifecycle_divergence: Option<String>,
    pub wave: usize,
    pub depends_on: Vec<String>,
    pub unlocks: Vec<String>,
    /// PRDs whose declared scope overlaps this one's, from the scheduler's
    /// own overlap rule. Two conflicting PRDs never share a wave.
    pub conflicts_with: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct DependencyGantt {
    pub waves: Vec<Vec<DependencyGanttNode>>,
    pub max_wave: usize,
    /// Why scope conflicts are missing, when they are: the waves are then
    /// dependency layers only and must not be read as launchable rounds.
    pub conflicts_error: Option<String>,
}

/// Builds the complete declared dependency graph, including already-completed
/// edges. The daemon validates cycles at discovery; the bounded relaxation is
/// defensive so malformed legacy data cannot hang the UI.
pub fn build_dependency_gantt(dependencies: &Value, backlog: &[BacklogRow]) -> DependencyGantt {
    use std::collections::HashMap;

    let status_by_path: HashMap<&str, &str> = backlog
        .iter()
        .map(|row| (row.prd_path.as_str(), row.status.as_str()))
        .collect();
    let lifecycle_by_path: HashMap<&str, &str> = backlog
        .iter()
        .map(|row| (row.prd_path.as_str(), row.lifecycle.as_str()))
        .collect();
    let divergence_by_path: HashMap<&str, Option<&str>> = backlog
        .iter()
        .map(|row| (row.prd_path.as_str(), row.lifecycle_divergence.as_deref()))
        .collect();
    let entries = items(dependencies);
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    let mut paths: HashMap<String, String> = HashMap::new();
    let mut conflicts: HashMap<String, Vec<String>> = HashMap::new();
    for entry in entries {
        let id = str_at(entry, "prd_id").to_string();
        paths.insert(id.clone(), str_at(entry, "prd_path").to_string());
        conflicts.insert(
            id.clone(),
            entry
                .get("conflicts_with")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        );
        parents.insert(
            id,
            entry
                .get("depends_on")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .map(|parent| str_at(parent, "prd_id").to_string())
                .collect(),
        );
    }
    // FAM-BUG-088: completed work is history, not a wave. A parent that is
    // already complete does not push its child later (PRD-92 sat in Wave 2
    // behind three landed PRDs), and a completed node neither occupies a
    // round nor blocks a conflict slot.
    let completed = |id: &str| -> bool {
        paths
            .get(id)
            .map(|path| {
                lifecycle_by_path
                    .get(path.as_str())
                    .filter(|state| !state.is_empty())
                    .copied()
                    .or_else(|| status_by_path.get(path.as_str()).copied())
                    == Some("completed")
            })
            .unwrap_or(false)
    };
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for (child, dependencies) in &parents {
        for parent in dependencies {
            children
                .entry(parent.clone())
                .or_default()
                .push(child.clone());
        }
    }
    let mut depths: HashMap<String, usize> = parents.keys().map(|id| (id.clone(), 0)).collect();
    for _ in 0..parents.len() {
        let previous = depths.clone();
        let mut changed = false;
        for (id, dependencies) in &parents {
            let depth = dependencies
                .iter()
                .filter(|parent| !completed(parent))
                .filter_map(|parent| previous.get(parent))
                .map(|depth| depth + 1)
                .max()
                .unwrap_or(0);
            if depths.get(id).copied().unwrap_or(0) != depth {
                depths.insert(id.clone(), depth);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Rounds, not layers: a wave is dependency-ready AND scope-disjoint, the
    // owner's definition. A PRD goes in the first round after all of its
    // parents in which nothing already placed conflicts with it, so two PRDs
    // the scheduler would serialize are never drawn side by side.
    let mut order: Vec<String> = parents.keys().cloned().collect();
    // The scheduler admits in PRD-number order (`PrdId: Ord` is numeric);
    // sorting the id string put every PRD-1xx ahead of PRD-86..98 and gave
    // them the free slots first, so the chart disagreed with the batch.
    order.sort_by_key(|id| (depths.get(id).copied().unwrap_or(0), prd_ordinal(id)));
    let mut rounds: HashMap<String, usize> = HashMap::new();
    let mut occupants: Vec<Vec<String>> = Vec::new();
    for id in order {
        if completed(&id) {
            rounds.insert(id, 0);
            continue;
        }
        let mut round = parents
            .get(&id)
            .into_iter()
            .flatten()
            .filter(|parent| !completed(parent))
            .filter_map(|parent| rounds.get(parent))
            .map(|round| round + 1)
            .max()
            .unwrap_or(0);
        let mine = conflicts.get(&id).cloned().unwrap_or_default();
        loop {
            if occupants.len() <= round {
                occupants.resize(round + 1, Vec::new());
            }
            if occupants[round].iter().any(|other| mine.contains(other)) {
                round += 1;
                continue;
            }
            break;
        }
        occupants[round].push(id.clone());
        rounds.insert(id, round);
    }
    let max_wave = rounds.values().copied().max().unwrap_or(0);
    let conflicts_error = dependencies
        .get("conflicts_error")
        .and_then(Value::as_str)
        .filter(|error| !error.is_empty())
        .map(str::to_owned);
    let mut waves = vec![Vec::new(); max_wave + 1];
    for (id, depends_on) in parents {
        let path = paths.remove(&id).unwrap_or_default();
        let wave = rounds.get(&id).copied().unwrap_or(0).min(max_wave);
        let mut unlocks = children.remove(&id).unwrap_or_default();
        unlocks.sort();
        // PRD-108: `path` came from the `dependencies` query, i.e. this
        // PRD was discovered on disk. A missing entry here means the
        // ledger has not caught up yet, not that the file is absent —
        // "not found" claimed the latter and was wrong.
        let status = status_by_path
            .get(path.as_str())
            .copied()
            .unwrap_or("unenrolled")
            .to_string();
        let lifecycle = lifecycle_by_path
            .get(path.as_str())
            .filter(|state| !state.is_empty())
            .map(|state| state.to_string())
            .unwrap_or_else(|| status.clone());
        let conflicts_with = conflicts.remove(&id).unwrap_or_default();
        waves[wave].push(DependencyGanttNode {
            prd_id: id,
            status,
            lifecycle,
            lifecycle_divergence: divergence_by_path
                .get(path.as_str())
                .and_then(|value| *value)
                .map(str::to_string),
            prd_path: path,
            wave,
            depends_on,
            unlocks,
            conflicts_with,
        });
    }
    for wave in &mut waves {
        wave.sort_by_key(|node| prd_ordinal(&node.prd_id));
    }
    DependencyGantt {
        waves,
        max_wave,
        conflicts_error,
    }
}

/// The scheduler's order for a PRD id: its number, then the id itself for
/// suffixes and anything unparseable.
fn prd_ordinal(id: &str) -> (u64, String) {
    let digits: String = id
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (digits.parse().unwrap_or(u64::MAX), id.to_string())
}

/// Why Start is not offered for a row, or `None` when it is.
///
/// Both refusals mirror something the runner would do anyway: it refuses a PRD
/// whose dependencies are incomplete, and it cannot read a file that is not
/// there. Presenting the refusal here turns a rejection into an explanation.
pub fn start_refusal(row: &BacklogRow, waiting_on: &[Blocker]) -> Option<String> {
    if let Some(since) = &row.missing_since {
        return Some(format!(
            "file has been missing since {since} — nothing can be run from this entry"
        ));
    }
    if waiting_on.is_empty() {
        return None;
    }
    Some(format!(
        "waiting on {}",
        waiting_on
            .iter()
            .map(|b| format!("{} ({})", b.prd_id, b.status))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The refusal, rendered under the row it belongs to.
pub fn refusal_markup(reason: &str) -> String {
    format!("<small>{}</small>", escape_markup(reason))
}

// ------------------------------------------------------- why it is blocked

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BlockingFinding {
    pub path: String,
    pub decision: String,
    pub rule_detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BlockedReason {
    /// One line naming the cause, e.g. "scope broadened".
    pub headline: String,
    pub findings: Vec<BlockingFinding>,
    pub review_findings: usize,
    /// Findings still awaiting a verdict. Zero means the numbered picker has
    /// nothing to ask, whatever the generic recovery advice says.
    pub pending_decisions: usize,
    /// The branch the retained work sits on, when there is one.
    pub branch: Option<String>,
}

/// Why each blocked PRD stopped, keyed by PRD path.
///
/// The headline follows the policy's own precedence: a prohibited or
/// undeclared change means the scope broadened; otherwise an ambiguous change
/// means a human has to adjudicate it. A stop with no scope findings at all is
/// reported as such rather than guessed at.
pub fn build_blocked_reasons(reasons: &Value) -> Vec<(String, BlockedReason)> {
    items(reasons)
        .iter()
        .map(|entry| {
            let findings: Vec<BlockingFinding> = entry
                .get("blocking")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .map(|f| BlockingFinding {
                            path: str_at(f, "path").to_string(),
                            decision: str_at(f, "decision").to_string(),
                            rule_detail: str_at(f, "rule_detail").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let review_findings = entry
                .get("review_findings")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;

            // One file produces several findings (one per change id), so a
            // count of findings overstates how much there is to look at. The
            // operator cares how many FILES are in the way.
            let distinct_paths = |decisions: &[&str]| -> usize {
                let mut paths: Vec<&str> = findings
                    .iter()
                    .filter(|f| decisions.contains(&f.decision.as_str()))
                    .map(|f| f.path.as_str())
                    .collect();
                paths.sort_unstable();
                paths.dedup();
                paths.len()
            };
            let broadening = ["undeclared_scope_expansion", "prohibited_change"];
            let broadened = distinct_paths(&broadening);
            let ambiguous = distinct_paths(&["ambiguous_human_review"]);
            let headline = if broadened > 0 {
                format!(
                    "scope broadened — {broadened} file{} outside the declared scope",
                    if broadened == 1 { "" } else { "s" }
                )
            } else if ambiguous > 0 {
                format!(
                    "needs your decision — {ambiguous} file{} a rule cannot classify",
                    if ambiguous == 1 { "" } else { "s" }
                )
            } else {
                let reason = entry
                    .get("invalid_reason")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty());
                let phase = str_at(entry, "phase");
                match reason {
                    Some(reason) => reason.to_string(),
                    None if review_findings > 0 => {
                        format!("{review_findings} review finding(s)")
                    }
                    // A candidate parked mid-pipeline has not "failed" — it
                    // stopped somewhere, and where it stopped is the useful
                    // fact.
                    None if !phase.is_empty() && phase != "blocked" => {
                        format!("stopped after {phase}")
                    }
                    None => "stopped without a recorded scope finding".to_string(),
                }
            };

            (
                str_at(entry, "prd_path").to_string(),
                BlockedReason {
                    headline,
                    findings,
                    review_findings,
                    pending_decisions: entry
                        .get("pending_decisions")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize,
                    branch: entry
                        .get("branch")
                        .and_then(Value::as_str)
                        .filter(|b| !b.is_empty())
                        .map(str::to_string),
                },
            )
        })
        .collect()
}

/// What the operator is actually being asked to decide.
///
/// The recovery commands the backlog emits are a template keyed off the stop
/// reason: a `scope_*` stop always advises running the numbered picker, even
/// when nothing is pending a verdict. This says what is really outstanding.
pub fn outstanding_markup(reason: &BlockedReason) -> String {
    let mut parts = Vec::new();
    if reason.pending_decisions > 0 {
        parts.push(format!(
            "{} finding{} awaiting your verdict",
            reason.pending_decisions,
            if reason.pending_decisions == 1 {
                ""
            } else {
                "s"
            }
        ));
    } else if !reason.findings.is_empty() {
        parts.push("every finding has already been decided".to_string());
    }
    match &reason.branch {
        Some(branch) => parts.push(format!("work retained on {branch}")),
        None => parts.push("no retained work".to_string()),
    }
    format!("<small>{}</small>", escape_markup(&parts.join(" · ")))
}

/// The offending paths, one line each.
///
/// The headline is the expander's own label, so it is not repeated here, and
/// the list is deduplicated: a single file routinely yields several findings
/// and printing it once per finding reads as several separate problems.
pub fn blocked_detail_markup(reason: &BlockedReason) -> String {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for finding in &reason.findings {
        let key = (finding.path.as_str(), finding.decision.as_str());
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    let mut out = String::new();
    for (path, decision) in seen.iter().take(12) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "<tt><small>{}</small></tt>  <small>{}</small>",
            escape_markup(path),
            escape_markup(decision)
        ));
    }
    if seen.len() > 12 {
        out.push_str(&format!("\n<small>… and {} more</small>", seen.len() - 12));
    }
    if out.is_empty() {
        out.push_str("<small>no individual findings were recorded</small>");
    }
    out
}

// ---------------------------------------------------------------- pipeline

/// The phases a PRD passes through, in the order `run.rs` records them.
///
/// Taken from the transition sequence in the runner rather than invented:
/// verification happens *before* independent review, and there is no
/// pull-request stage — delivery is a separate decision.
pub const PIPELINE: [(&str, &str); 7] = [
    ("preflight", "Preflight"),
    ("implemented", "Implementing"),
    ("verified", "Verifying"),
    ("reviewed", "Review"),
    ("approved", "Approved"),
    ("integrated", "Integrated"),
    ("completed", "Done"),
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Progress {
    /// Index into [`PIPELINE`] of the furthest phase reached.
    pub reached: Option<usize>,
    /// Stopped and waiting for a human.
    pub blocked: bool,
    pub detail: String,
}

pub fn pipeline_index(phase: &str) -> Option<usize> {
    PIPELINE.iter().position(|(id, _)| *id == phase)
}

/// Latest phase per PRD path, from the execution checkpoints.
pub fn build_progress(checkpoints: &Value) -> Vec<(String, Progress)> {
    items(checkpoints)
        .iter()
        .map(|c| {
            let phase = str_at(c, "phase");
            let blocked = matches!(phase, "blocked" | "invalid_checkpoint");
            (
                str_at(c, "prd_path").to_string(),
                Progress {
                    reached: pipeline_index(phase),
                    blocked,
                    detail: if blocked {
                        let reason = str_at(c, "invalid_reason");
                        if reason.is_empty() {
                            "blocked".to_string()
                        } else {
                            format!("blocked: {reason}")
                        }
                    } else {
                        phase.to_string()
                    },
                },
            )
        })
        .collect()
}

/// The progress meter for one PRD: every stage, with the ones behind it done
/// and the one it is on marked.
pub fn progress_markup(progress: &Progress) -> String {
    let stages: Vec<String> = PIPELINE
        .iter()
        .enumerate()
        .map(|(index, (_, label))| match progress.reached {
            Some(reached) if index < reached => format!("<b>{label}</b>"),
            Some(reached) if index == reached => format!("<b>[{label}]</b>"),
            _ => format!("<small>{label}</small>"),
        })
        .collect();
    let bar = stages.join(" › ");
    if progress.blocked {
        format!("{bar}   <b>⨯ {}</b>", escape_markup(&progress.detail))
    } else {
        bar
    }
}

// ------------------------------------------------------------------ config

/// What kind of editor a setting needs. Derived from the value already in the
/// file rather than from a hard-coded schema, so a section nobody anticipated
/// still gets a usable widget.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum FieldKind {
    Bool,
    Integer,
    Float,
    Text,
    /// A list of scalars, edited as comma-separated text.
    List,
}

/// Where a project's value came from. A setting shown on a project must say
/// whether it is that project's own or the global default it inherits, or the
/// operator cannot tell what changing it would affect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum FieldOrigin {
    /// Written in the global tables, or shown on the global form.
    Global,
    /// Overridden for this repository.
    Overridden,
    /// Not set for this repository; the global value applies.
    Inherited,
    /// Not set for this repository and nothing global to inherit; the value
    /// shown is the profile's default, and saving it creates the override.
    Default,
}

/// Settings that exist only per repository: a PRD location and its grammar.
/// They have no global counterpart, so a project page that showed only what
/// the file already set hid them from every repository on profile defaults.
pub use familiar_ai_core::backlog::{PROJECT_OVERRIDABLE_TABLES, REPOSITORY_ONLY_KEYS};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfigField {
    /// Path through the document; numeric segments index arrays.
    pub path: Vec<String>,
    pub name: String,
    pub kind: FieldKind,
    pub value: String,
    pub origin: FieldOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConfigSection {
    /// Dotted path, e.g. `agents.implementation`. Empty for the top level.
    pub title: String,
    pub fields: Vec<ConfigField>,
}

fn scalar_field(path: Vec<String>, name: &str, value: &Value) -> Option<ConfigField> {
    let (kind, text) = match value {
        Value::Bool(b) => (FieldKind::Bool, b.to_string()),
        Value::Number(n) if n.is_i64() || n.is_u64() => (FieldKind::Integer, n.to_string()),
        Value::Number(n) => (FieldKind::Float, n.to_string()),
        Value::String(s) => (FieldKind::Text, s.clone()),
        Value::Array(items) if items.iter().all(|i| !i.is_object() && !i.is_array()) => (
            FieldKind::List,
            items
                .iter()
                .map(|i| match i {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        // Null has no type to preserve, and nested containers become their own
        // sections rather than fields.
        _ => return None,
    };
    Some(ConfigField {
        path,
        name: name.to_string(),
        kind,
        value: text,
        origin: FieldOrigin::Global,
    })
}

/// Every scalar in a document, with its full path. Arrays of tables are
/// indexed, which is the shape `[[a.b.checks]]` produces.
fn flatten_fields(value: &Value, prefix: &[String]) -> Vec<ConfigField> {
    let mut out = Vec::new();
    let Some(map) = value.as_object() else {
        return out;
    };
    for (key, child) in map {
        let mut path = prefix.to_vec();
        path.push(key.clone());
        match child {
            Value::Object(_) => out.extend(flatten_fields(child, &path)),
            Value::Array(items) if items.iter().any(|i| i.is_object()) => {
                for (index, item) in items.iter().enumerate() {
                    let mut item_path = path.clone();
                    item_path.push(index.to_string());
                    out.extend(flatten_fields(item, &item_path));
                }
            }
            scalar => {
                if let Some(field) = scalar_field(path, key, scalar) {
                    out.push(field);
                }
            }
        }
    }
    out
}

/// Groups fields into sections by their parent path, ordered so the form does
/// not reshuffle between openings.
fn into_sections(fields: Vec<ConfigField>, strip: usize) -> Vec<ConfigSection> {
    let mut sections: Vec<ConfigSection> = Vec::new();
    for field in fields {
        let title = field
            .path
            .get(strip..field.path.len().saturating_sub(1))
            .unwrap_or(&[])
            .join(".");
        match sections.iter_mut().find(|s| s.title == title) {
            Some(section) => section.fields.push(field),
            None => sections.push(ConfigSection {
                title,
                fields: vec![field],
            }),
        }
    }
    for section in &mut sections {
        section.fields.sort_by(|a, b| a.name.cmp(&b.name));
    }
    sections.sort_by(|a, b| {
        (a.title.is_empty().cmp(&b.title.is_empty()).reverse()).then(a.title.cmp(&b.title))
    });
    sections
}

/// Fields an adapter does not accept, keyed by the adapter it is NOT.
///
/// `effort` and `permission_mode` are valid only for `claude-code`; the config
/// rejects them outright for `codex` and `ollama`. Offering them there is a
/// field whose every value is invalid, so the form drops them instead.
const CLAUDE_CODE_ONLY: [&str; 2] = ["effort", "permission_mode"];

/// Removes fields the section's adapter does not accept.
///
/// Scoped per section because that is how the config scopes the rule: each
/// `[agents.<role>]` table is validated against its own `adapter`.
fn apply_adapter_rules(sections: &mut Vec<ConfigSection>) {
    for section in sections.iter_mut() {
        let adapter = section
            .fields
            .iter()
            .find(|f| f.name == "adapter" || f.name == "adapter_id")
            .map(|f| f.value.clone());
        let Some(adapter) = adapter else {
            continue;
        };
        if adapter == "claude-code" {
            continue;
        }
        section
            .fields
            .retain(|f| !CLAUDE_CODE_ONLY.contains(&f.name.as_str()));
    }
    sections.retain(|s| !s.fields.is_empty());
}

/// The global settings form: everything except the per-repository tables.
///
/// `repositories` is excluded deliberately. Those values belong to one project
/// and are edited on that project's own page; listing them here produced
/// sections named after a filesystem path and left it ambiguous whether a
/// change was global or not.
pub fn build_config_form(document: &Value) -> Vec<ConfigSection> {
    let fields = flatten_fields(document, &[])
        .into_iter()
        .filter(|f| f.path.first().map(String::as_str) != Some("repositories"))
        .collect();
    let mut sections = into_sections(fields, 0);
    apply_adapter_rules(&mut sections);
    sections
}

/// One project's settings: its own overrides, plus the global values it
/// inherits, each marked with where it came from.
///
/// The union matters in both directions. A repository can override something
/// the global tables also set (`review`, `execution_context`), and it can set
/// things that only exist per repository (`active_dir`, `profile`), which have
/// no global counterpart to inherit from.
pub fn build_project_config_form(
    document: &Value,
    repo: &str,
    defaults: Option<&Value>,
) -> Vec<ConfigSection> {
    // Only the tables a repository entry can shadow are inheritable. Listing
    // `dashboard` or `logging` here as Inherited implied an override the
    // loader refuses (`RepositoryConfig` denies unknown fields).
    let global: Vec<ConfigField> = flatten_fields(document, &[])
        .into_iter()
        .filter(|f| {
            f.path
                .first()
                .is_some_and(|table| PROJECT_OVERRIDABLE_TABLES.contains(&table.as_str()))
        })
        .collect();
    let overrides = document
        .get("repositories")
        .and_then(|r| r.get(repo))
        .cloned()
        .unwrap_or(Value::Null);
    let overridden = flatten_fields(&overrides, &[]);

    let prefix = vec!["repositories".to_string(), repo.to_string()];
    let mut fields: Vec<ConfigField> = Vec::new();

    for mut field in overridden.clone() {
        field.origin = FieldOrigin::Overridden;
        let mut path = prefix.clone();
        path.extend(field.path.clone());
        field.path = path;
        fields.push(field);
    }
    for mut field in global {
        let already = overridden.iter().any(|o| o.path == field.path);
        if already {
            continue;
        }
        field.origin = FieldOrigin::Inherited;
        let mut path = prefix.clone();
        path.extend(field.path.clone());
        field.path = path;
        fields.push(field);
    }
    // The repository-only settings, always, with the effective default when
    // the file does not set them.
    for key in REPOSITORY_ONLY_KEYS {
        if overridden.iter().any(|o| o.path == [key.to_string()]) {
            continue;
        }
        let value = defaults
            .and_then(|d| d.get(key))
            .cloned()
            .unwrap_or_else(|| {
                if key == "risk_vocabulary" {
                    Value::Array(Vec::new())
                } else {
                    Value::String(String::new())
                }
            });
        let mut path = prefix.clone();
        path.push(key.to_string());
        if let Some(mut field) = scalar_field(path, key, &value) {
            field.origin = FieldOrigin::Default;
            fields.push(field);
        }
    }
    let mut sections = into_sections(fields, prefix.len());
    apply_adapter_rules(&mut sections);
    sections
}

/// Repository paths, in the form the other queries expect back.
pub fn build_repository_list(repositories: &Value) -> Vec<String> {
    repositories
        .get("repositories")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| {
                    let path = str_at(r, "path");
                    if path.is_empty() {
                        str_at(r, "repository_key").to_string()
                    } else {
                        path.to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A repository on profile defaults still gets its PRD-location fields
    /// on the project page, marked Default and carrying the effective value.
    /// Installation-wide tables never appear on a project page: a repository
    /// entry cannot hold them, so offering them as Inherited was a lie.
    #[test]
    fn project_form_offers_only_what_a_repository_can_override() {
        let document = json!({
            "dashboard": {"bind_address": "127.0.0.1:9400"},
            "logging": {"level": "info"},
            "inference": {"text": {"mode": "hybrid"}},
            "review": {"max_review_attempts": 3},
            "execution_context": {"max_context_tokens": 100000},
            "repositories": {"/r/one": {}}
        });
        let sections = build_project_config_form(&document, "/r/one", None);
        let names: Vec<&str> = sections
            .iter()
            .flat_map(|s| s.fields.iter().map(|f| f.name.as_str()))
            .collect();
        assert!(names.contains(&"max_review_attempts"));
        assert!(names.contains(&"max_context_tokens"));
        assert!(!names.contains(&"bind_address"), "{names:?}");
        assert!(!names.contains(&"level"), "{names:?}");
        assert!(!names.contains(&"mode"), "{names:?}");
        // The global page still shows all of them.
        let global = build_config_form(&document);
        assert!(global
            .iter()
            .flat_map(|s| s.fields.iter())
            .any(|f| f.name == "bind_address"));
    }

    #[test]
    fn project_form_always_offers_the_repository_only_settings() {
        let document = json!({
            "review": {"max_review_attempts": 3},
            "repositories": {
                "/r/one": {"archived_dir": "docs/prds/finished"},
            }
        });
        let defaults = json!({
            "profile": "canonical",
            "active_dir": "docs/prds",
            "archived_dir": "docs/prds/finished",
            "prd_metadata_policy": "incremental",
            "risk_vocabulary": ["persistence", "routing"],
        });
        let sections = build_project_config_form(&document, "/r/one", Some(&defaults));
        let field = |name: &str| {
            sections
                .iter()
                .flat_map(|s| s.fields.iter())
                .find(|f| f.name == name)
                .cloned()
                .unwrap_or_else(|| panic!("{name} missing"))
        };
        assert_eq!(field("archived_dir").origin, FieldOrigin::Overridden);
        assert_eq!(field("active_dir").origin, FieldOrigin::Default);
        assert_eq!(field("active_dir").value, "docs/prds");
        assert_eq!(field("profile").value, "canonical");
        assert_eq!(field("risk_vocabulary").origin, FieldOrigin::Default);
        assert_eq!(
            field("risk_vocabulary").path,
            ["repositories", "/r/one", "risk_vocabulary"]
        );
        assert_eq!(field("max_review_attempts").origin, FieldOrigin::Inherited);
        // Without defaults the fields still exist, empty, so they can be set.
        let bare = build_project_config_form(&document, "/r/two", None);
        assert!(bare
            .iter()
            .flat_map(|s| s.fields.iter())
            .any(|f| f.name == "active_dir" && f.origin == FieldOrigin::Default));
    }

    #[test]
    fn money_and_duration_read_in_human_units() {
        assert_eq!(format_usd(Some(35_689_570)), "$35.69");
        assert_eq!(format_usd(Some(0)), "$0.00");
        assert_eq!(format_usd(None), "—");
        assert_eq!(format_duration(Some(3_433_890)), "57m");
        assert_eq!(format_duration(Some(28_800_000)), "8h 0m");
        assert_eq!(format_duration(None), "—");
    }

    #[test]
    fn settings_view_reads_router_health() {
        let status = json!({
            "text_mode": "hybrid",
            "text_primary": {"loaded": true, "healthy": true, "backend_name": "ollama", "last_error": null},
            "text_fallback": {"loaded": true, "healthy": false, "backend_name": "remote", "last_error": "timeout"},
            "embedding_primary": {"loaded": false, "healthy": false, "backend_name": null, "last_error": null},
            "embedding_fallback": null,
        });
        let view = build_settings_view(&status);
        assert_eq!(view.text_mode, "hybrid");
        assert_eq!(view.text.len(), 2);
        assert_eq!(view.text[0].state, BackendState::Healthy);
        assert_eq!(view.text[0].backend_name, "ollama");
        assert_eq!(view.text[1].state, BackendState::Degraded);
        assert_eq!(view.text[1].last_error.as_deref(), Some("timeout"));
        // A null fallback is absent, not a row reading "not configured".
        assert_eq!(view.embedding.len(), 1);
        assert_eq!(view.embedding[0].state, BackendState::NotLoaded);
    }

    #[test]
    fn gates_group_by_prd_not_by_attempt() {
        let gates = json!({"items": [
            {"prd_id": "PRD-76", "prd_path": "docs/prds/PRD-076.md", "detail": "verification_failed",
             "recovery_commands": ["familiar-ai resume PRD-76"]},
            {"prd_id": "PRD-76", "prd_path": "docs/prds/PRD-076.md", "detail": "malformed_output",
             "recovery_commands": ["familiar-ai resume PRD-76", "familiar-ai waive --help"]},
            {"prd_id": "PRD-92", "prd_path": "docs/prds/PRD-092.md", "detail": "scope_broadened",
             "recovery_commands": ["familiar-ai scope-decisions"]},
        ]});
        let view = build_gates_view(&gates);
        assert_eq!(view.stopped_attempts, 3);
        assert_eq!(view.groups.len(), 2);
        assert_eq!(view.groups[0].prd_id, "PRD-76");
        assert_eq!(
            view.groups[0].reasons,
            vec!["verification_failed", "malformed_output"]
        );
        // Commands are unioned, not repeated once per stopped attempt.
        assert_eq!(
            view.groups[0].recovery_commands,
            vec!["familiar-ai resume PRD-76", "familiar-ai waive --help"]
        );
    }

    #[test]
    fn gates_fall_back_to_kind_when_detail_is_absent() {
        let gates = json!({"items": [
            {"prd_id": "PRD-1", "prd_path": "docs/prds/PRD-001.md", "kind": "stopped_attempt"}
        ]});
        let view = build_gates_view(&gates);
        assert_eq!(view.groups[0].reasons, vec!["stopped_attempt"]);
    }

    #[test]
    fn empty_gates_produce_an_empty_view() {
        let view = build_gates_view(&json!({"items": []}));
        assert_eq!(view.stopped_attempts, 0);
        assert!(view.groups.is_empty());
    }

    #[test]
    fn backlog_counts_every_status_but_lists_only_open_work() {
        let backlog = json!({"items": [
            {"prd_path": "a.md", "status": "completed", "updated_at": "2026-09-01T00:00:00Z"},
            {"prd_path": "b.md", "status": "pending", "updated_at": "2026-09-02T00:00:00Z"},
            {"prd_path": "c.md", "status": "completed", "updated_at": "2026-09-03T00:00:00Z"},
            {"prd_path": "d.md", "status": "in_progress", "updated_at": "2026-09-04T00:00:00Z"},
        ], "next_cursor": null});
        let view = build_backlog_view(&backlog);
        assert_eq!(view.counts[0], ("completed".to_string(), 2));
        assert_eq!(view.open.len(), 2);
        assert!(view.open.iter().all(|r| r.status != "completed"));
        assert!(!view.truncated);
    }

    #[test]
    fn a_full_page_is_reported_as_truncated() {
        let backlog = json!({"items": [], "next_cursor": "docs/prds/PRD-200.md"});
        assert!(build_backlog_view(&backlog).truncated);
    }

    #[test]
    fn a_session_with_no_end_is_still_running() {
        let sessions = json!({"items": [
            {"session_id": "drive-1", "started_at": "2026-09-05T08:00:00Z", "ended_at": null},
            {"session_id": "drive-0", "started_at": "2026-09-04T08:00:00Z", "ended_at": "2026-09-04T09:00:00Z"},
        ]});
        let rows = build_sessions_view(&sessions);
        assert!(rows[0].is_running());
        assert!(!rows[1].is_running());
    }

    #[test]
    fn session_detail_joins_attempts_to_their_review() {
        let budget = json!({
            "known_cost_microusd": 35_689_570,
            "known_cost_attempts": 4,
            "unknown_cost_attempts": 0,
            "warrant": {"max_prds": 4, "max_duration_ms": 28_800_000},
        });
        let attempts = json!({"items": [
            {"sequence": 1, "prd_id": "PRD-83", "model": "sonnet", "outcome": "retained",
             "retained_reason": "integration_failed", "known_cost_microusd": 9_900_946, "duration_ms": 3_433_890},
            {"sequence": 2, "prd_id": "PRD-87", "model": "sonnet", "outcome": "retained",
             "retained_reason": "scope_broadened", "known_cost_microusd": 14_560_000, "duration_ms": 3_300_000},
        ]});
        let review = json!({"items": [
            {"prd_id": "PRD-83", "disposition": "ready_for_human_approval", "blocking_findings": []},
            {"prd_id": "PRD-87", "disposition": "human_review_required", "blocking_findings": [
                {"path": "docs/running_bugs.md", "rule_id": "undeclared_change",
                 "rule_detail": "matches no Expected Files entry"}
            ]},
        ]});
        let view = build_session_detail(&budget, &attempts, &review);
        assert_eq!(view.spent, "$35.69");
        assert_eq!(view.warrant, "4 PRDs / 8h 0m");
        assert_eq!(view.attempts.len(), 2);
        assert_eq!(view.attempts[0].cost, "$9.90");
        assert_eq!(view.attempts[0].duration, "57m");
        assert_eq!(
            view.attempts[0].review_disposition.as_deref(),
            Some("ready_for_human_approval")
        );
        assert_eq!(view.attempts[0].blocking_findings, 0);
        assert_eq!(view.attempts[1].blocking_findings, 1);
        assert_eq!(view.findings.len(), 1);
        assert_eq!(view.findings[0].prd_id, "PRD-87");
        assert_eq!(view.findings[0].rule, "undeclared_change");
    }

    #[test]
    fn an_attempt_with_no_review_is_not_dropped() {
        let attempts =
            json!({"items": [{"sequence": 1, "prd_id": "PRD-99", "outcome": "retained"}]});
        let view = build_session_detail(&json!({}), &attempts, &json!({"items": []}));
        assert_eq!(view.attempts.len(), 1);
        assert!(view.attempts[0].review_disposition.is_none());
        assert_eq!(view.warrant, "unbounded");
    }

    #[test]
    fn long_findings_are_clipped_on_a_char_boundary() {
        let long = "é".repeat(500);
        let clipped = clip(&long, 200);
        assert_eq!(clipped.chars().count(), 201); // 200 plus the ellipsis
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("short", 200), "short");
    }

    #[test]
    fn markup_escapes_everything_gtk_would_choke_on() {
        assert_eq!(
            escape_markup("a & b < c > d ' e \" f"),
            "a &amp; b &lt; c &gt; d &apos; e &quot; f"
        );
        assert_eq!(escape_markup("plain"), "plain");
    }

    /// A repository path or an agent-written reason containing `&` would
    /// otherwise make GTK reject the label and draw nothing at all.
    #[test]
    fn hostile_text_survives_every_label_builder() {
        let group = GateGroup {
            prd_id: "PRD-1 & 2".into(),
            prd_path: "docs/<prd>/a&b.md".into(),
            reasons: vec!["scope_broadened".into(), "a<b".into()],
            recovery_commands: vec![],
        };
        let m = gate_label_markup(&group);
        assert!(m.contains("PRD-1 &amp; 2"));
        assert!(m.contains("docs/&lt;prd&gt;/a&amp;b.md"));
        assert!(m.contains("a&lt;b"));
        assert!(!m.contains("a&b.md"));

        let session = SessionRow {
            session_id: "drive-1&2".into(),
            started_at: "2026-09-05".into(),
            ended_at: None,
        };
        let m = session_label_markup(&session);
        assert!(m.contains("drive-1&amp;2"));
        assert!(m.contains("running"));

        let attempt = AttemptRow {
            sequence: 1,
            prd_id: "PRD-1".into(),
            model: "sonnet".into(),
            outcome: "retained".into(),
            retained_reason: Some("scope & policy".into()),
            review_disposition: Some("human_review_required".into()),
            blocking_findings: 3,
            cost: "$1.00".into(),
            duration: "5m".into(),
        };
        assert!(attempt_outcome_markup(&attempt).contains("scope &amp; policy"));
        assert!(attempt_review_markup(&attempt).contains("3 blocking"));
    }

    #[test]
    fn an_attempt_with_no_review_shows_a_dash_not_an_empty_cell() {
        let attempt = AttemptRow {
            sequence: 1,
            prd_id: "PRD-1".into(),
            model: String::new(),
            outcome: "retained".into(),
            retained_reason: None,
            review_disposition: None,
            blocking_findings: 0,
            cost: "—".into(),
            duration: "—".into(),
        };
        assert_eq!(attempt_review_markup(&attempt), "—");
        assert_eq!(attempt_outcome_markup(&attempt), "retained");
    }

    #[test]
    fn budget_headline_reports_unpriced_attempts_only_when_there_are_some() {
        let mut view = SessionDetailView {
            spent: "$35.69".into(),
            priced_attempts: 4,
            unpriced_attempts: 0,
            warrant: "4 PRDs / 8h 0m".into(),
            attempts: vec![],
            findings: vec![],
        };
        let m = budget_markup(&view);
        assert!(m.contains("Spent $35.69"));
        assert!(m.contains("4 priced attempts"));
        assert!(!m.contains("unpriced"));

        view.unpriced_attempts = 2;
        assert!(budget_markup(&view).contains("2 unpriced"));

        view.priced_attempts = 1;
        assert!(
            budget_markup(&view).contains("1 priced attempt·")
                || budget_markup(&view).contains("1 priced attempt,")
        );
    }

    #[test]
    fn command_json_reads_as_a_command_in_both_accepted_shapes() {
        assert_eq!(
            command_summary(
                r#"{"argv":["familiar-ai","run","docs/prds/PRD-1.md"],"timeout_ms":null}"#
            ),
            "familiar-ai run docs/prds/PRD-1.md"
        );
        // The worker also accepts a bare argv array.
        assert_eq!(
            command_summary(r#"["familiar-ai","drive"]"#),
            "familiar-ai drive"
        );
        // Anything else is shown as-is rather than swallowed.
        assert_eq!(command_summary("not json"), "not json");
    }

    #[test]
    fn only_live_executions_offer_a_stop() {
        let executions = json!({"items": [
            {"execution_id": "e1", "state": "running", "mode": "detached",
             "command_json": r#"{"argv":["familiar-ai","run","a.md"]}"#, "created_at": "t1"},
            {"execution_id": "e2", "state": "queued", "mode": "detached", "command_json": "[]", "created_at": "t2"},
            {"execution_id": "e3", "state": "completed", "mode": "detached", "command_json": "[]", "created_at": "t3"},
            {"execution_id": "e4", "state": "cancelled", "mode": "detached", "command_json": "[]", "created_at": "t4"},
        ]});
        let rows = build_executions_view(&executions);
        assert_eq!(rows.len(), 4, "terminal runs stay visible");
        assert!(rows[0].is_stoppable());
        assert!(rows[1].is_stoppable());
        assert!(!rows[2].is_stoppable());
        assert!(!rows[3].is_stoppable());
        assert_eq!(rows[0].command, "familiar-ai run a.md");
    }

    /// An unregistered project must not read as "active": nothing can be
    /// scheduled for it until it is registered.
    #[test]
    fn project_state_distinguishes_unregistered_from_active() {
        assert_eq!(project_state_label(&json!({"state": "active"})), "active");
        assert_eq!(project_state_label(&json!({"state": "paused"})), "paused");
        assert_eq!(
            project_state_label(&json!({"state": null})),
            "not registered"
        );
        assert_eq!(project_state_label(&json!({})), "not registered");
    }

    #[test]
    fn execution_markup_escapes_its_command() {
        let row = ExecutionRowView {
            execution_id: "e1".into(),
            state: "running".into(),
            mode: "detached".into(),
            command: "sh -c 'a & b'".into(),
            created_at: "t".into(),
        };
        let m = execution_label_markup(&row);
        assert!(m.contains("a &amp; b"));
        assert!(!m.contains("a & b"));
    }

    fn catalogue() -> Value {
        json!({
            "adapters": [
                {"value": "claude-code", "available": true, "detail": "claude at /usr/bin/claude"},
                {"value": "codex", "available": false, "detail": "codex not on PATH"},
            ],
            "permission_modes": ["default", "plan", "acceptEdits", "bypassPermissions"],
            "providers": [{"value": "anthropic", "available": true, "detail": "credential resolves"}],
            "models": ["opus", "sonnet"],
            "inference_modes": ["disabled", "hybrid"],
            "prd_metadata_policies": ["incremental", "strict"],
        })
    }

    #[test]
    fn adapters_are_offered_with_the_ones_that_are_not_installed_labelled() {
        let set = choices_for(
            &["agents".into(), "implementation".into(), "adapter".into()],
            &catalogue(),
        );
        let Some(ChoiceSet::Closed(choices)) = set else {
            panic!("adapters must be a closed set")
        };
        assert_eq!(choices[0].value, "claude-code");
        assert!(choices[0].available);
        // An uninstalled adapter stays listed, saying why: hiding it looks
        // like the form is broken.
        assert_eq!(choices[1].value, "codex");
        assert!(!choices[1].available);
        assert!(choices[1].label.contains("not on PATH"));
    }

    /// The config refuses bypassPermissions for a reviewer, so offering it
    /// would be a choice that is rejected on save.
    #[test]
    fn the_reviewer_is_never_offered_bypass_permissions() {
        let reviewer = choices_for(
            &["agents".into(), "reviewer".into(), "permission_mode".into()],
            &catalogue(),
        );
        let Some(ChoiceSet::Closed(choices)) = reviewer else {
            panic!("closed set")
        };
        assert!(!choices.iter().any(|c| c.value == "bypassPermissions"));

        // The implementer may still have it.
        let implementer = choices_for(
            &[
                "agents".into(),
                "implementation".into(),
                "permission_mode".into(),
            ],
            &catalogue(),
        );
        let Some(ChoiceSet::Closed(choices)) = implementer else {
            panic!("closed set")
        };
        assert!(choices.iter().any(|c| c.value == "bypassPermissions"));
    }

    #[test]
    fn the_same_setting_gets_the_same_dropdown_wherever_it_appears() {
        // A repository's review agent names the same things as [agents.*].
        let path: Vec<String> = [
            "repositories",
            "/p/one",
            "review",
            "implementation_agent",
            "adapter_id",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(matches!(
            choices_for(&path, &catalogue()),
            Some(ChoiceSet::Closed(_))
        ));
    }

    #[test]
    fn model_names_stay_enterable_because_the_list_cannot_be_complete() {
        let set = choices_for(&["agents".into(), "x".into(), "model".into()], &catalogue());
        assert!(matches!(set, Some(ChoiceSet::Open(_))));
    }

    /// With nothing known, a dropdown would make the setting uneditable.
    #[test]
    fn an_empty_catalogue_falls_back_to_free_text() {
        let empty = json!({"adapters": [], "providers": [], "models": []});
        assert!(choices_for(&["agents".into(), "x".into(), "provider".into()], &empty).is_none());
        // `model` is the exception: it stays a dropdown so discovered models
        // can be added to it once they arrive.
        assert!(matches!(
            choices_for(&["agents".into(), "x".into(), "model".into()], &empty),
            Some(ChoiceSet::Open(_))
        ));
        // And a setting with no known options is free text as before.
        assert!(choices_for(
            &["driver".into(), "max_prds_per_session".into()],
            &catalogue()
        )
        .is_none());
    }

    #[test]
    fn only_prds_with_unmet_dependencies_are_reported_as_blocked() {
        let deps = json!({"items": [
            {"prd_path": "a.md", "prd_id": "PRD-1", "blocked_by": []},
            {"prd_path": "b.md", "prd_id": "PRD-2", "blocked_by": [
                {"prd_id": "PRD-1", "status": "pending"},
                {"prd_id": "PRD-9", "status": "not found"},
            ]},
        ]});
        let blockers = build_blockers(&deps);
        // A PRD with everything satisfied is simply absent from the list.
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].0, "b.md");
        assert_eq!(blockers[0].1.len(), 2);

        let row = BacklogRow {
            prd_path: "b.md".into(),
            status: "pending".into(),
            lifecycle: String::new(),
            lifecycle_divergence: None,
            updated_at: String::new(),
            missing_since: None,
        };
        let reason = start_refusal(&row, &blockers[0].1).unwrap();
        assert!(reason.contains("PRD-1 (pending)"), "{reason}");
        // A dependency that cannot be found at all is named rather than hidden.
        assert!(reason.contains("PRD-9 (not found)"), "{reason}");
    }

    #[test]
    fn dependency_gantt_places_children_after_every_parent() {
        let row = |path: &str| BacklogRow {
            prd_path: path.to_string(),
            status: "pending".to_string(),
            lifecycle: String::new(),
            lifecycle_divergence: None,
            updated_at: "2026-09-21T00:00:00Z".to_string(),
            missing_since: None,
        };
        let dependencies = json!({"items": [
            {"prd_id":"PRD-03","prd_path":"docs/prds/PRD-003.md","depends_on":[]},
            {"prd_id":"PRD-95","prd_path":"docs/prds/PRD-095.md","depends_on":[{"prd_id":"PRD-03","status":"pending"}]},
            {"prd_id":"PRD-06","prd_path":"docs/prds/PRD-006.md","depends_on":[{"prd_id":"PRD-03","status":"pending"}]},
            {"prd_id":"PRD-494","prd_path":"docs/prds/PRD-494.md","depends_on":[{"prd_id":"PRD-95","status":"pending"},{"prd_id":"PRD-06","status":"pending"}]},
            {"prd_id":"PRD-01","prd_path":"docs/prds/PRD-001.md","depends_on":[{"prd_id":"PRD-494","status":"pending"}]}
        ]});
        let backlog = [
            row("docs/prds/PRD-003.md"),
            row("docs/prds/PRD-095.md"),
            row("docs/prds/PRD-006.md"),
            row("docs/prds/PRD-494.md"),
            row("docs/prds/PRD-001.md"),
        ];
        let chart = build_dependency_gantt(&dependencies, &backlog);
        assert_eq!(chart.max_wave, 3);
        assert_eq!(chart.waves[0][0].prd_id, "PRD-03");
        assert_eq!(chart.waves[0][0].unlocks, ["PRD-06", "PRD-95"]);
        assert_eq!(chart.waves[1].len(), 2);
        assert_eq!(chart.waves[2][0].prd_id, "PRD-494");
        assert_eq!(chart.waves[3][0].prd_id, "PRD-01");
    }

    /// FAM-BUG-088: PRD-92's three parents had all landed, and it still sat
    /// in Wave 2 while 103/104/106 filled Foundations. Landed work is not a
    /// wave: a child of completed parents is launchable now, and completed
    /// nodes neither take a round of their own nor block a conflict slot.
    #[test]
    fn completed_parents_do_not_push_children_into_later_waves() {
        let row = |path: &str, status: &str, lifecycle: &str| BacklogRow {
            prd_path: path.to_string(),
            status: status.to_string(),
            lifecycle: lifecycle.to_string(),
            lifecycle_divergence: None,
            updated_at: String::new(),
            missing_since: None,
        };
        let dependencies = json!({"items": [
            {"prd_id":"PRD-99","prd_path":"docs/prds/done/PRD-099.md","depends_on":[],"conflicts_with":["PRD-92"]},
            {"prd_id":"PRD-92","prd_path":"docs/prds/PRD-092.md","depends_on":[{"prd_id":"PRD-99","status":"completed"}],"conflicts_with":["PRD-99"]},
            {"prd_id":"PRD-103","prd_path":"docs/prds/PRD-103.md","depends_on":[],"conflicts_with":["PRD-104"]},
            {"prd_id":"PRD-104","prd_path":"docs/prds/PRD-104.md","depends_on":[],"conflicts_with":["PRD-103"]}
        ], "conflicts_error": null});
        let backlog = [
            row("docs/prds/done/PRD-099.md", "completed", "completed"),
            row("docs/prds/PRD-092.md", "pending", "ready"),
            row("docs/prds/PRD-103.md", "pending", "ready"),
            row("docs/prds/PRD-104.md", "pending", "ready"),
        ];
        let chart = build_dependency_gantt(&dependencies, &backlog);
        let wave_of = |id: &str| {
            chart
                .waves
                .iter()
                .position(|wave| wave.iter().any(|node| node.prd_id == id))
                .unwrap()
        };
        assert_eq!(
            wave_of("PRD-92"),
            0,
            "completed parents do not delay a child"
        );
        assert_eq!(wave_of("PRD-103"), 0);
        assert_eq!(
            wave_of("PRD-104"),
            1,
            "scope conflict still serializes 104 after 103"
        );
        assert_eq!(
            wave_of("PRD-99"),
            0,
            "completed work sits in the first wave, hidden by default"
        );
        assert_eq!(chart.max_wave, 1);
        assert!(chart.conflicts_error.is_none());
    }

    /// The chart places PRDs in the scheduler's order, which is numeric.
    /// PRD-92 and PRD-101 conflict; the scheduler admits 92 first, so the
    /// chart must too, whatever the id strings sort like.
    #[test]
    fn wave_placement_follows_prd_number_not_id_text() {
        let row = |path: &str| BacklogRow {
            prd_path: path.to_string(),
            status: "pending".to_string(),
            lifecycle: "ready".to_string(),
            lifecycle_divergence: None,
            updated_at: String::new(),
            missing_since: None,
        };
        let dependencies = json!({"items": [
            {"prd_id":"PRD-101","prd_path":"docs/prds/PRD-101.md","depends_on":[],"conflicts_with":["PRD-92"]},
            {"prd_id":"PRD-92","prd_path":"docs/prds/PRD-092.md","depends_on":[],"conflicts_with":["PRD-101"]}
        ]});
        let chart = build_dependency_gantt(
            &dependencies,
            &[row("docs/prds/PRD-101.md"), row("docs/prds/PRD-092.md")],
        );
        assert_eq!(chart.waves[0][0].prd_id, "PRD-92");
        assert_eq!(chart.waves[1][0].prd_id, "PRD-101");
        assert_eq!(prd_ordinal("PRD-9"), (9, "PRD-9".into()));
        assert!(prd_ordinal("PRD-9") < prd_ordinal("PRD-10"));
        assert!(prd_ordinal("PRD-10") < prd_ordinal("PRD-10a"));
    }

    #[test]
    fn a_conflicts_error_travels_with_the_chart() {
        let dependencies = json!({"items": [
            {"prd_id":"PRD-1","prd_path":"docs/prds/PRD-001.md","depends_on":[]}
        ], "conflicts_error": "PRD-001 (docs/prds/PRD-001.md): no authoritative `## Expected Files` heading found"});
        let chart = build_dependency_gantt(&dependencies, &[]);
        assert!(chart.conflicts_error.unwrap().contains("PRD-001"));
    }

    /// A wave is dependency-ready AND scope-disjoint. Two roots that the
    /// scheduler would serialize on a shared file must not be drawn side by
    /// side, and a child of the later one lands after it.
    #[test]
    fn conflicting_prds_never_share_a_wave() {
        let row = |path: &str| BacklogRow {
            prd_path: path.to_string(),
            status: "pending".to_string(),
            lifecycle: String::new(),
            lifecycle_divergence: None,
            updated_at: String::new(),
            missing_since: None,
        };
        let dependencies = json!({"items": [
            {"prd_id":"PRD-1","prd_path":"docs/prds/PRD-001.md","depends_on":[],"conflicts_with":["PRD-2"]},
            {"prd_id":"PRD-2","prd_path":"docs/prds/PRD-002.md","depends_on":[],"conflicts_with":["PRD-1"]},
            {"prd_id":"PRD-3","prd_path":"docs/prds/PRD-003.md","depends_on":[{"prd_id":"PRD-2","status":"pending"}],"conflicts_with":[]}
        ]});
        let backlog = [
            row("docs/prds/PRD-001.md"),
            row("docs/prds/PRD-002.md"),
            row("docs/prds/PRD-003.md"),
        ];
        let chart = build_dependency_gantt(&dependencies, &backlog);
        assert_eq!(chart.max_wave, 2);
        assert_eq!(
            chart.waves[0]
                .iter()
                .map(|n| n.prd_id.as_str())
                .collect::<Vec<_>>(),
            ["PRD-1"]
        );
        assert_eq!(
            chart.waves[1]
                .iter()
                .map(|n| n.prd_id.as_str())
                .collect::<Vec<_>>(),
            ["PRD-2"]
        );
        assert_eq!(
            chart.waves[2]
                .iter()
                .map(|n| n.prd_id.as_str())
                .collect::<Vec<_>>(),
            ["PRD-3"]
        );
        assert_eq!(chart.waves[0][0].conflicts_with, ["PRD-2"]);
    }

    /// The point of the summary: "33 pending" invites a 34th, while
    /// "5 startable" invites finishing something.
    #[test]
    fn the_summary_counts_work_not_rows() {
        let row = |path: &str, status: &str, missing: bool| BacklogRow {
            prd_path: path.into(),
            status: status.into(),
            lifecycle: String::new(),
            lifecycle_divergence: None,
            updated_at: String::new(),
            missing_since: missing.then(|| "2026-08-09".to_string()),
        };
        let view = BacklogView {
            counts: vec![],
            truncated: false,
            all: vec![],
            open: vec![
                row("free.md", "pending", false),
                row("waiting.md", "pending", false),
                row("claimed.md", "in_progress", false),
                row("gone.md", "pending", true),
                // A missing file outranks everything else: nothing can run.
                row("gone-and-blocked.md", "pending", true),
            ],
        };
        let blockers = vec![
            (
                "waiting.md".to_string(),
                vec![Blocker {
                    prd_id: "PRD-1".into(),
                    status: "pending".into(),
                }],
            ),
            (
                "gone-and-blocked.md".to_string(),
                vec![Blocker {
                    prd_id: "PRD-1".into(),
                    status: "pending".into(),
                }],
            ),
        ];
        let summary = build_backlog_summary(&view, &blockers);
        assert_eq!(summary.startable, 1);
        assert_eq!(summary.blocked, 1);
        assert_eq!(summary.in_progress, 1);
        assert_eq!(summary.missing, 2);

        let headline = summary.headline();
        assert!(headline.starts_with("<b>1 startable</b>"), "{headline}");
        assert!(headline.contains("1 blocked"));
    }

    /// Nothing in the way anywhere: the headline should not be cluttered with
    /// zeroes for categories that are empty.
    #[test]
    fn a_clean_backlog_reports_only_what_is_startable() {
        let view = BacklogView {
            counts: vec![],
            truncated: false,
            all: vec![],
            open: vec![BacklogRow {
                prd_path: "a.md".into(),
                status: "pending".into(),
                lifecycle: String::new(),
                lifecycle_divergence: None,
                updated_at: String::new(),
                missing_since: None,
            }],
        };
        let summary = build_backlog_summary(&view, &[]);
        assert_eq!(summary.headline(), "<b>1 startable</b>");
    }

    #[test]
    fn a_prd_with_nothing_in_its_way_can_be_started() {
        let row = BacklogRow {
            prd_path: "a.md".into(),
            status: "pending".into(),
            lifecycle: String::new(),
            lifecycle_divergence: None,
            updated_at: String::new(),
            missing_since: None,
        };
        assert!(start_refusal(&row, &[]).is_none());
    }

    /// The backlog keeps a row after its file disappears. Nothing can be run
    /// from it, and 25 of the owner's 40 open rows were in exactly this state.
    #[test]
    fn an_entry_whose_file_vanished_cannot_be_started_and_says_so() {
        let row = BacklogRow {
            prd_path: "gone.md".into(),
            status: "pending".into(),
            lifecycle: String::new(),
            lifecycle_divergence: None,
            updated_at: String::new(),
            missing_since: Some("2026-08-09T08:20:17Z".into()),
        };
        let reason = start_refusal(&row, &[]).unwrap();
        assert!(reason.contains("missing since 2026-08-09"), "{reason}");

        // A missing file outranks a dependency: it is the nearer problem.
        let blockers = vec![Blocker {
            prd_id: "PRD-1".into(),
            status: "pending".into(),
        }];
        assert!(start_refusal(&row, &blockers).unwrap().contains("missing"));
    }

    #[test]
    fn backlog_rows_carry_whether_their_file_still_exists() {
        let backlog = json!({"items": [
            {"prd_path": "a.md", "status": "pending", "updated_at": "t", "missing_since": null},
            {"prd_path": "b.md", "status": "pending", "updated_at": "t",
             "missing_since": "2026-08-09T08:20:17Z"},
        ]});
        let v = build_backlog_view(&backlog);
        assert!(v.open[0].missing_since.is_none());
        assert!(v.open[1].missing_since.is_some());
    }

    #[test]
    fn a_block_names_its_cause_using_the_policy_precedence() {
        let reasons = json!({"items": [
            {"prd_path": "a.md", "review_findings": 0, "blocking": [
                {"path": "SPECTRA.md", "decision": "undeclared_scope_expansion", "rule_detail": "no match"},
                {"path": "Cargo.lock", "decision": "ambiguous_human_review", "rule_detail": "lockfile"},
            ]},
            {"prd_path": "b.md", "review_findings": 0, "blocking": [
                {"path": "Cargo.lock", "decision": "ambiguous_human_review", "rule_detail": "lockfile"},
            ]},
        ]});
        let built = build_blocked_reasons(&reasons);
        // Broadened outranks ambiguous, exactly as the policy orders them.
        assert!(
            built[0].1.headline.starts_with("scope broadened"),
            "{:?}",
            built[0].1.headline
        );
        assert!(built[0].1.headline.contains("1 file"));
        assert!(built[1].1.headline.starts_with("needs your decision"));

        let m = blocked_detail_markup(&built[0].1);
        assert!(m.contains("SPECTRA.md"), "{m}");
        assert!(m.contains("undeclared_scope_expansion"), "{m}");
        // The expander label already carries the headline.
        assert!(!m.contains("scope broadened"), "{m}");
    }

    /// One file yields a finding per change id. Counting findings made two
    /// files read as six problems.
    #[test]
    fn repeated_findings_for_one_file_are_counted_and_listed_once() {
        let dup = |path: &str| json!({"path": path, "decision": "undeclared_scope_expansion", "rule_detail": ""});
        let reasons = json!({"items": [{"prd_path": "a.md", "review_findings": 0, "blocking": [
            dup("SPECTRA.md"), dup("docker-compose.yml"),
            dup("SPECTRA.md"), dup("docker-compose.yml"),
            dup("SPECTRA.md"), dup("docker-compose.yml"),
        ]}]});
        let built = build_blocked_reasons(&reasons);
        assert!(
            built[0].1.headline.contains("2 files"),
            "{}",
            built[0].1.headline
        );
        let m = blocked_detail_markup(&built[0].1);
        assert_eq!(m.matches("SPECTRA.md").count(), 1, "{m}");
        assert_eq!(m.matches("docker-compose.yml").count(), 1, "{m}");
    }

    /// Several PRDs really are blocked with no scope finding recorded. Saying
    /// "scope broadened" there would be inventing a cause.
    /// The stock advice says "run the numbered picker" for every scope stop.
    /// When nothing is pending, saying so is the difference between a decision
    /// and a wild goose chase.
    #[test]
    fn outstanding_work_is_reported_from_state_not_from_the_advice() {
        let reasons = json!({"items": [{
            "prd_path": "a.md", "review_findings": 0, "pending_decisions": 0,
            "branch": "familiar/drive-1/PRD-92",
            "blocking": [{"path": "x.md", "decision": "undeclared_scope_expansion", "rule_detail": ""}],
        }]});
        let built = build_blocked_reasons(&reasons);
        let m = outstanding_markup(&built[0].1);
        assert!(m.contains("already been decided"), "{m}");
        assert!(m.contains("familiar/drive-1/PRD-92"), "{m}");

        let reasons = json!({"items": [{
            "prd_path": "b.md", "review_findings": 0, "pending_decisions": 3,
            "branch": null, "blocking": [],
        }]});
        let built = build_blocked_reasons(&reasons);
        let m = outstanding_markup(&built[0].1);
        assert!(m.contains("3 findings awaiting your verdict"), "{m}");
        assert!(m.contains("no retained work"), "{m}");
    }

    /// A candidate parked at `implemented` has not failed; it stopped, and
    /// where it stopped is what the operator needs to know.
    #[test]
    fn a_candidate_parked_mid_pipeline_says_where_it_stopped() {
        let reasons = json!({"items": [
            {"prd_path": "a.md", "phase": "implemented", "review_findings": 0,
             "blocking": [], "invalid_reason": null}
        ]});
        assert_eq!(
            build_blocked_reasons(&reasons)[0].1.headline,
            "stopped after implemented"
        );
    }

    #[test]
    fn a_block_with_nothing_recorded_says_exactly_that() {
        let reasons = json!({"items": [
            {"prd_path": "a.md", "review_findings": 0, "blocking": [], "invalid_reason": null}
        ]});
        let built = build_blocked_reasons(&reasons);
        assert!(built[0]
            .1
            .headline
            .contains("without a recorded scope finding"));

        // An explicit invalid_reason is preferred when there is one.
        let reasons = json!({"items": [
            {"prd_path": "a.md", "review_findings": 0, "blocking": [], "invalid_reason": "dirty worktree"}
        ]});
        assert_eq!(
            build_blocked_reasons(&reasons)[0].1.headline,
            "dirty worktree"
        );
    }

    #[test]
    fn the_pipeline_is_the_order_the_runner_records() {
        assert_eq!(pipeline_index("preflight"), Some(0));
        // Verification happens before independent review.
        assert!(pipeline_index("verified").unwrap() < pipeline_index("reviewed").unwrap());
        assert_eq!(pipeline_index("completed"), Some(PIPELINE.len() - 1));
        assert_eq!(pipeline_index("blocked"), None);
    }

    #[test]
    fn the_meter_marks_what_is_done_what_is_current_and_what_is_left() {
        let progress = Progress {
            reached: Some(2),
            blocked: false,
            detail: "verified".into(),
        };
        let m = progress_markup(&progress);
        assert!(m.contains("<b>Preflight</b>"), "{m}");
        assert!(m.contains("<b>[Verifying]</b>"), "{m}");
        assert!(m.contains("<small>Review</small>"), "{m}");
        assert!(!m.contains("⨯"));
    }

    #[test]
    fn a_blocked_prd_says_so_and_says_why() {
        let checkpoints = json!({"items": [
            {"prd_path": "docs/prds/PRD-1.md", "phase": "implemented"},
            {"prd_path": "docs/prds/PRD-2.md", "phase": "blocked", "invalid_reason": "dirty worktree"},
        ]});
        let progress = build_progress(&checkpoints);
        assert_eq!(progress[0].1.reached, Some(1));
        assert!(!progress[0].1.blocked);
        assert!(progress[1].1.blocked);
        assert!(progress_markup(&progress[1].1).contains("dirty worktree"));
    }

    fn config_fixture() -> Value {
        json!({
            "driver": {"max_prds_per_session": 6},
            "review": {"enabled": true, "max_review_attempts": 3},
            "repositories": {
                "/p/one": {
                    "profile": "strict",
                    "review": {"max_review_attempts": 5},
                },
                "/p/two": {"profile": "loose"},
            },
        })
    }

    /// Per-repository tables are one project's business. Listing them on the
    /// global form produced sections named after a filesystem path, leaving it
    /// ambiguous whether a change was global.
    /// The config refuses `effort` and `permission_mode` for any adapter but
    /// claude-code, so a form that offers them there offers only invalid
    /// values.
    #[test]
    fn fields_an_adapter_rejects_are_not_shown_for_it() {
        let doc = json!({
            "agents": {
                "implementation": {
                    "adapter": "claude-code",
                    "model": "sonnet",
                    "permission_mode": "bypassPermissions",
                    "effort": "high",
                },
                "reviewer": {
                    "adapter": "ollama",
                    "model": "qwen2.5",
                    "permission_mode": "default",
                    "effort": "low",
                },
            }
        });
        let sections = build_config_form(&doc);
        let names = |title: &str| -> Vec<String> {
            sections
                .iter()
                .find(|s| s.title == title)
                .unwrap()
                .fields
                .iter()
                .map(|f| f.name.clone())
                .collect()
        };
        // claude-code keeps them.
        assert!(names("agents.implementation").contains(&"permission_mode".to_string()));
        assert!(names("agents.implementation").contains(&"effort".to_string()));
        // ollama does not, but keeps everything it does accept.
        let reviewer = names("agents.reviewer");
        assert!(
            !reviewer.contains(&"permission_mode".to_string()),
            "{reviewer:?}"
        );
        assert!(!reviewer.contains(&"effort".to_string()), "{reviewer:?}");
        assert!(reviewer.contains(&"adapter".to_string()));
        assert!(reviewer.contains(&"model".to_string()));
    }

    /// The same rule has to hold on a project's own page, where the fields are
    /// nested under `repositories.<path>`.
    #[test]
    fn adapter_rules_apply_on_a_project_form_too() {
        let doc = json!({
            "review": {"implementation_agent": {"adapter_id": "claude-code", "model": "sonnet"}},
            "repositories": {
                "/p/one": {
                    "review": {
                        "implementation_agent": {
                            "adapter_id": "codex",
                            "model": "o4",
                            "permission_mode": "default",
                        }
                    }
                }
            },
        });
        let sections = build_project_config_form(&doc, "/p/one", None);
        let agent = sections
            .iter()
            .find(|s| s.title == "review.implementation_agent")
            .unwrap();
        let names: Vec<&str> = agent.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(!names.contains(&"permission_mode"), "{names:?}");
        assert!(names.contains(&"adapter_id"));
    }

    #[test]
    fn the_global_form_excludes_per_repository_settings() {
        let sections = build_config_form(&config_fixture());
        let titles: Vec<_> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["driver", "review"]);
        assert!(sections
            .iter()
            .flat_map(|s| &s.fields)
            .all(|f| f.path[0] != "repositories"));
        assert!(sections
            .iter()
            .flat_map(|s| &s.fields)
            .all(|f| f.origin == FieldOrigin::Global));
    }

    #[test]
    fn a_project_form_marks_what_it_overrides_and_what_it_inherits() {
        let sections = build_project_config_form(&config_fixture(), "/p/one", None);
        let field = |section: &str, name: &str| {
            sections
                .iter()
                .find(|s| s.title == section)
                .unwrap_or_else(|| panic!("no section {section}"))
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("no field {name}"))
                .clone()
        };

        // Overridden: the project's own value wins and is marked as its own.
        let attempts = field("review", "max_review_attempts");
        assert_eq!(attempts.origin, FieldOrigin::Overridden);
        assert_eq!(attempts.value, "5");
        assert_eq!(
            attempts.path,
            vec!["repositories", "/p/one", "review", "max_review_attempts"]
        );

        // Inherited: the global value is shown, and writing it would create an
        // override at the project's own path.
        let enabled = field("review", "enabled");
        assert_eq!(enabled.origin, FieldOrigin::Inherited);
        assert_eq!(enabled.value, "true");
        assert_eq!(
            enabled.path,
            vec!["repositories", "/p/one", "review", "enabled"]
        );

        // Repository-only settings have no global counterpart to inherit from.
        let profile = field("", "profile");
        assert_eq!(profile.origin, FieldOrigin::Overridden);
        assert_eq!(profile.value, "strict");
    }

    #[test]
    fn a_project_with_no_overrides_inherits_everything() {
        let sections = build_project_config_form(&config_fixture(), "/p/unknown", None);
        assert!(!sections.is_empty());
        // Everything global is inherited; the repository-only PRD-location
        // settings are offered as defaults so they can be set from here.
        assert!(sections.iter().flat_map(|s| &s.fields).all(|f| {
            f.origin == FieldOrigin::Inherited
                || (f.origin == FieldOrigin::Default
                    && REPOSITORY_ONLY_KEYS.contains(&f.name.as_str()))
        }));
        // One project's overrides never leak into another's page.
        assert!(sections
            .iter()
            .flat_map(|s| &s.fields)
            .all(|f| f.value != "strict"));
    }

    #[test]
    fn config_form_covers_every_scalar_and_types_it_from_the_file() {
        let doc = json!({
            "driver": {"max_prds_per_session": 6, "ratio": 0.5, "enabled": true},
            "review": {"enabled": true, "paths": ["a", "b"]},
            "name": "top-level",
        });
        let sections = build_config_form(&doc);
        // Top level first, then alphabetical, so the form does not reshuffle.
        assert_eq!(sections[0].title, "");
        assert_eq!(
            sections
                .iter()
                .map(|s| s.title.as_str())
                .collect::<Vec<_>>(),
            vec!["", "driver", "review"]
        );

        let driver = &sections[1];
        let kinds: Vec<_> = driver
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f.kind.clone(), f.value.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("enabled", FieldKind::Bool, "true"),
                ("max_prds_per_session", FieldKind::Integer, "6"),
                ("ratio", FieldKind::Float, "0.5"),
            ]
        );
        assert_eq!(driver.fields[0].path, vec!["driver", "enabled"]);

        // A list of scalars is editable as text rather than skipped.
        let paths = sections[2]
            .fields
            .iter()
            .find(|f| f.name == "paths")
            .unwrap();
        assert_eq!(paths.kind, FieldKind::List);
        assert_eq!(paths.value, "a, b");
    }

    /// Arrays of tables are the shape `[[repositories.x.verification.checks]]`
    /// produces, and skipping them would silently hide real settings.
    #[test]
    fn config_form_indexes_arrays_of_tables() {
        let doc = json!({
            "verification": {"checks": [
                {"check_id": "fmt", "required": true},
                {"check_id": "test", "required": false},
            ]}
        });
        let sections = build_config_form(&doc);
        let titles: Vec<_> = sections.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["verification.checks.0", "verification.checks.1"]
        );
        assert_eq!(
            sections[1].fields[0].path,
            vec!["verification", "checks", "1", "check_id"]
        );
        assert_eq!(sections[1].fields[1].value, "false");
    }

    #[test]
    fn config_form_omits_empty_sections_and_untyped_values() {
        let doc = json!({
            "empty_table": {},
            "has_null": {"nothing": null, "something": 1},
        });
        let sections = build_config_form(&doc);
        let titles: Vec<_> = sections.iter().map(|s| s.title.as_str()).collect();
        // `empty_table` yields no fields, and a null has no type to preserve.
        assert_eq!(titles, vec!["has_null"]);
        assert_eq!(sections[0].fields.len(), 1);
        assert_eq!(sections[0].fields[0].name, "something");
    }

    #[test]
    fn repository_list_prefers_the_path_form_callers_must_send_back() {
        let repos = json!({"repositories": [
            {"repository_key": "/home/u/p/.git", "path": "/home/u/p"},
            {"repository_key": "/home/u/q/.git"},
        ]});
        assert_eq!(
            build_repository_list(&repos),
            vec!["/home/u/p".to_string(), "/home/u/q/.git".to_string()]
        );
    }
}

#[cfg(test)]
mod escalation_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn gates(rows: serde_json::Value) -> GatesView {
        build_gates_view(&rows)
    }

    #[test]
    fn a_gate_is_announced_once_and_then_never_again() {
        // The property that decides whether the surface stays worth reading.
        let view = gates(json!({"items": [
            {"prd_id": "PRD-90", "prd_path": "docs/prds/PRD-090.md", "kind": "scope_violation"},
            {"prd_id": "PRD-100", "prd_path": "docs/prds/PRD-100.md", "kind": "human_review_required"}
        ]}));

        let first = escalation(&view, &BTreeSet::new());
        assert_eq!(first.count, 2);
        assert_eq!(first.to_announce.len(), 2, "both are new");

        let second = escalation(&view, &first.seen);
        assert_eq!(second.count, 2, "the badge still shows them");
        assert!(
            second.to_announce.is_empty(),
            "nothing new to say: {:?}",
            second.to_announce
        );
    }

    #[test]
    fn the_same_prd_stopping_a_new_way_is_new_information() {
        let before = gates(json!({"items": [
            {"prd_id": "PRD-90", "prd_path": "p", "kind": "scope_violation"}
        ]}));
        let seen = escalation(&before, &BTreeSet::new()).seen;

        let after = gates(json!({"items": [
            {"prd_id": "PRD-90", "prd_path": "p", "kind": "scope_violation"},
            {"prd_id": "PRD-90", "prd_path": "p", "kind": "verification_failed"}
        ]}));
        let next = escalation(&after, &seen);
        assert_eq!(next.to_announce.len(), 1, "the new reason is announced");
    }

    #[test]
    fn a_decided_gate_that_recurs_announces_again() {
        // Memory is rebuilt from the live set, not accumulated, so deciding a
        // gate and having it come back is not silently suppressed.
        let view = gates(json!({"items": [
            {"prd_id": "PRD-90", "prd_path": "p", "kind": "scope_violation"}
        ]}));
        let seen = escalation(&view, &BTreeSet::new()).seen;

        let cleared = escalation(&gates(json!({"items": []})), &seen);
        assert_eq!(cleared.count, 0);
        assert!(
            cleared.seen.is_empty(),
            "nothing is remembered once resolved"
        );

        let returned = escalation(&view, &cleared.seen);
        assert_eq!(returned.to_announce.len(), 1, "it must speak up again");
    }

    #[test]
    fn nothing_pending_is_completely_silent() {
        let empty = escalation(&gates(json!({"items": []})), &BTreeSet::new());
        assert_eq!(empty.count, 0);
        assert!(empty.to_announce.is_empty());
    }

    #[test]
    fn the_message_names_the_prd_and_why_it_stopped() {
        let view = gates(json!({"items": [
            {"prd_id": "PRD-90", "prd_path": "docs/prds/PRD-090.md", "kind": "scope_violation"}
        ]}));
        let (title, body) = escalation_message(&view.groups[0]);
        assert!(title.contains("PRD-90"), "{title}");
        assert!(body.contains("scope_violation"), "{body}");
    }
}

// ------------------------------------------------------------ rounds view

/// One round: a driver session, and what it did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Round {
    /// 1-based position in the chart, left to right, oldest first.
    pub ordinal: usize,
    pub session_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub termination_reason: Option<String>,
}

/// What happened to one PRD in one round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum RoundOutcome {
    Completed,
    /// Stopped with work retained — the state a PRD sits in when a gate or a
    /// scope finding halted it.
    Retained,
    /// Started and never recorded an end. Either running now, or the daemon
    /// died mid-attempt; the ledger cannot tell those apart, so neither does
    /// this.
    Unfinished,
}

impl RoundOutcome {
    /// The colour of the bar. Chosen to survive a dark theme, and to put
    /// "retained" nearer to a warning than to a failure, because retained
    /// work is waiting for a person rather than broken.
    pub fn colour(&self) -> &'static str {
        match self {
            Self::Completed => "#2e7d32",
            Self::Retained => "#b26a00",
            Self::Unfinished => "#4a4a4a",
        }
    }

    pub fn caption(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Retained => "retained",
            Self::Unfinished => "unfinished",
        }
    }
}

/// One PRD's appearance in one round.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RoundCell {
    pub outcome: RoundOutcome,
    pub sequence: i64,
    pub retained_reason: Option<String>,
    pub duration_ms: Option<u64>,
}

/// One PRD across every round.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Lane {
    pub prd_path: String,
    pub status: String,
    /// One entry per round, in the same order as [`RoundsView::rounds`].
    /// `None` where this PRD was not touched in that round.
    pub cells: Vec<Option<RoundCell>>,
    /// True when this PRD appears in at least one round.
    pub recorded: bool,
}

impl Lane {
    /// The first round this PRD appears in, used to sort the chart so the
    /// waterfall steps down and to the right instead of scattering.
    pub fn first_round(&self) -> Option<usize> {
        self.cells.iter().position(Option::is_some)
    }

    pub fn attempts(&self) -> usize {
        self.cells.iter().filter(|c| c.is_some()).count()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct RoundsView {
    pub rounds: Vec<Round>,
    /// PRDs that appear in at least one round, ordered by where they first
    /// appear.
    pub recorded: Vec<Lane>,
    /// PRDs the ledger has nothing on. Kept, not dropped: a chart that shows
    /// only what was recorded invites reading absence as inactivity.
    pub unrecorded: Vec<Lane>,
    /// How many sessions exist in total, against how many are drawn.
    pub total_sessions: usize,
}

impl RoundsView {
    /// The honest headline: how much of the backlog this chart can actually
    /// place. Everything else is a count of what was never written down.
    pub fn coverage(&self) -> (usize, usize) {
        let total = self.recorded.len() + self.unrecorded.len();
        (self.recorded.len(), total)
    }

    pub fn truncated(&self) -> bool {
        self.total_sessions > self.rounds.len()
    }
}

/// Builds the waterfall from the rounds payload and the backlog rows.
///
/// Every backlog row becomes a lane, whether or not the driver ever touched
/// it, because the question "what has this repository done" is not the same
/// as "what did the driver do" — and on a repository where most PRDs were
/// finished by hand, answering only the second one would be a lie of omission.
pub fn build_rounds_view(rounds: &Value, backlog: &[BacklogRow]) -> RoundsView {
    let sessions: Vec<Round> = rounds
        .get("sessions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(|(index, session)| Round {
            ordinal: index + 1,
            session_id: str_at(session, "session_id").to_string(),
            started_at: str_at(session, "started_at").to_string(),
            ended_at: session
                .get("ended_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            termination_reason: session
                .get("termination_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
        .collect();

    let column_of: std::collections::HashMap<&str, usize> = sessions
        .iter()
        .enumerate()
        .map(|(index, round)| (round.session_id.as_str(), index))
        .collect();

    let attempts = rounds
        .get("attempts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut recorded = Vec::new();
    let mut unrecorded = Vec::new();
    for row in backlog {
        let mut cells: Vec<Option<RoundCell>> = vec![None; sessions.len()];
        for attempt in attempts {
            if str_at(attempt, "prd_path") != row.prd_path {
                continue;
            }
            let Some(&column) = column_of.get(str_at(attempt, "session_id")) else {
                continue;
            };
            let outcome = match attempt.get("outcome").and_then(Value::as_str) {
                Some("completed") => RoundOutcome::Completed,
                Some("retained") => RoundOutcome::Retained,
                _ => RoundOutcome::Unfinished,
            };
            let cell = RoundCell {
                outcome,
                sequence: attempt
                    .get("sequence")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                retained_reason: attempt
                    .get("retained_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                duration_ms: attempt.get("duration_ms").and_then(Value::as_u64),
            };
            // A PRD can be attempted twice in one session (an escalation
            // re-runs it). The later attempt is the one that decided the
            // round, so it wins the cell.
            match &cells[column] {
                Some(existing) if existing.sequence > cell.sequence => {}
                _ => cells[column] = Some(cell),
            }
        }
        let recorded_here = cells.iter().any(Option::is_some);
        let lane = Lane {
            prd_path: row.prd_path.clone(),
            status: row.status.clone(),
            cells,
            recorded: recorded_here,
        };
        if recorded_here {
            recorded.push(lane);
        } else {
            unrecorded.push(lane);
        }
    }

    // Earliest appearance first, so the chart steps forward in time. Ties
    // break on path, so the order does not wobble between refreshes.
    recorded.sort_by(|a, b| {
        a.first_round()
            .cmp(&b.first_round())
            .then_with(|| a.prd_path.cmp(&b.prd_path))
    });
    unrecorded.sort_by(|a, b| a.prd_path.cmp(&b.prd_path));

    RoundsView {
        rounds: sessions,
        recorded,
        unrecorded,
        total_sessions: rounds
            .get("total_sessions")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize,
    }
}

/// How wide one round's cell is, in monospace characters.
///
/// The whole strip is one Pango label rather than a widget per cell: a
/// hundred-odd PRDs against twenty rounds is two thousand cells, and two
/// thousand widgets is a window that takes a visible moment to open.
/// Monospace plus a fixed width is what keeps the columns under the header.
const CELL_WIDTH: usize = 3;

/// The column headings: the round numbers, right-aligned over their columns.
///
/// Deliberately the same size as the lanes below. The heading started out
/// `<small>`, which looks tidier and is wrong: a smaller font has a narrower
/// monospace advance, so the headings drifted left of their own columns a
/// little more with every round until, six columns along, the number sat over
/// the wrong bar.
pub fn rounds_header_markup(view: &RoundsView) -> String {
    let mut out = String::from("<tt>");
    for round in &view.rounds {
        out.push_str(&format!("{:>width$}", round.ordinal, width = CELL_WIDTH));
    }
    out.push_str("</tt>");
    out
}

/// One lane's strip of bars.
pub fn lane_markup(lane: &Lane) -> String {
    let mut out = String::from("<tt>");
    for cell in &lane.cells {
        match cell {
            Some(cell) => out.push_str(&format!(
                "<span background=\"{}\">{}</span>",
                cell.outcome.colour(),
                " ".repeat(CELL_WIDTH)
            )),
            // A dim marker rather than blank space, so the columns stay
            // readable across a wide chart and an empty round is visibly a
            // round rather than a gap in the drawing.
            None => out.push_str(&format!(
                "<span foreground=\"#555555\"> {} </span>",
                "\u{00b7}"
            )),
        }
    }
    out.push_str("</tt>");
    out
}

/// What a lane says on hover: which rounds touched it and how each ended.
pub fn lane_tooltip(lane: &Lane, rounds: &[Round]) -> String {
    if !lane.recorded {
        return format!(
            "{}\nNo recorded round. The ledger has no attempt for this PRD.",
            lane.prd_path
        );
    }
    let mut lines = vec![lane.prd_path.clone()];
    for (index, cell) in lane.cells.iter().enumerate() {
        let Some(cell) = cell else { continue };
        let ordinal = rounds.get(index).map(|r| r.ordinal).unwrap_or(index + 1);
        let mut line = format!("Round {ordinal}: {}", cell.outcome.caption());
        if let Some(ms) = cell.duration_ms {
            line.push_str(&format!(" in {}", human_duration(ms)));
        }
        if let Some(reason) = &cell.retained_reason {
            line.push_str(&format!(" — {reason}"));
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// A duration a person can read, from milliseconds.
pub fn human_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// The line that stops the chart from lying by omission.
///
/// Without it, a reader sees bars for a quarter of the backlog and concludes
/// the rest never happened, when in fact most of it was finished outside the
/// driver and simply never written to the ledger.
pub fn coverage_note(view: &RoundsView) -> String {
    let (placed, total) = view.coverage();
    if total == 0 {
        return "Nothing in the backlog.".to_string();
    }
    if placed == total {
        return format!("All {total} PRDs are placed in a round.");
    }
    format!(
        "{placed} of {total} PRDs appear in a recorded round. \
         The other {} were never driven by a session, so the ledger has no \
         round to place them in.",
        total - placed
    )
}

/// How the rounds axis itself is captioned, including the truncation it is
/// hiding.
pub fn rounds_caption(view: &RoundsView) -> String {
    if view.rounds.is_empty() {
        return "No rounds recorded — the driver has never run here.".to_string();
    }
    if view.truncated() {
        return format!(
            "Last {} of {} rounds, oldest on the left.",
            view.rounds.len(),
            view.total_sessions
        );
    }
    format!("{} rounds, oldest on the left.", view.rounds.len())
}

#[cfg(test)]
mod rounds_tests {
    use super::*;
    use serde_json::json;

    fn row(path: &str, status: &str) -> BacklogRow {
        BacklogRow {
            prd_path: path.to_string(),
            status: status.to_string(),
            lifecycle: String::new(),
            lifecycle_divergence: None,
            updated_at: "2026-09-01T00:00:00Z".to_string(),
            missing_since: None,
        }
    }

    /// The page counted completed PRDs in its header and then showed only the
    /// open ones. The chart needs the finished work too — that is most of
    /// what a repository has done.
    #[test]
    fn the_backlog_view_keeps_completed_rows_as_well_as_open_ones() {
        let backlog = json!({"items": [
            {"prd_path": "a.md", "status": "completed", "updated_at": "t"},
            {"prd_path": "b.md", "status": "pending", "updated_at": "t"},
            {"prd_path": "c.md", "status": "completed", "updated_at": "t"},
        ]});
        let view = build_backlog_view(&backlog);
        assert_eq!(
            view.open.len(),
            1,
            "open is still only the outstanding work"
        );
        assert_eq!(view.all.len(), 3, "all is everything, completed included");
        assert_eq!(
            view.all
                .iter()
                .map(|r| r.prd_path.as_str())
                .collect::<Vec<_>>(),
            ["a.md", "b.md", "c.md"]
        );
    }

    fn payload() -> Value {
        json!({
            "sessions": [
                {"session_id": "s1", "started_at": "2026-09-01T00:00:00Z",
                 "ended_at": "2026-09-01T01:00:00Z", "termination_reason": "prd_ceiling"},
                {"session_id": "s2", "started_at": "2026-09-02T00:00:00Z",
                 "ended_at": null, "termination_reason": null},
            ],
            "attempts": [
                {"session_id": "s1", "sequence": 1, "prd_id": "PRD-1", "prd_path": "docs/prds/PRD-1.md",
                 "started_at": "2026-09-01T00:00:00Z", "ended_at": "2026-09-01T00:30:00Z",
                 "outcome": "completed", "retained_reason": null, "duration_ms": 1_800_000},
                {"session_id": "s2", "sequence": 1, "prd_id": "PRD-2", "prd_path": "docs/prds/PRD-2.md",
                 "started_at": "2026-09-02T00:00:00Z", "ended_at": null,
                 "outcome": "retained", "retained_reason": "scope broadened", "duration_ms": null},
            ],
            "total_sessions": 5,
        })
    }

    /// The finding that shaped this view: most of a real backlog has never
    /// been near a driver session. Those PRDs must still appear, separately
    /// and labelled, rather than being dropped because they have no bar.
    #[test]
    fn prds_with_no_recorded_round_are_kept_and_counted() {
        let backlog = vec![
            row("docs/prds/PRD-1.md", "completed"),
            row("docs/prds/PRD-2.md", "in_progress"),
            row("docs/prds/PRD-3.md", "completed"),
            row("docs/prds/PRD-4.md", "pending"),
        ];
        let view = build_rounds_view(&payload(), &backlog);

        assert_eq!(view.recorded.len(), 2);
        assert_eq!(view.unrecorded.len(), 2);
        assert_eq!(view.coverage(), (2, 4));
        // Including a *completed* one: finished work with no ledger entry is
        // exactly the case that must not read as "never happened".
        let unrecorded: Vec<&str> = view
            .unrecorded
            .iter()
            .map(|l| l.prd_path.as_str())
            .collect();
        assert_eq!(unrecorded, ["docs/prds/PRD-3.md", "docs/prds/PRD-4.md"]);
    }

    #[test]
    fn the_coverage_note_names_what_is_missing() {
        let backlog = vec![
            row("docs/prds/PRD-1.md", "completed"),
            row("docs/prds/PRD-3.md", "completed"),
        ];
        let view = build_rounds_view(&payload(), &backlog);
        let note = coverage_note(&view);
        assert!(note.contains("1 of 2"), "{note}");
        assert!(note.contains("never driven by a session"), "{note}");
    }

    #[test]
    fn full_coverage_says_so_without_a_caveat() {
        let backlog = vec![
            row("docs/prds/PRD-1.md", "completed"),
            row("docs/prds/PRD-2.md", "in_progress"),
        ];
        let view = build_rounds_view(&payload(), &backlog);
        assert_eq!(coverage_note(&view), "All 2 PRDs are placed in a round.");
    }

    #[test]
    fn a_lane_carries_one_cell_per_round_in_session_order() {
        let backlog = vec![row("docs/prds/PRD-2.md", "in_progress")];
        let view = build_rounds_view(&payload(), &backlog);
        let lane = &view.recorded[0];
        assert_eq!(lane.cells.len(), 2, "one cell per round, filled or not");
        assert!(lane.cells[0].is_none(), "PRD-2 was not in round 1");
        assert_eq!(
            lane.cells[1].as_ref().unwrap().outcome,
            RoundOutcome::Retained
        );
        assert_eq!(lane.first_round(), Some(1));
        assert_eq!(lane.attempts(), 1);
    }

    /// An attempt with no outcome has not finished. The ledger cannot tell a
    /// running attempt from one whose daemon died, and neither should this.
    #[test]
    fn an_attempt_without_an_outcome_is_unfinished() {
        let payload = json!({
            "sessions": [{"session_id": "s1", "started_at": "2026-09-01T00:00:00Z",
                          "ended_at": null, "termination_reason": null}],
            "attempts": [{"session_id": "s1", "sequence": 1, "prd_id": "PRD-1",
                          "prd_path": "docs/prds/PRD-1.md", "started_at": "2026-09-01T00:00:00Z",
                          "ended_at": null, "outcome": null, "retained_reason": null,
                          "duration_ms": null}],
            "total_sessions": 1,
        });
        let view = build_rounds_view(&payload, &[row("docs/prds/PRD-1.md", "in_progress")]);
        assert_eq!(
            view.recorded[0].cells[0].as_ref().unwrap().outcome,
            RoundOutcome::Unfinished
        );
    }

    /// An escalation re-runs a PRD inside the same session. The round shows
    /// how it ended, which is the later attempt, not the one it superseded.
    #[test]
    fn a_second_attempt_in_one_round_supersedes_the_first() {
        let payload = json!({
            "sessions": [{"session_id": "s1", "started_at": "2026-09-01T00:00:00Z",
                          "ended_at": null, "termination_reason": null}],
            "attempts": [
                {"session_id": "s1", "sequence": 1, "prd_id": "PRD-1", "prd_path": "docs/prds/PRD-1.md",
                 "started_at": "t", "ended_at": "t", "outcome": "retained",
                 "retained_reason": "first try", "duration_ms": null},
                {"session_id": "s1", "sequence": 2, "prd_id": "PRD-1", "prd_path": "docs/prds/PRD-1.md",
                 "started_at": "t", "ended_at": "t", "outcome": "completed",
                 "retained_reason": null, "duration_ms": null},
            ],
            "total_sessions": 1,
        });
        let view = build_rounds_view(&payload, &[row("docs/prds/PRD-1.md", "completed")]);
        let cell = view.recorded[0].cells[0].as_ref().unwrap();
        assert_eq!(cell.outcome, RoundOutcome::Completed);
        assert_eq!(cell.sequence, 2);
    }

    /// The waterfall has to step forward: a lane that first appears in an
    /// earlier round sits above one that appears later.
    #[test]
    fn lanes_are_ordered_by_where_they_first_appear() {
        let backlog = vec![
            row("docs/prds/PRD-2.md", "in_progress"),
            row("docs/prds/PRD-1.md", "completed"),
        ];
        let view = build_rounds_view(&payload(), &backlog);
        let order: Vec<&str> = view.recorded.iter().map(|l| l.prd_path.as_str()).collect();
        assert_eq!(
            order,
            ["docs/prds/PRD-1.md", "docs/prds/PRD-2.md"],
            "PRD-1 first appears in round 1, PRD-2 in round 2"
        );
    }

    /// Header and lanes are drawn as separate labels, so they line up only if
    /// both spend the same number of monospace characters per round *and*
    /// render them at the same size.
    #[test]
    fn every_cell_is_the_same_width_as_its_heading() {
        let backlog = vec![row("docs/prds/PRD-2.md", "in_progress")];
        let view = build_rounds_view(&payload(), &backlog);

        let header = rounds_header_markup(&view);
        assert_eq!(visible_chars(&header), view.rounds.len() * 3, "{header}");

        let lane = lane_markup(&view.recorded[0]);
        assert_eq!(visible_chars(&lane), view.rounds.len() * 3, "{lane}");
    }

    /// Equal character counts are not equal widths if one side is drawn
    /// smaller. The heading used `<small>` and drifted a little further left
    /// with every column; by the sixth round the number sat over the wrong
    /// bar. Neither side may carry a size tag.
    #[test]
    fn neither_the_heading_nor_the_lanes_change_font_size() {
        let view = build_rounds_view(&payload(), &[row("docs/prds/PRD-2.md", "in_progress")]);
        for markup in [rounds_header_markup(&view), lane_markup(&view.recorded[0])] {
            for tag in ["<small>", "<big>", "size="] {
                assert!(
                    !markup.contains(tag),
                    "{tag} changes the monospace advance and breaks alignment: {markup}"
                );
            }
        }
    }

    /// Strips Pango tags, leaving what the reader actually sees.
    fn visible_chars(markup: &str) -> usize {
        let mut count = 0;
        let mut in_tag = false;
        for ch in markup.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => count += 1,
                _ => {}
            }
        }
        count
    }

    #[test]
    fn the_caption_admits_truncation() {
        let view = build_rounds_view(&payload(), &[]);
        assert!(view.truncated(), "2 of 5 sessions drawn");
        let caption = rounds_caption(&view);
        assert!(caption.contains("Last 2 of 5 rounds"), "{caption}");
    }

    #[test]
    fn no_rounds_at_all_says_the_driver_never_ran() {
        let empty = json!({"sessions": [], "attempts": [], "total_sessions": 0});
        let view = build_rounds_view(&empty, &[row("docs/prds/PRD-1.md", "pending")]);
        assert!(view.rounds.is_empty());
        assert_eq!(view.unrecorded.len(), 1);
        assert!(rounds_caption(&view).contains("never run here"));
    }

    #[test]
    fn a_tooltip_names_the_round_the_outcome_and_the_reason() {
        let backlog = vec![row("docs/prds/PRD-2.md", "in_progress")];
        let view = build_rounds_view(&payload(), &backlog);
        let tip = lane_tooltip(&view.recorded[0], &view.rounds);
        assert!(tip.contains("Round 2: retained"), "{tip}");
        assert!(tip.contains("scope broadened"), "{tip}");
    }

    #[test]
    fn an_unrecorded_lane_says_why_it_has_no_bars() {
        let view = build_rounds_view(&payload(), &[row("docs/prds/PRD-9.md", "completed")]);
        let tip = lane_tooltip(&view.unrecorded[0], &view.rounds);
        assert!(tip.contains("No recorded round"), "{tip}");
    }

    #[test]
    fn durations_read_as_time_not_milliseconds() {
        assert_eq!(human_duration(45_000), "45s");
        assert_eq!(human_duration(1_800_000), "30m 0s");
        assert_eq!(human_duration(5_400_000), "1h 30m");
    }
}

// ------------------------------------------------------------------ gantt

/// One PRD's run inside a session, positioned in time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GanttBar {
    pub prd_id: String,
    pub outcome: String,
    pub detail: String,
    pub duration_label: String,
    /// Columns from the session's start before the bar begins.
    pub offset: usize,
    /// Columns the bar spans. Never zero: a run that happened is visible.
    pub width: usize,
}

/// One drive session, with its own time axis.
///
/// Sessions rather than one global axis because attempts span months while a
/// session spans minutes — a single linear scale would compress every run into
/// the same pixel. This is the same reason the reference Gantt groups tasks
/// under phases rather than laying a quarter out flat.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GanttSession {
    pub session_id: String,
    pub span_label: String,
    pub bars: Vec<GanttBar>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct GanttView {
    pub sessions: Vec<GanttSession>,
    /// PRDs that have never been driven, so have no run to chart.
    pub undriven: usize,
}

fn parse_stamp(value: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(value).ok()
}

/// Lay one session's attempts out in time.
///
/// `columns` is the width of the chart in characters. Offsets and widths are
/// proportional to wall-clock, so two attempts that ran at once sit under each
/// other — which is the thing the round grid could not show and the whole
/// reason to plot time at all.
pub fn build_gantt_session(session: &Value, attempts: &Value, columns: usize) -> GanttSession {
    let session_id = str_at(session, "session_id").to_string();
    let rows = items(attempts);

    let starts: Vec<i64> = rows
        .iter()
        .filter_map(|row| parse_stamp(str_at(row, "started_at")))
        .map(|t| t.timestamp_millis())
        .collect();
    let origin = starts.iter().copied().min().unwrap_or(0);
    let last_end = rows
        .iter()
        .filter_map(|row| {
            let start = parse_stamp(str_at(row, "started_at"))?.timestamp_millis();
            let ms = row.get("duration_ms").and_then(Value::as_i64).unwrap_or(0);
            Some(start + ms)
        })
        .max()
        .unwrap_or(origin);
    let span = (last_end - origin).max(1);

    let mut bars = Vec::new();
    for row in rows.iter() {
        let Some(started) = parse_stamp(str_at(row, "started_at")) else {
            continue;
        };
        let start_ms = started.timestamp_millis();
        let duration = row.get("duration_ms").and_then(Value::as_i64).unwrap_or(0);
        let offset = (((start_ms - origin) as f64 / span as f64) * columns as f64) as usize;
        // A run that happened is always at least one column wide; rounding a
        // short attempt to nothing would say it never ran.
        let width = ((duration as f64 / span as f64) * columns as f64)
            .round()
            .max(1.0) as usize;
        let outcome = str_at(row, "outcome").to_string();
        let detail = {
            let reason = str_at(row, "retained_reason");
            if reason.is_empty() {
                outcome.clone()
            } else {
                reason.to_string()
            }
        };
        bars.push(GanttBar {
            prd_id: str_at(row, "prd_id").to_string(),
            outcome,
            detail,
            duration_label: human_duration(duration.max(0) as u64),
            offset: offset.min(columns.saturating_sub(1)),
            width: width.min(columns.saturating_sub(offset).max(1)),
        });
    }
    // Longest first: the run that dominated the session is the one worth
    // seeing at a glance.
    bars.sort_by(|a, b| b.width.cmp(&a.width).then(a.prd_id.cmp(&b.prd_id)));

    let span_label = match (
        parse_stamp(str_at(session, "started_at")),
        parse_stamp(str_at(session, "ended_at")),
    ) {
        (Some(start), Some(end)) => format!(
            "{} → {} · {}",
            start.format("%Y-%m-%d %H:%M"),
            end.format("%H:%M"),
            human_duration((end - start).num_milliseconds().max(0) as u64)
        ),
        (Some(start), None) => format!("{} · running", start.format("%Y-%m-%d %H:%M")),
        _ => String::new(),
    };

    GanttSession {
        session_id,
        span_label,
        bars,
    }
}

/// The bar itself, as monospace blocks.
///
/// Pango markup rather than a drawing area, so what the operator sees stays a
/// tested pure function and GTK only lays it out — the idiom this crate
/// already follows.
pub fn gantt_bar_markup(bar: &GanttBar, columns: usize) -> String {
    let colour = match bar.outcome.as_str() {
        "completed" => "#4a9e5c",
        "retained" => "#d6883b",
        _ => "#7a7a7a",
    };
    let lead = " ".repeat(bar.offset.min(columns));
    let body = "█".repeat(
        bar.width
            .clamp(1, columns.saturating_sub(bar.offset).max(1)),
    );
    let tail = columns.saturating_sub(bar.offset + bar.width);
    format!(
        "<tt>{lead}<span foreground=\"{colour}\">{body}</span>{}</tt>",
        " ".repeat(tail)
    )
}

#[cfg(test)]
mod gantt_tests {
    use super::*;
    use serde_json::json;

    fn session() -> Value {
        json!({
            "session_id": "drive-1",
            "started_at": "2026-09-19T11:23:48+00:00",
            "ended_at": "2026-09-19T12:39:14+00:00"
        })
    }

    /// Round 1's four PRDs all started at 11:27:41. The round grid drew one
    /// dot each and could not show that; a time axis must.
    #[test]
    fn simultaneous_runs_line_up_under_each_other() {
        let attempts = json!({"items": [
            {"prd_id": "PRD-85",  "started_at": "2026-09-19T11:27:41+00:00", "duration_ms": 2297286, "outcome": "retained"},
            {"prd_id": "PRD-90",  "started_at": "2026-09-19T11:27:41+00:00", "duration_ms": 1866000, "outcome": "retained"},
            {"prd_id": "PRD-96",  "started_at": "2026-09-19T11:27:41+00:00", "duration_ms": 1045000, "outcome": "retained"},
            {"prd_id": "PRD-100", "started_at": "2026-09-19T11:27:41+00:00", "duration_ms": 4292000, "outcome": "retained"}
        ]});
        let g = build_gantt_session(&session(), &attempts, 40);
        assert_eq!(g.bars.len(), 4);
        for bar in &g.bars {
            assert_eq!(bar.offset, 0, "{} started with the others", bar.prd_id);
        }
        // Longest first, and the longest fills the axis it defines.
        assert_eq!(g.bars[0].prd_id, "PRD-100");
        assert_eq!(g.bars[0].width, 40);
        // Width is proportional: 1045s against 4292s is roughly a quarter.
        let shortest = g.bars.iter().find(|b| b.prd_id == "PRD-96").unwrap();
        assert!(
            (9..=11).contains(&shortest.width),
            "expected about a quarter of 40, got {}",
            shortest.width
        );
    }

    #[test]
    fn a_later_start_is_offset_from_the_first() {
        let attempts = json!({"items": [
            {"prd_id": "PRD-1", "started_at": "2026-09-19T11:00:00+00:00", "duration_ms": 600000, "outcome": "completed"},
            {"prd_id": "PRD-2", "started_at": "2026-09-19T11:10:00+00:00", "duration_ms": 600000, "outcome": "completed"}
        ]});
        let g = build_gantt_session(&session(), &attempts, 40);
        let first = g.bars.iter().find(|b| b.prd_id == "PRD-1").unwrap();
        let second = g.bars.iter().find(|b| b.prd_id == "PRD-2").unwrap();
        assert_eq!(first.offset, 0);
        assert!(
            second.offset >= 18,
            "second ran after the first: {second:?}"
        );
    }

    #[test]
    fn a_run_that_happened_is_never_invisible() {
        // Rounding a short attempt to zero columns would say it never ran.
        let attempts = json!({"items": [
            {"prd_id": "PRD-long",  "started_at": "2026-09-19T11:00:00+00:00", "duration_ms": 7200000, "outcome": "completed"},
            {"prd_id": "PRD-blink", "started_at": "2026-09-19T11:00:00+00:00", "duration_ms": 1000,    "outcome": "retained"}
        ]});
        let g = build_gantt_session(&session(), &attempts, 40);
        let blink = g.bars.iter().find(|b| b.prd_id == "PRD-blink").unwrap();
        assert!(blink.width >= 1, "a run that happened must be visible");
    }

    #[test]
    fn the_bar_never_overruns_the_chart() {
        let attempts = json!({"items": [
            {"prd_id": "PRD-1", "started_at": "2026-09-19T11:00:00+00:00", "duration_ms": 600000, "outcome": "completed"},
            {"prd_id": "PRD-2", "started_at": "2026-09-19T11:09:00+00:00", "duration_ms": 600000, "outcome": "retained"}
        ]});
        let g = build_gantt_session(&session(), &attempts, 40);
        for bar in &g.bars {
            assert!(
                bar.offset + bar.width <= 40,
                "{} runs off the axis: offset {} width {}",
                bar.prd_id,
                bar.offset,
                bar.width
            );
            let markup = gantt_bar_markup(bar, 40);
            assert!(markup.starts_with("<tt>") && markup.ends_with("</tt>"));
        }
    }

    #[test]
    fn outcome_is_legible_without_reading_the_label() {
        let completed = GanttBar {
            prd_id: "PRD-1".into(),
            outcome: "completed".into(),
            detail: "completed".into(),
            duration_label: "10m".into(),
            offset: 0,
            width: 4,
        };
        let retained = GanttBar {
            outcome: "retained".into(),
            ..completed.clone()
        };
        assert!(gantt_bar_markup(&completed, 10).contains("#4a9e5c"));
        assert!(gantt_bar_markup(&retained, 10).contains("#d6883b"));
    }

    #[test]
    fn an_empty_session_charts_nothing_rather_than_dividing_by_zero() {
        let g = build_gantt_session(&session(), &json!({"items": []}), 40);
        assert!(g.bars.is_empty());
        assert!(g.span_label.contains("2026-09-19 11:23"));
    }
}
