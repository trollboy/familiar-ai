use familiar_ai_core::models::Project;
use familiar_ai_core::AppStatus;

/// Logical menu item describing what should appear in the tray menu.
/// This is a pure data structure, fully testable without any GUI.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuItemSpec {
    Header(String),
    Separator,
    LlmToggle { enabled: bool },
    PauseToggle { paused: bool },
    RecentProject { name: String, repo_root: String },
    RecentProjectsHeader,
    EmptyRecentProjects,
    OpenSettings,
    About,
    Quit,
}

/// Build the logical menu structure from current state.
pub fn build_menu_spec(
    status: &AppStatus,
    recent_projects: &[Project],
    recent_count: usize,
) -> Vec<MenuItemSpec> {
    let mut items = Vec::new();

    // Status header
    items.push(MenuItemSpec::Header(format!(
        "Familiar — {} active project(s)",
        status.active_projects
    )));
    items.push(MenuItemSpec::Header(format!(
        "LLM: {} | MCP: {}",
        if status.local_llm_enabled {
            "on"
        } else {
            "off"
        },
        if status.mcp_enabled { "on" } else { "off" }
    )));
    items.push(MenuItemSpec::Separator);

    // LLM toggle
    items.push(MenuItemSpec::LlmToggle {
        enabled: status.local_llm_enabled,
    });

    // Pause toggle (paused state not currently tracked in AppStatus, default false)
    items.push(MenuItemSpec::PauseToggle { paused: false });

    items.push(MenuItemSpec::Separator);

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
        if status.local_llm_enabled {
            "enabled"
        } else {
            "disabled"
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

    fn make_status(active: usize, llm: bool, mcp: bool) -> AppStatus {
        let mut s = AppStatus::new();
        s.active_projects = active;
        s.local_llm_enabled = llm;
        s.mcp_enabled = mcp;
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
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5);
        // header(2) + sep + llm + pause + sep + recent_header + empty + sep + settings + about + sep + quit = 13
        assert_eq!(items.len(), 13);
        assert!(matches!(items[0], MenuItemSpec::Header(_)));
        assert!(matches!(
            items[3],
            MenuItemSpec::LlmToggle { enabled: false }
        ));
        assert!(items.contains(&MenuItemSpec::EmptyRecentProjects));
        assert!(items.contains(&MenuItemSpec::Quit));
    }

    #[test]
    fn llm_enabled_reflected() {
        let status = make_status(2, true, false);
        let items = build_menu_spec(&status, &[], 5);
        assert!(matches!(
            items[3],
            MenuItemSpec::LlmToggle { enabled: true }
        ));
    }

    #[test]
    fn recent_projects_listed() {
        let status = make_status(3, false, false);
        let projects = vec![
            make_project(1, "alpha", "/a"),
            make_project(2, "beta", "/b"),
            make_project(3, "gamma", "/c"),
        ];
        let items = build_menu_spec(&status, &projects, 5);
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
        let items = build_menu_spec(&status, &projects, 5);
        let recent_count = items
            .iter()
            .filter(|i| matches!(i, MenuItemSpec::RecentProject { .. }))
            .count();
        assert_eq!(recent_count, 5);
    }

    #[test]
    fn no_recent_projects_shows_empty() {
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5);
        assert!(items.contains(&MenuItemSpec::EmptyRecentProjects));
        assert!(!items
            .iter()
            .any(|i| matches!(i, MenuItemSpec::RecentProject { .. })));
    }

    #[test]
    fn header_shows_project_count() {
        let status = make_status(7, true, true);
        let items = build_menu_spec(&status, &[], 5);
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
        let items = build_menu_spec(&status, &[], 5);
        assert!(items.contains(&MenuItemSpec::Quit));
    }

    #[test]
    fn always_includes_settings_and_about() {
        let status = make_status(0, false, false);
        let items = build_menu_spec(&status, &[], 5);
        assert!(items.contains(&MenuItemSpec::OpenSettings));
        assert!(items.contains(&MenuItemSpec::About));
    }
}
