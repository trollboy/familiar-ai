//! `familiar-ai report` — render one unattended driver session.

use super::shared::database;

/// Read-only: renders recorded rows and constructs no agents.
pub fn report_command(session_id: Option<&str>) -> Result<(), String> {
    let db = database()?;
    let rendered = crate::report::render(&db, session_id).map_err(|e| e.to_string())?;
    print!("{rendered}");
    Ok(())
}
