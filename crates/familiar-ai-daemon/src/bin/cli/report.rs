use super::common::database;

/// Read-only: renders recorded rows and constructs no agents.
pub fn report_command(session_id: Option<&str>) -> Result<(), String> {
    let db = database()?;
    let rendered =
        familiar_ai_daemon::report::render(&db, session_id).map_err(|e| e.to_string())?;
    print!("{rendered}");
    Ok(())
}
