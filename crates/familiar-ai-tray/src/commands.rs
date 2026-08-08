use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum TrayCommand {
    EnableLlm,
    DisableLlm,
    PauseHeavyTasks,
    ResumeHeavyTasks,
    OpenSettings,
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
        let _ = TrayCommand::PauseHeavyTasks;
        let _ = TrayCommand::ResumeHeavyTasks;
        let _ = TrayCommand::OpenSettings;
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
