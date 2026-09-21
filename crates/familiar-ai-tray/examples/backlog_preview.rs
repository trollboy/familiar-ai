//! Opens the dashboard window against canned data, so the backlog waterfall
//! can be looked at without a daemon, a database or a real repository.
//!
//! Development tool, not a product surface: the layout is the one thing the
//! view tests cannot check, and a chart is worth seeing before it ships.
//!
//!     cargo run -p familiar-ai-tray --example backlog_preview

use std::sync::Arc;

use serde_json::{json, Value};

use gtk::prelude::*;

use familiar_ai_tray::data::{Action, DataSource, Query};

struct Canned;

/// Shaped like the repository this was built against: a handful of rounds,
/// a few PRDs the driver actually touched, and a long tail that was finished
/// by hand and never written to the ledger.
impl DataSource for Canned {
    fn query(&self, query: Query) -> Result<Value, String> {
        Ok(match query {
            Query::Repositories => json!({"repositories": [{"path": "/home/you/Projects/demo"}]}),
            Query::Backlog { .. } => {
                let mut items = Vec::new();
                for (path, status) in [
                    ("docs/prds/PRD-090.md", "in_progress"),
                    ("docs/prds/PRD-092.md", "in_progress"),
                    ("docs/prds/PRD-100.md", "in_progress"),
                    ("docs/prds/PRD-082.md", "pending"),
                    ("docs/prds/PRD-086.md", "pending"),
                    ("docs/prds/PRD-093.md", "pending"),
                ] {
                    items.push(json!({"prd_path": path, "status": status, "updated_at": "2026-09-19T10:00:00Z"}));
                }
                for n in 1..=14 {
                    items.push(json!({
                        "prd_path": format!("docs/prds/PRD-{n:03}.md"),
                        "status": "completed",
                        "updated_at": "2026-08-01T10:00:00Z",
                    }));
                }
                json!({"items": items, "next_cursor": null})
            }
            Query::Rounds { .. } => {
                let sessions: Vec<Value> = (1..=6)
                    .map(|n| {
                        json!({
                            "session_id": format!("s{n}"),
                            "started_at": format!("2026-09-{:02}T09:00:00Z", 10 + n),
                            "ended_at": format!("2026-09-{:02}T17:00:00Z", 10 + n),
                            "termination_reason": "prd_ceiling",
                        })
                    })
                    .collect();
                let attempt = |session: &str,
                               seq: i64,
                               prd: &str,
                               outcome: Option<&str>,
                               reason: Option<&str>,
                               ms: Option<u64>| {
                    json!({
                        "session_id": session, "sequence": seq,
                        "prd_id": prd, "prd_path": format!("docs/prds/{prd}.md"),
                        "started_at": "2026-09-11T09:00:00Z", "ended_at": null,
                        "outcome": outcome, "retained_reason": reason, "duration_ms": ms,
                    })
                };
                json!({
                    "sessions": sessions,
                    "attempts": [
                        attempt("s1", 1, "PRD-001", Some("completed"), None, Some(1_500_000)),
                        attempt("s1", 2, "PRD-002", Some("retained"), Some("a gate is open"), Some(900_000)),
                        attempt("s2", 1, "PRD-002", Some("completed"), None, Some(600_000)),
                        attempt("s2", 2, "PRD-090", Some("retained"), Some("stopped without a recorded scope finding"), Some(2_400_000)),
                        attempt("s3", 1, "PRD-090", Some("retained"), Some("stopped without a recorded scope finding"), Some(3_000_000)),
                        attempt("s4", 1, "PRD-092", Some("retained"), Some("scope broadened — 2 files outside the declared scope"), Some(1_200_000)),
                        attempt("s5", 1, "PRD-003", Some("completed"), None, Some(450_000)),
                        attempt("s6", 1, "PRD-100", None, None, None),
                    ],
                    "total_sessions": 21,
                })
            }
            Query::Checkpoints { .. } => json!({"items": []}),
            Query::BlockedReasons { .. } => json!({"items": []}),
            Query::Dependencies { .. } => json!({"items": []}),
            Query::Gates { .. } => json!({"items": []}),
            Query::ProjectState { .. } => json!({"state": "active"}),
            _ => json!({"items": []}),
        })
    }

    fn act(&self, _action: Action) -> Result<Value, String> {
        Ok(json!({"note": "preview — nothing was done"}))
    }
}

fn main() {
    gtk::init().expect("GTK should start");
    familiar_ai_tray::windows::open_dashboard_window(Arc::new(Canned));
    // Straight to the page this preview exists for. The window opens on its
    // first tab, which is not the one being looked at.
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(300), || {
        for window in gtk::Window::list_toplevels() {
            if let Some(notebook) = find_notebook(&window) {
                notebook.set_current_page(Some(1));
            }
        }
        gtk::glib::ControlFlow::Break
    });
    // With PREVIEW_OPEN_MENU set, pop the first row's Actions menu so the
    // dropdown can be looked at too.
    if std::env::var_os("PREVIEW_OPEN_MENU").is_some() {
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(1200), || {
            for window in gtk::Window::list_toplevels() {
                if let Some(button) = find_menu_button(&window) {
                    button.emit_clicked();
                    break;
                }
            }
            gtk::glib::ControlFlow::Break
        });
    }
    gtk::main();
}

fn find_menu_button(widget: &gtk::Widget) -> Option<gtk::MenuButton> {
    use gtk::prelude::*;
    if let Ok(button) = widget.clone().downcast::<gtk::MenuButton>() {
        return Some(button);
    }
    let container = widget.clone().downcast::<gtk::Container>().ok()?;
    container.children().iter().find_map(find_menu_button)
}

/// The dashboard's notebook, wherever it ended up in the widget tree.
fn find_notebook(widget: &gtk::Widget) -> Option<gtk::Notebook> {
    use gtk::prelude::*;
    if let Ok(notebook) = widget.clone().downcast::<gtk::Notebook>() {
        return Some(notebook);
    }
    let container = widget.clone().downcast::<gtk::Container>().ok()?;
    container.children().iter().find_map(find_notebook)
}
