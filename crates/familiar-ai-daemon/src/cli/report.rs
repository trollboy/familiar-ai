//! `familiar-ai report` — render one unattended driver session.

use familiar_ai_core::AppPaths;

use super::shared::{database, effective_repository_config};

/// Read-only: renders recorded rows and constructs no agents.
pub fn report_command(session_id: Option<&str>) -> Result<(), String> {
    let db = database()?;
    // The autonomy floor is a per-repository config knob (PRD-085); a
    // repository this command is not run from still renders a report, just
    // without a configured floor to compare against.
    let min_unattended_percent = AppPaths::resolve()
        .ok()
        .and_then(|paths| {
            let cwd = std::env::current_dir().ok()?;
            effective_repository_config(&paths, &cwd).ok()
        })
        .map(|config| config.driver.min_unattended_percent)
        .unwrap_or(0);
    let rendered = crate::report::render(&db, session_id, min_unattended_percent)
        .map_err(|e| e.to_string())?;
    print!("{rendered}");
    Ok(())
}
