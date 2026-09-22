#[test]
fn frontend_maps_the_complete_operator_inventory() {
    let javascript = include_str!("../ui/app.js");
    for query in familiar_ai_core::operator_ui::OperatorQuery::NAMES {
        assert!(
            javascript.contains(&format!("query:'{query}'")),
            "desktop has no control for query {query}"
        );
    }
    // Mutations whose controls require a selected row are provided by the
    // generic action dispatcher even when no fixture row exists at startup.
    for action in familiar_ai_core::operator_ui::OperatorAction::NAMES {
        assert!(
            javascript.contains(&format!("action:'{action}'")),
            "desktop has no action path for {action}"
        );
    }
}

#[test]
fn release_configuration_is_locked_down() {
    let config = include_str!("../tauri.conf.json");
    assert!(config.contains("object-src 'none'"));
    assert!(config.contains("frame-src 'none'"));
    assert!(!config.contains("http://127.0.0.1"));
    assert!(include_str!("../src/windows.rs").contains(".devtools(false)"));
    let capability = include_str!("../capabilities/default.json");
    assert!(!capability.contains("shell:"));
    assert!(!capability.contains("fs:"));
    assert!(!capability.contains("http:"));
}

#[test]
fn windows_are_singletons_and_the_desktop_has_no_direct_authority() {
    let windows = include_str!("../src/windows.rs");
    assert!(windows.contains("get_webview_window(label)"));
    assert!(windows.contains("set_focus"));
    assert!(windows.contains("on_navigation"));

    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains("rusqlite"));
    assert!(!manifest.contains("toml_edit"));
    assert!(!manifest.contains("reqwest"));
    let client = include_str!("../src/client.rs");
    assert!(!client.contains("std::process::Command"));
    assert!(!client.contains("Authorization"));
}

#[test]
fn quitting_the_desktop_does_not_request_daemon_shutdown() {
    let main = include_str!("../src/main.rs");
    assert!(main.contains("\"quit-desktop\" => app.exit(0)"));
    assert!(!main.contains("\"quit-desktop\" => { let _ = app.emit(\"familiar://request-stop\""));
    assert!(main.contains("\"stop-daemon\""));
}

#[test]
fn tauri_reuses_the_gtk_presentation_contract() {
    let adapter = include_str!("../src/presentation.rs");
    let javascript = include_str!("../ui/app.js");
    for builder in [
        "build_settings_view",
        "build_gates_view",
        "build_backlog_view",
        "build_blockers",
        "build_dependency_gantt",
        "build_blocked_reasons",
        "build_progress",
        "build_rounds_view",
        "build_executions_view",
        "build_sessions_view",
        "build_session_detail",
        "build_gantt_session",
        "build_config_form",
        "build_project_config_form",
    ] {
        assert!(
            adapter.contains(builder),
            "desktop omitted GTK view builder {builder}"
        );
    }
    assert!(javascript.contains("invoke('present'"));
    assert!(!javascript.contains("JSON.stringify(data"));
    assert!(!javascript.contains("prd-path"));
    assert!(!javascript.contains("execution-id"));
    assert!(!javascript.contains("session-id"));
}

#[test]
fn status_pills_have_explicit_contrast_and_normalized_names() {
    let javascript = include_str!("../ui/app.js");
    let css = include_str!("../ui/styles.css");
    assert!(javascript.contains("replace(/[^a-z0-9]+/g,'-')"));
    for status in [
        "draft",
        "ready",
        "implementing",
        "testing",
        "reviewed",
        "approved",
        "completed",
        "blocked",
        "failed",
        "awaiting-feedback",
    ] {
        assert!(
            css.contains(&format!(".status-{status}")),
            "missing explicit pill style for {status}"
        );
    }
    assert!(css.contains(".status{border:1px solid transparent;background:#475467;color:#fff"));
}

#[test]
fn backlog_can_hide_completed_prds_without_losing_the_dependency_graph() {
    let javascript = include_str!("../ui/app.js");
    assert!(javascript.contains("id=\"hide-completed\""));
    assert!(javascript.contains("n.dataset.lifecycle==='completed'"));
    assert!(javascript.contains("state.hideCompleted"));
    assert!(javascript.contains("id=\"lifecycle-filter\""));
    assert!(javascript.contains("Ledger: ${esc(node.status)}"));
}

#[test]
fn dependency_waves_have_gated_batch_launch_controls() {
    let javascript = include_str!("../ui/app.js");
    assert!(javascript.contains(">Launch wave</button>"));
    assert!(javascript.contains("lifecycle(n)==='ready'"));
    assert!(javascript.contains("launchable.length===unfinished.length"));
    assert!(javascript.contains("Promise.all(paths.map(path=>mutate({action:'start_prd'"));
    assert!(javascript.contains("disabled title="));
}
