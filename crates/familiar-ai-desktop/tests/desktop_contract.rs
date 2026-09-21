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
