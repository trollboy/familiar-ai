use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub fn open(app: &AppHandle, label: &str) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(label) {
        window.show()?;
        window.unminimize()?;
        window.set_focus()?;
        return Ok(());
    }
    let title = match label {
        "settings" => "Familiar Settings",
        "local-llm" => "Configure Local LLM",
        _ => "Familiar",
    };
    WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(format!("index.html#{label}").into()),
    )
    .title(title)
    .inner_size(1180.0, 780.0)
    .min_inner_size(800.0, 600.0)
    .devtools(false)
    .on_navigation(|url| matches!(url.scheme(), "tauri" | "asset"))
    .build()?;
    Ok(())
}

pub fn show_dashboard(app: &AppHandle) {
    let _ = open(app, "dashboard");
}
