use familiar_ai_core::models::Project;
use familiar_ai_core::AppStatus;

use crate::commands::DashboardTarget;

/// Logical menu item describing what should appear in the tray menu.
/// This is a pure data structure, fully testable without any GUI.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuItemSpec {
    Header(String),
    Separator,
    Llm(LlmMenuState),
    PauseToggle {
        paused: bool,
    },
    RecentProject {
        name: String,
        repo_root: String,
    },
    RecentProjectsHeader,
    EmptyRecentProjects,
    OpenSettings,
    /// Only present when the dashboard is actually reachable: an item that
    /// opens a port nothing is listening on is worse than no item.
    OpenDashboard {
        target: DashboardTarget,
    },
    About,
    Quit,
}

/// What the inference item offers, which is not a two-way toggle: a daemon
/// with no backend in its config has nothing to enable, so it is offered a way
/// to configure one instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmMenuState {
    /// Nothing is configured. The item leads to the settings screen.
    Unconfigured,
    /// Configured and loaded. The item turns it off.
    Enabled,
    /// Configured but not loaded. The item turns it on.
    Disabled,
}

impl LlmMenuState {
    pub fn of(status: &AppStatus) -> Self {
        match (status.local_llm_configured, status.local_llm_enabled) {
            (false, _) => Self::Unconfigured,
            (true, true) => Self::Enabled,
            (true, false) => Self::Disabled,
        }
    }

    /// The label as it appears in the menu.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unconfigured => "Configure Local LLM…",
            Self::Enabled => "Disable Local LLM",
            Self::Disabled => "Enable Local LLM",
        }
    }

    pub fn command(&self) -> crate::commands::TrayCommand {
        match self {
            Self::Unconfigured => crate::commands::TrayCommand::ConfigureLlm,
            Self::Enabled => crate::commands::TrayCommand::DisableLlm,
            Self::Disabled => crate::commands::TrayCommand::EnableLlm,
        }
    }
}

/// Build the logical menu structure from current state.
pub fn build_menu_spec(
    status: &AppStatus,
    recent_projects: &[Project],
    recent_count: usize,
    dashboard: Option<DashboardTarget>,
) -> Vec<MenuItemSpec> {
    let mut items = Vec::new();

    // Status header
    items.push(MenuItemSpec::Header(format!(
        "Familiar — {} active project(s)",
        status.active_projects
    )));
    items.push(MenuItemSpec::Header(format!(
        "LLM: {} | MCP: {}",
        match LlmMenuState::of(status) {
            LlmMenuState::Unconfigured => "not configured",
            LlmMenuState::Enabled => "on",
            LlmMenuState::Disabled => "off",
        },
        if status.mcp_enabled { "on" } else { "off" }
    )));
    items.push(MenuItemSpec::Separator);

    items.push(MenuItemSpec::Llm(LlmMenuState::of(status)));

    // Pause toggle (paused state not currently tracked in AppStatus, default false)
    items.push(MenuItemSpec::PauseToggle { paused: false });

    items.push(MenuItemSpec::Separator);

    // Dashboard — planned in vision.md 4.2 and missing until now, which left
    // the dashboard and settings pages with no route in from the tray.
    if let Some(target) = dashboard {
        items.push(MenuItemSpec::OpenDashboard { target });
        items.push(MenuItemSpec::Separator);
    }

    // Recent projects
    items.push(MenuItemSpec::RecentProjectsHeader);
    let to_show = recent_projects.iter().take(recent_count);
    let count = to_show.clone().count();
    if count == 0 {
        items.push(MenuItemSpec::EmptyRecentProjects);
    } else {
        for project in to_show {
            items.push(MenuItemSpec::RecentProject {
                name: project.name.clone(),
                repo_root: project.repo_root.clone(),
            });
        }
    }

    items.push(MenuItemSpec::Separator);
    items.push(MenuItemSpec::OpenSettings);
    items.push(MenuItemSpec::About);
    items.push(MenuItemSpec::Separator);
    items.push(MenuItemSpec::Quit);

    items
}

/// Build a tooltip string from current status.
pub fn build_tooltip(status: &AppStatus) -> String {
    format!(
        "Familiar\nActive projects: {}\nLLM: {}\nMCP: {}",
        status.active_projects,
        match LlmMenuState::of(status) {
            LlmMenuState::Unconfigured => "not configured",
            LlmMenuState::Enabled => "enabled",
            LlmMenuState::Disabled => "disabled",
        },
        if status.mcp_enabled {
            "enabled"
        } else {
            "disabled"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use familiar_ai_core::models::Project;

    /// A status whose inference is configured, and loaded when `llm`. An
    /// enabled backend is a configured one by definition; the unconfigured
    /// case is built by `make_unconfigured` because it is a third state, not
    /// the `false` end of this one.
    fn make_status(active: usize, llm: bool, mcp: bool) -> AppStatus {
        let mut s = AppStatus::new();
        s.active_projects = active;
        s.local_llm_enabled = llm;
        s.local_llm_configured = true;
        s.mcp_enabled = mcp;
        s
    }

    fn make_unconfigured(active: usize) -> AppStatus {
        let mut s = AppStatus::new();
        s.active_projects = active;
        s.local_llm_enabled = false;
        s.local_llm_configured = false;
        s
    }

    fn make_project(id: i64, name: &str, repo: &str) -> Project {
        let now = Utc::now();
        Project {
            id,
            name: name.into(),
            repo_root: repo.into(),
            active: true,
            last_used_at: now,
            ignored_paths: vec![],
            token_budget: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn empty_state_menu() {
        let status = make_unconfigured(0);
        let items = build_menu_spec(&status, &[], 5, None);
        // header(2) + sep + llm + pause + sep + recent_header + empty + sep + settings + about + sep + quit = 13
        assert_eq!(items.len(), 13);
        assert!(matches!(items[0], MenuItemSpec::Header(_)));
        assert_eq!(items[3], MenuItemSpec::Llm(LlmMenuState::Unconfigured));
        assert!(items.contains(&MenuItemSpec::EmptyRecentProjects));
        assert!(items.contains(&MenuItemSpec::Quit));
    }

    #[test]
    fn llm_enabled_reflected() {
        let status = make_status(2, true, false);
        let items = build_menu_spec(&status, &[], 5, None);
        assert_eq!(items[3], MenuItemSpec::Llm(LlmMenuState::Enabled));
    }

    /// The three states are distinct in label and in what clicking does. An
    /// unconfigured daemon must not be offered "Enable Local LLM": there is
    /// no backend for it to load, so the click can only appear to do nothing.
    #[test]
    fn unconfigured_offers_configuration_not_a_toggle() {
        let items = build_menu_spec(&make_unconfigured(0), &[], 5, None);
        assert_eq!(items[3], MenuItemSpec::Llm(LlmMenuState::Unconfigured));
        assert_eq!(LlmMenuState::Unconfigured.label(), "Configure Local LLM…");
        assert!(matches!(
            LlmMenuState::Unconfigured.command(),
            crate::commands::TrayCommand::ConfigureLlm
        ));
    }

    #[test]
    fn configured_but_unloaded_offers_enable() {
        let items = build_menu_spec(&make_status(0, false, false), &[], 5, None);
        assert_eq!(items[3], MenuItemSpec::Llm(LlmMenuState::Disabled));
        assert_eq!(LlmMenuState::Disabled.label(), "Enable Local LLM");
        assert!(matches!(
            LlmMenuState::Disabled.command(),
            crate::commands::TrayCommand::EnableLlm
        ));
    }

    #[test]
    fn enabled_offers_disable() {
        assert_eq!(LlmMenuState::Enabled.label(), "Disable Local LLM");
        assert!(matches!(
            LlmMenuState::Enabled.command(),
            crate::commands::TrayCommand::DisableLlm
        ));
    }

    /// The header and tooltip have to tell "off" apart from "never set up",
    /// because the remedy for each is a different click.
    #[test]
    fn unconfigured_reads_differently_from_off() {
        let unconfigured = build_tooltip(&make_unconfigured(0));
        let off = build_tooltip(&make_status(0, false, false));
        assert!(
            unconfigured.contains("LLM: not configured"),
            "{unconfigured}"
        );
        assert!(off.contains("LLM: disabled"), "{off}");
    }

    #[test]
    fn recent_projects_listed() {
        let status = make_status(3, false, false);
        let projects = vec![
            make_project(1, "alpha", "/a"),
            make_project(2, "beta", "/b"),
            make_project(3, "gamma", "/c"),
        ];
        let items = build_menu_spec(&status, &projects, 5, None);
        let recent_count = items
            .iter()
            .filter(|i| matches!(i, MenuItemSpec::RecentProject { .. }))
            .count();
        assert_eq!(recent_count, 3);
    }

    #[test]
    fn recent_projects_capped() {
        let status = make_status(10, false, false);
        let projects: Vec<_> = (0..10)
            .map(|i| make_project(i, &format!("p{i}"), &format!("/p{i}")))
            .collect();
        let items = build_menu_spec(&status, &projects, 5, None);
        let recent_count = items
            .iter()
            .filter(|i| matches!(i, MenuItemSpec::RecentProject { .. }))
            .count();
        assert_eq!(recent_count, 5);
    }

    #[test]
    fn no_recent_projects_shows_empty() {
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5, None);
        assert!(items.contains(&MenuItemSpec::EmptyRecentProjects));
        assert!(!items
            .iter()
            .any(|i| matches!(i, MenuItemSpec::RecentProject { .. })));
    }

    #[test]
    fn header_shows_project_count() {
        let status = make_status(7, true, true);
        let items = build_menu_spec(&status, &[], 5, None);
        let header = match &items[0] {
            MenuItemSpec::Header(s) => s.clone(),
            _ => panic!("expected header"),
        };
        assert!(header.contains("7"));
    }

    #[test]
    fn tooltip_includes_status() {
        let status = make_status(3, true, false);
        let tooltip = build_tooltip(&status);
        assert!(tooltip.contains("3"));
        assert!(tooltip.contains("enabled"));
        assert!(tooltip.contains("disabled"));
    }

    #[test]
    fn always_includes_quit() {
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5, None);
        assert!(items.contains(&MenuItemSpec::Quit));
    }

    #[test]
    fn always_includes_settings_and_about() {
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5, None);
        assert!(items.contains(&MenuItemSpec::OpenSettings));
        assert!(items.contains(&MenuItemSpec::About));
    }

    #[test]
    fn dashboard_item_present_when_enabled() {
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5, Some(DashboardTarget::Window));
        assert!(items.contains(&MenuItemSpec::OpenDashboard {
            target: DashboardTarget::Window,
        }));

        let items = build_menu_spec(
            &status,
            &[],
            5,
            Some(DashboardTarget::Web("http://127.0.0.1:9400".to_string())),
        );
        assert!(items.contains(&MenuItemSpec::OpenDashboard {
            target: DashboardTarget::Web("http://127.0.0.1:9400".to_string()),
        }));
    }

    /// An item that opens a port nothing is listening on is worse than no
    /// item, so a disabled dashboard must leave the menu exactly as it was.
    #[test]
    fn dashboard_item_absent_when_disabled() {
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5, None);
        assert!(!items
            .iter()
            .any(|i| matches!(i, MenuItemSpec::OpenDashboard { .. })));
        assert_eq!(items.len(), 13);
    }
}
