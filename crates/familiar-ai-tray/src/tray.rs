use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tokio::sync::{mpsc, watch};
use tray_icon::TrayIconBuilder;

use familiar_ai_core::config::TrayConfig;
use familiar_ai_core::{AppStatus, FamiliarError, VersionInfo};
use familiar_ai_storage::{Database, ProjectRepository};

use crate::commands::{DashboardTarget, TrayCommand};
use crate::data::DataSource;
use crate::icon::{load_tray_icon, tray_icon_with_count};
use crate::menu::{build_tooltip, MenuItemSpec};

pub struct TrayApp {
    config: TrayConfig,
    status: Arc<Mutex<AppStatus>>,
    db: Arc<Mutex<Database>>,
    command_tx: mpsc::Sender<TrayCommand>,
    config_path: PathBuf,
    /// How the dashboard is reached, or `None` when it is unreachable and the
    /// menu item should be omitted entirely.
    dashboard: Option<DashboardTarget>,
    /// Answers the windows' queries. Absent in builds with no data behind the
    /// tray, in which case the windows are never opened.
    source: Option<Arc<dyn DataSource>>,
    /// PRD-102: how many PRDs are waiting on a decision, as last counted by
    /// the badge poll. Held rather than queried so building the menu — which
    /// happens on the GTK thread, in response to a click — never blocks on
    /// the database.
    pending_gates: Arc<AtomicUsize>,
    /// Flipped true once the daemon is shutting down. `gtk::main()` owns the
    /// main thread and returns only when something calls `gtk::main_quit()`,
    /// so without this the process outlives its own SIGTERM: the workers stop,
    /// the icon stays in the tray, and the PID file is never removed.
    shutdown_rx: watch::Receiver<bool>,
}

impl TrayApp {
    /// The count the menu label uses. Read from the badge poll's last result.
    fn pending_gates(&self) -> usize {
        self.pending_gates.load(Ordering::Relaxed)
    }

    // Every argument is a distinct collaborator the tray holds for its whole
    // life; collapsing them into a parameter struct would only move the same
    // eight names one indirection away.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: TrayConfig,
        status: Arc<Mutex<AppStatus>>,
        db: Arc<Mutex<Database>>,
        command_tx: mpsc::Sender<TrayCommand>,
        config_path: PathBuf,
        dashboard: Option<DashboardTarget>,
        source: Option<Arc<dyn DataSource>>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            config,
            status,
            db,
            command_tx,
            config_path,
            dashboard,
            source,
            shutdown_rx,
            pending_gates: Arc::new(AtomicUsize::new(0)),
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

        // The handle must outlive the builder: updating the badge means
        // calling set_icon on a live tray, and the previous code dropped it
        // into `_tray` where it could never be reached again.
        let tray = TrayIconBuilder::new()
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

        // PRD-102: the badge and the notification.
        //
        // This runs on the GTK thread because both touch UI — the tray icon
        // and gio's notification service. The cadence is deliberately slow:
        // a gate is a human-scale event and polling it every second would
        // spend the daemon's time telling itself nothing changed.
        #[cfg(target_os = "linux")]
        if let Some(source) = self.source.clone() {
            use crate::notify::{DesktopNotifier, Notifier};
            let tray_for_badge = tray;
            let notifier = DesktopNotifier::new("net.shoggoth.familiar-ai");
            let mut announced: std::collections::BTreeSet<String> = Default::default();
            let mut drawn: Option<usize> = None;
            let gate_count = self.pending_gates.clone();
            glib::source::timeout_add_local(Duration::from_secs(10), move || {
                let repos = match source.query(crate::data::Query::Repositories) {
                    Ok(v) => v,
                    Err(error) => {
                        tracing::debug!(%error, "tray: could not list repositories for gates");
                        return glib::ControlFlow::Continue;
                    }
                };
                let mut groups = Vec::new();
                for repo in crate::view::build_repository_list(&repos) {
                    if let Ok(gates) = source.query(crate::data::Query::Gates { repo }) {
                        groups.extend(crate::view::build_gates_view(&gates).groups);
                    }
                }
                let view = crate::view::GatesView {
                    stopped_attempts: groups.len(),
                    groups,
                };
                let escalation = crate::view::escalation(&view, &announced);

                // Redraw only on change: composing a 512x512 icon every tick
                // to produce the same bytes is work nobody asked for.
                gate_count.store(escalation.count, Ordering::Relaxed);
                if drawn != Some(escalation.count) {
                    match tray_icon_with_count(escalation.count) {
                        Ok(icon) => {
                            if let Err(error) = tray_for_badge.set_icon(Some(icon)) {
                                tracing::warn!(%error, "tray: could not update the badge");
                            } else {
                                drawn = Some(escalation.count);
                            }
                        }
                        Err(error) => tracing::warn!(%error, "tray: could not compose the badge"),
                    }
                }

                // A notifier that fails must never alter what the gate surface
                // reports: the badge above is already correct, and `announced`
                // is updated regardless so a broken notification service
                // cannot produce an endless retry every tick.
                for group in &escalation.to_announce {
                    let (title, body) = crate::view::escalation_message(group);
                    if let Err(error) = notifier.notify(&title, &body) {
                        tracing::warn!(%error, prd = %group.prd_id, "tray: notification not delivered");
                    }
                }
                announced = escalation.seen;
                glib::ControlFlow::Continue
            });
        }

        // Main loop: receive menu events and dispatch
        #[cfg(target_os = "linux")]
        {
            // Spawn a glib timer to poll menu events
            let ids_for_loop = ids_arc.clone();
            let command_tx_for_loop = command_tx.clone();
            let config_path_for_loop = config_path.clone();
            let shutdown_rx = self.shutdown_rx.clone();
            let source_for_loop = self.source.clone();
            glib::source::timeout_add_local(Duration::from_millis(100), move || {
                let mut quitting = false;
                while let Ok(event) = menu_channel.try_recv() {
                    let ids = ids_for_loop.lock().unwrap();
                    if let Some(cmd) = ids.resolve(&event.id) {
                        // Read before the move: handle_command consumes it.
                        quitting |= matches!(cmd, TrayCommand::Quit);
                        handle_command(
                            cmd,
                            &command_tx_for_loop,
                            &config_path_for_loop,
                            source_for_loop.as_ref(),
                        );
                    }
                }
                // Either end of the shutdown can reach us: the user picked Quit
                // from the menu, or the daemon is stopping on its own (SIGTERM).
                // Both have to leave gtk::main(), which is what returns the main
                // thread to the caller so the PID file is removed.
                if quitting || *shutdown_rx.borrow() {
                    gtk::main_quit();
                    return glib::ControlFlow::Break;
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
                        handle_command(cmd, &command_tx, &config_path, self.source.as_ref());
                        if should_quit {
                            break;
                        }
                    }
                }
                if *self.shutdown_rx.borrow() {
                    break;
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

        let spec = crate::menu::build_menu_spec(
            &status,
            &recent,
            self.config.recent_projects_count,
            self.dashboard.clone(),
            self.pending_gates(),
        );
        drop(recent);

        for item in spec {
            match item {
                MenuItemSpec::Header(text) => {
                    let mi = MenuItem::new(text, false, None);
                    menu.append(&mi).ok();
                }
                MenuItemSpec::PendingGates { count, target } => {
                    let mi = MenuItem::new(crate::menu::pending_gates_label(count), true, None);
                    ids.insert(mi.id().clone(), TrayCommand::OpenDashboard(target.clone()));
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
                MenuItemSpec::OpenDashboard { target } => {
                    let mi = MenuItem::new("Open Dashboard", true, None);
                    ids.insert(mi.id().clone(), TrayCommand::OpenDashboard(target.clone()));
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

fn handle_command(
    cmd: TrayCommand,
    command_tx: &mpsc::Sender<TrayCommand>,
    config_path: &PathBuf,
    source: Option<&Arc<dyn DataSource>>,
) {
    match &cmd {
        TrayCommand::OpenSettings => match source {
            // A window beats dropping the user into a text editor, and it can
            // still open the file for anything that has to persist.
            #[cfg(target_os = "linux")]
            Some(source) => {
                crate::windows::open_settings_window(source.clone(), config_path.clone())
            }
            _ => {
                if let Err(e) = opener::open(config_path) {
                    tracing::warn!(error = %e, "failed to open settings file");
                }
            }
        },
        TrayCommand::OpenDashboard(target) => match (target, source) {
            #[cfg(target_os = "linux")]
            (DashboardTarget::Window, Some(source)) => {
                crate::windows::open_dashboard_window(source.clone())
            }
            (DashboardTarget::Web(url), _) => {
                if let Err(e) = opener::open_browser(url) {
                    tracing::warn!(error = %e, url = %url, "failed to open dashboard");
                }
            }
            (DashboardTarget::Window, _) => {
                tracing::warn!("dashboard window requested with no data source");
            }
        },
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
