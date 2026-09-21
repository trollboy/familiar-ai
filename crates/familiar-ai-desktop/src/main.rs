mod client;
mod commands;
mod presentation;
mod windows;

use std::sync::Arc;

use commands::ClientState;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::Emitter;
use tokio::sync::Mutex;

fn main() {
    let client = match client::DesktopClient::resolve() {
        Ok(client) => Arc::new(Mutex::new(client)),
        Err(error) => {
            eprintln!("cannot initialize Familiar desktop: {error}");
            std::process::exit(1);
        }
    };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            windows::show_dashboard(app);
        }))
        .manage::<ClientState>(client)
        .invoke_handler(tauri::generate_handler![
            commands::connection_status,
            commands::operator_query,
            commands::operator_mutate,
            commands::operator_observe,
            presentation::present
        ])
        .setup(|app| {
            let dashboard =
                MenuItem::with_id(app, "dashboard", "Open Dashboard", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            let llm =
                MenuItem::with_id(app, "local-llm", "Configure Local LLM…", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop-daemon", "Stop Familiar…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit-desktop", "Quit Desktop", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&dashboard, &settings, &llm, &stop, &quit])?;
            let icon = tauri::image::Image::from_bytes(include_bytes!(
                "../../familiar-ai-tray/assets/icon.png"
            ))?;
            TrayIconBuilder::with_id("familiar-ai")
                .icon(icon)
                .icon_as_template(true)
                .tooltip("Familiar")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "dashboard" => {
                        let _ = windows::open(app, "dashboard");
                    }
                    "settings" => {
                        let _ = windows::open(app, "settings");
                    }
                    "local-llm" => {
                        let _ = windows::open(app, "local-llm");
                    }
                    "quit-desktop" => app.exit(0),
                    "stop-daemon" => {
                        let _ = windows::open(app, "dashboard");
                        let _ = app.emit("familiar://request-stop", ());
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(event, tauri::tray::TrayIconEvent::DoubleClick { .. }) {
                        windows::show_dashboard(tray.app_handle());
                    }
                })
                .build(app)?;
            windows::show_dashboard(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Familiar desktop");

    // Closing a window does not stop the daemon; Quit Desktop exits only this
    // process. The tray keeps the app alive when no window is visible.
    app.run(|_, _| {});
}
