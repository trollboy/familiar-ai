//! Tauri adapter for the mature, display-neutral GTK view models.
//!
//! The desktop must not reinterpret ledger rows independently. Both shells
//! consume the same tested grouping, status, blocker, formatting, and config
//! rules from `familiar_ai_tray::view` while GTK remains the parity oracle.

use familiar_ai_tray::view;
use serde_json::{json, Value};

#[tauri::command]
pub fn present(kind: String, input: Value) -> Result<Value, String> {
    let rendered = match kind.as_str() {
        "inference" => serde_json::to_value(view::build_settings_view(required(&input, "status")?)),
        "gates" => serde_json::to_value(view::build_gates_view(required(&input, "gates")?)),
        "backlog" => {
            let backlog = view::build_backlog_view(required(&input, "backlog")?);
            let blockers = view::build_blockers(required(&input, "dependencies")?);
            let blocked_reasons = view::build_blocked_reasons(required(&input, "blocked_reasons")?);
            let progress = view::build_progress(required(&input, "checkpoints")?);
            let rounds = view::build_rounds_view(required(&input, "rounds")?, &backlog.all);
            let summary = view::build_backlog_summary(&backlog, &blockers);
            serde_json::to_value(json!({
                "backlog": backlog,
                "blockers": blockers,
                "blocked_reasons": blocked_reasons,
                "progress": progress,
                "rounds": rounds,
                "summary": summary,
            }))
        }
        "executions" => {
            serde_json::to_value(view::build_executions_view(required(&input, "executions")?))
        }
        "sessions" => {
            serde_json::to_value(view::build_sessions_view(required(&input, "sessions")?))
        }
        "session_detail" => serde_json::to_value(view::build_session_detail(
            required(&input, "budget")?,
            required(&input, "attempts")?,
            required(&input, "review")?,
        )),
        "gantt" => serde_json::to_value(view::build_gantt_session(
            required(&input, "session")?,
            required(&input, "attempts")?,
            44,
        )),
        "config" => {
            let document = required(&input, "document")?;
            let sections = match input.get("repo").and_then(Value::as_str) {
                Some(repo) if !repo.is_empty() => view::build_project_config_form(document, repo),
                _ => view::build_config_form(document),
            };
            let catalogue = required(&input, "choices")?;
            let fields: Vec<Value> = sections
                .iter()
                .flat_map(|section| {
                    section.fields.iter().map(move |field| {
                        json!({
                            "section": section.title,
                            "field": field,
                            "choices": view::choices_for(&field.path, catalogue),
                        })
                    })
                })
                .collect();
            serde_json::to_value(json!({ "sections": sections, "fields": fields }))
        }
        _ => return Err(format!("unknown presentation kind: {kind}")),
    };
    rendered.map_err(|error| format!("cannot serialize {kind} presentation: {error}"))
}

fn required<'a>(input: &'a Value, key: &str) -> Result<&'a Value, String> {
    input
        .get(key)
        .ok_or_else(|| format!("presentation input is missing {key}"))
}
