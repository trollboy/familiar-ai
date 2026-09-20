use std::path::PathBuf;

/// Where the dashboard lives for this build. A native window is preferred —
/// it needs no HTTP listener and no browser — and the web page is the fallback
/// on platforms without GTK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashboardTarget {
    /// A window in this process.
    Window,
    /// A page served by the daemon's dashboard listener.
    Web(String),
}

#[derive(Debug, Clone)]
pub enum TrayCommand {
    EnableLlm,
    DisableLlm,
    /// Opens the screen that sets inference up. Offered in place of the
    /// enable/disable toggle while nothing is configured, because there is
    /// nothing for that toggle to act on yet.
    ConfigureLlm,
    PauseHeavyTasks,
    ResumeHeavyTasks,
    OpenSettings,
    /// Opens the dashboard. Carries how to reach it, because that differs by
    /// platform: a native window where GTK is available, the web page
    /// otherwise.
    OpenDashboard(DashboardTarget),
    OpenProject(PathBuf),
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_construct() {
        let _ = TrayCommand::EnableLlm;
        let _ = TrayCommand::DisableLlm;
        let _ = TrayCommand::ConfigureLlm;
        let _ = TrayCommand::PauseHeavyTasks;
        let _ = TrayCommand::ResumeHeavyTasks;
        let _ = TrayCommand::OpenSettings;
        let _ = TrayCommand::OpenDashboard(DashboardTarget::Window);
        let _ =
            TrayCommand::OpenDashboard(DashboardTarget::Web("http://127.0.0.1:9400".to_string()));
        let _ = TrayCommand::OpenProject(PathBuf::from("/test"));
        let _ = TrayCommand::Quit;
    }

    #[test]
    fn debug_format() {
        let cmd = TrayCommand::OpenProject(PathBuf::from("/test"));
        let s = format!("{cmd:?}");
        assert!(s.contains("OpenProject"));
    }
}
