use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tokio::sync::mpsc;
use tray_icon::TrayIconBuilder;

use familiar_core::config::TrayConfig;
use familiar_core::{AppStatus, FamiliarError, VersionInfo};
use familiar_storage::{Database, ProjectRepository};

use crate::commands::TrayCommand;
use crate::icon::load_tray_icon;
use crate::menu::{build_tooltip, MenuItemSpec};

pub struct TrayApp {
    config: TrayConfig,
    status: Arc<Mutex<AppStatus>>,
    db: Arc<Mutex<Database>>,
    command_tx: mpsc::Sender<TrayCommand>,
    config_path: PathBuf,
}

impl TrayApp {
    pub fn new(
        config: TrayConfig,
        status: Arc<Mutex<AppStatus>>,
        db: Arc<Mutex<Database>>,
        command_tx: mpsc::Sender<TrayCommand>,
        config_path: PathBuf,
    ) -> Self {
        Self {
            config,
            status,
            db,
            command_tx,
            config_path,
        }
    }

    /// Run the tray on the main thread. This blocks until quit.
    pub fn run(self) -> Result<(), FamiliarError> {
        #[cfg(target_os = "linux")]
        {
            gtk::init()
                .map_err(|e| FamiliarError::Config(format!("failed to initialize GTK: {e}")))?;
        }

        let icon = load_tray_icon()?;

        // Build initial menu
        let (menu, ids) = self.build_muda_menu()?;

        let _tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(build_tooltip(&self.status.lock().unwrap()))
            .with_icon(icon)
            .build()
            .map_err(|e| FamiliarError::Config(format!("failed to build tray icon: {e}")))?;

        // Set up menu event handler
        let menu_channel = MenuEvent::receiver();
        let command_tx = self.command_tx.clone();
        let config_path = self.config_path.clone();
        let ids_arc = Arc::new(Mutex::new(ids));

        // Start a background thread to poll status and update menu/tooltip periodically
        let status_clone = self.status.clone();
        let _db_clone = self.db.clone();
        let _ids_for_refresh = ids_arc.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if let Ok(s) = status_clone.lock() {
                tracing::trace!(active_projects = s.active_projects, "tray status tick");
            }
        });

        // Main loop: receive menu events and dispatch
        #[cfg(target_os = "linux")]
        {
            // Spawn a glib timer to poll menu events
            let ids_for_loop = ids_arc.clone();
            let command_tx_for_loop = command_tx.clone();
            let config_path_for_loop = config_path.clone();
            glib::source::timeout_add_local(Duration::from_millis(100), move || {
                while let Ok(event) = menu_channel.try_recv() {
                    let ids = ids_for_loop.lock().unwrap();
                    if let Some(cmd) = ids.resolve(&event.id) {
                        handle_command(cmd, &command_tx_for_loop, &config_path_for_loop);
                    }
                }
                glib::ControlFlow::Continue
            });
            gtk::main();
        }

        #[cfg(not(target_os = "linux"))]
        {
            // macOS: simple loop polling menu events
            loop {
                if let Ok(event) = menu_channel.try_recv() {
                    let ids = ids_arc.lock().unwrap();
                    if let Some(cmd) = ids.resolve(&event.id) {
                        let should_quit = matches!(cmd, TrayCommand::Quit);
                        handle_command(cmd, &command_tx, &config_path);
                        if should_quit {
                            break;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        Ok(())
    }

    fn build_muda_menu(&self) -> Result<(Menu, MenuIdMap), FamiliarError> {
        let menu = Menu::new();
        let mut ids = MenuIdMap::new();

        let status = self.status.lock().unwrap().clone();
        let recent = self
            .db
            .lock()
            .unwrap()
            .list_active_projects()
            .unwrap_or_default();

        let spec =
            crate::menu::build_menu_spec(&status, &recent, self.config.recent_projects_count);
        drop(recent);

        for item in spec {
            match item {
                MenuItemSpec::Header(text) => {
                    let mi = MenuItem::new(text, false, None);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::Separator => {
                    menu.append(&PredefinedMenuItem::separator()).ok();
                }
                MenuItemSpec::LlmToggle { enabled } => {
                    let label = if enabled {
                        "Disable Local LLM"
                    } else {
                        "Enable Local LLM"
                    };
                    let mi = MenuItem::new(label, true, None);
                    let cmd = if enabled {
                        TrayCommand::DisableLlm
                    } else {
                        TrayCommand::EnableLlm
                    };
                    ids.insert(mi.id().clone(), cmd);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::PauseToggle { paused } => {
                    let label = if paused {
                        "Resume Heavy Tasks"
                    } else {
                        "Pause Heavy Tasks"
                    };
                    let mi = MenuItem::new(label, true, None);
                    let cmd = if paused {
                        TrayCommand::ResumeHeavyTasks
                    } else {
                        TrayCommand::PauseHeavyTasks
                    };
                    ids.insert(mi.id().clone(), cmd);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::RecentProjectsHeader => {
                    let mi = MenuItem::new("Recent Projects", false, None);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::EmptyRecentProjects => {
                    let mi = MenuItem::new("  (none)", false, None);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::RecentProject { name, repo_root } => {
                    let label = format!("  {name}");
                    let mi = MenuItem::new(label, true, None);
                    ids.insert(
                        mi.id().clone(),
                        TrayCommand::OpenProject(PathBuf::from(repo_root)),
                    );
                    menu.append(&mi).ok();
                }
                MenuItemSpec::OpenSettings => {
                    let mi = MenuItem::new("Settings", true, None);
                    ids.insert(mi.id().clone(), TrayCommand::OpenSettings);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::About => {
                    let version = VersionInfo::current();
                    let label = format!("About ({version})");
                    let mi = MenuItem::new(label, false, None);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::Quit => {
                    let mi = MenuItem::new("Quit", true, None);
                    ids.insert(mi.id().clone(), TrayCommand::Quit);
                    menu.append(&mi).ok();
                }
            }
        }

        // Suppress unused warning for Submenu import
        let _ = std::marker::PhantomData::<Submenu>;

        Ok((menu, ids))
    }
}

pub struct MenuIdMap {
    map: HashMap<muda::MenuId, TrayCommand>,
}

impl MenuIdMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn insert(&mut self, id: muda::MenuId, cmd: TrayCommand) {
        self.map.insert(id, cmd);
    }

    pub fn resolve(&self, id: &muda::MenuId) -> Option<TrayCommand> {
        self.map.get(id).cloned()
    }
}

impl Default for MenuIdMap {
    fn default() -> Self {
        Self::new()
    }
}

fn handle_command(cmd: TrayCommand, command_tx: &mpsc::Sender<TrayCommand>, config_path: &PathBuf) {
    match &cmd {
        TrayCommand::OpenSettings => {
            if let Err(e) = opener::open(config_path) {
                tracing::warn!(error = %e, "failed to open settings file");
            }
        }
        TrayCommand::OpenProject(path) => {
            if let Err(e) = opener::open(path) {
                tracing::warn!(error = %e, path = %path.display(), "failed to open project");
            }
        }
        _ => {}
    }
    if let Err(e) = command_tx.try_send(cmd) {
        tracing::warn!(error = %e, "failed to send tray command");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_id_map_insert_resolve() {
        let mut map = MenuIdMap::new();
        let id = muda::MenuId::new("test-id");
        map.insert(id.clone(), TrayCommand::Quit);
        assert!(matches!(map.resolve(&id), Some(TrayCommand::Quit)));
    }

    #[test]
    fn menu_id_map_unknown_returns_none() {
        let map = MenuIdMap::new();
        let id = muda::MenuId::new("unknown");
        assert!(map.resolve(&id).is_none());
    }
}
