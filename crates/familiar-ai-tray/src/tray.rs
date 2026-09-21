use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tokio::sync::{mpsc, watch};
use tray_icon::TrayIconBuilder;

use familiar_ai_core::config::TrayConfig;
use familiar_ai_core::{AppStatus, FamiliarError, VersionInfo};
use familiar_ai_storage::{Database, ProjectRepository};

use crate::commands::{DashboardTarget, TrayCommand};
use crate::data::DataSource;
use crate::icon::load_tray_icon;
#[cfg(target_os = "linux")]
use crate::icon::tray_icon_with_count;
use crate::menu::{build_tooltip, MenuItemSpec};

/// How often the drawn menu is compared against the state it describes.
///
/// The comparison reads the database, so it runs on its own cadence rather
/// than on every pass of the 100ms event tick. A second is well inside what
/// reads as immediate for a menu the operator has to open to see.
const MENU_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

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

        #[cfg(target_os = "macos")]
        let macos_app = {
            use objc2::MainThreadMarker;
            use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

            let mtm = MainThreadMarker::new().ok_or_else(|| {
                FamiliarError::Config("macOS tray must run on the main thread".into())
            })?;
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            app.finishLaunching();
            app
        };

        let icon = load_tray_icon()?;

        // Build initial menu
        let mut spec = self.menu_spec();
        let (menu, ids) = render_menu(&spec);

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
        let dashboard = self.dashboard.clone();
        let ids_arc = Arc::new(Mutex::new(ids));

        // Shared rather than moved: the badge below and the menu redraw in the
        // event loop are two timers on the same GTK thread, and both need the
        // live handle. Rc is enough — neither ever leaves this thread.
        let tray = Rc::new(tray);

        // PRD-102: the badge and the notification.
        //
        // This runs on the GTK thread because both touch UI — the tray icon
        // and gio's notification service. The cadence is deliberately slow:
        // a gate is a human-scale event and polling it every second would
        // spend the daemon's time telling itself nothing changed.
        #[cfg(target_os = "linux")]
        if let Some(source) = self.source.clone() {
            use crate::notify::{DesktopNotifier, Notifier};
            let tray_for_badge = tray.clone();
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
            let refresh = self.refresher();
            let mut last_refresh = Instant::now();
            glib::source::timeout_add_local(Duration::from_millis(100), move || {
                let mut quitting = false;
                while let Ok(event) = menu_channel.try_recv() {
                    let cmd = ids_for_loop.lock().unwrap().resolve(&event.id);
                    if let Some(cmd) = cmd {
                        // Read before the move: handle_command consumes it.
                        quitting |= matches!(cmd, TrayCommand::Quit);
                        handle_command(
                            cmd,
                            &command_tx_for_loop,
                            &config_path_for_loop,
                            source_for_loop.as_ref(),
                            dashboard.as_ref(),
                        );
                    }
                }
                // Either end of the shutdown can reach us: the user picked Quit
                // from the menu, or the daemon is stopping on its own (SIGTERM).
                // Both have to leave gtk::main(), which is what returns the main
                // thread to the caller so the PID file is removed.
                //
                // Checked before the redraw, never after: the redraw takes the
                // database lock, and making SIGTERM wait behind it is how a
                // shutdown misses its deadline and the supervisor escalates.
                if quitting || *shutdown_rx.borrow() {
                    gtk::main_quit();
                    return glib::ControlFlow::Break;
                }
                // Redraw when the menu would now read differently. Without
                // this the menu is whatever it was at startup for the life of
                // the process: toggling inference changed the daemon and left
                // the item still saying "Enable Local LLM", which is
                // indistinguishable from the click having done nothing.
                if last_refresh.elapsed() >= MENU_REFRESH_INTERVAL {
                    last_refresh = Instant::now();
                    refresh.refresh_if_changed(&tray, &mut spec, &ids_for_loop);
                }
                glib::ControlFlow::Continue
            });
            gtk::main();
        }

        #[cfg(not(target_os = "linux"))]
        {
            // macOS needs its AppKit event queue pumped on this main thread.
            // A plain sleeping Rust loop leaves the NSStatusItem constructed
            // but never presented, which looks exactly like a missing tray.
            let refresh = self.refresher();
            let mut last_refresh = Instant::now();
            loop {
                #[cfg(target_os = "macos")]
                {
                    use objc2_app_kit::NSEventMask;
                    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

                    let now = NSDate::distantPast();
                    // Imported Objective-C constants are extern statics; the
                    // framework guarantees this one is initialized for the
                    // process lifetime.
                    let mode = unsafe { NSDefaultRunLoopMode };
                    while let Some(event) = macos_app
                        .nextEventMatchingMask_untilDate_inMode_dequeue(
                            NSEventMask::Any,
                            Some(&now),
                            mode,
                            true,
                        )
                    {
                        macos_app.sendEvent(&event);
                    }
                    macos_app.updateWindows();
                }
                if let Ok(event) = menu_channel.try_recv() {
                    let cmd = ids_arc.lock().unwrap().resolve(&event.id);
                    if let Some(cmd) = cmd {
                        let should_quit = matches!(cmd, TrayCommand::Quit);
                        handle_command(
                            cmd,
                            &command_tx,
                            &config_path,
                            self.source.as_ref(),
                            dashboard.as_ref(),
                        );
                        if should_quit {
                            break;
                        }
                    }
                }
                if *self.shutdown_rx.borrow() {
                    break;
                }
                if last_refresh.elapsed() >= MENU_REFRESH_INTERVAL {
                    last_refresh = Instant::now();
                    refresh.refresh_if_changed(&tray, &mut spec, &ids_arc);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        Ok(())
    }

    /// The menu as it should read right now.
    fn menu_spec(&self) -> Vec<MenuItemSpec> {
        let status = self.status.lock().unwrap().clone();
        let recent = self
            .db
            .lock()
            .unwrap()
            .list_active_projects()
            .unwrap_or_default();
        crate::menu::build_menu_spec(
            &status,
            &recent,
            self.config.recent_projects_count,
            self.dashboard.clone(),
            self.pending_gates(),
        )
    }

    /// The collaborators the redraw needs, owned, so it can live inside the
    /// event loop's closure after `self` has been consumed.
    fn refresher(&self) -> MenuRefresher {
        MenuRefresher {
            status: self.status.clone(),
            db: self.db.clone(),
            recent_projects_count: self.config.recent_projects_count,
            dashboard: self.dashboard.clone(),
            pending_gates: self.pending_gates.clone(),
        }
    }
}

/// Keeps the drawn menu in step with the state it describes.
struct MenuRefresher {
    status: Arc<Mutex<AppStatus>>,
    db: Arc<Mutex<Database>>,
    recent_projects_count: usize,
    dashboard: Option<DashboardTarget>,
    /// PRD-102. Shared with the badge poll, so the signpost's count and the
    /// number drawn on the icon are the same number rather than two reads of
    /// the same table a few seconds apart.
    pending_gates: Arc<AtomicUsize>,
}

impl MenuRefresher {
    /// Rebuilds the tray's menu when its contents would differ from what is
    /// drawn, and leaves it alone otherwise.
    ///
    /// The comparison is over the spec rather than a hand-picked set of
    /// fields, so a new menu item cannot be added without its changes also
    /// triggering a redraw. Rebuilding unconditionally would work too, but it
    /// replaces the menu ten times a second, which closes it under the
    /// pointer while someone is reading it.
    fn refresh_if_changed(
        &self,
        tray: &tray_icon::TrayIcon,
        drawn: &mut Vec<MenuItemSpec>,
        ids: &Arc<Mutex<MenuIdMap>>,
    ) {
        let status = match self.status.lock() {
            Ok(status) => status.clone(),
            Err(_) => return,
        };
        let recent = match self.db.lock() {
            Ok(db) => db.list_active_projects().unwrap_or_default(),
            Err(_) => return,
        };
        let spec = crate::menu::build_menu_spec(
            &status,
            &recent,
            self.recent_projects_count,
            self.dashboard.clone(),
            self.pending_gates.load(Ordering::Relaxed),
        );
        if spec == *drawn {
            return;
        }
        let (menu, new_ids) = render_menu(&spec);
        tray.set_menu(Some(Box::new(menu)));
        if let Err(e) = tray.set_tooltip(Some(build_tooltip(&status))) {
            tracing::warn!(error = %e, "failed to update tray tooltip");
        }
        *ids.lock().unwrap() = new_ids;
        *drawn = spec;
    }
}

/// Turns the logical menu into muda widgets, and records which widget id maps
/// to which command.
fn render_menu(spec: &[MenuItemSpec]) -> (Menu, MenuIdMap) {
    let menu = Menu::new();
    let mut ids = MenuIdMap::new();
    {
        let spec = spec.to_vec();
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
                MenuItemSpec::Llm(state) => {
                    let mi = MenuItem::new(state.label(), true, None);
                    ids.insert(mi.id().clone(), state.command());
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
    }

    // Suppress unused warning for Submenu import
    let _ = std::marker::PhantomData::<Submenu>;

    (menu, ids)
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
    dashboard: Option<&DashboardTarget>,
) {
    match &cmd {
        // Straight to the inference page rather than the window's first tab:
        // the operator clicked an item about the LLM, and making them find
        // the right tab is the same dead end as the item that did nothing.
        TrayCommand::ConfigureLlm => match (source, dashboard) {
            #[cfg(target_os = "linux")]
            (Some(source), _) => {
                crate::windows::open_inference_settings_window(source.clone(), config_path.clone())
            }
            (_, Some(DashboardTarget::Web(url))) => {
                let url = format!("{}/settings/inference", url.trim_end_matches('/'));
                if let Err(e) = opener::open_browser(&url) {
                    tracing::warn!(error = %e, url = %url, "failed to open inference settings");
                }
            }
            _ => {
                if let Err(e) = opener::open(config_path) {
                    tracing::warn!(error = %e, "failed to open settings file");
                }
            }
        },
        TrayCommand::OpenSettings => match (source, dashboard) {
            // A window beats dropping the user into a text editor, and it can
            // still open the file for anything that has to persist.
            #[cfg(target_os = "linux")]
            (Some(source), _) => {
                crate::windows::open_settings_window(source.clone(), config_path.clone())
            }
            (_, Some(DashboardTarget::Web(url))) => {
                let url = format!("{}/settings/inference", url.trim_end_matches('/'));
                if let Err(e) = opener::open_browser(&url) {
                    tracing::warn!(error = %e, url = %url, "failed to open settings UI");
                }
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
