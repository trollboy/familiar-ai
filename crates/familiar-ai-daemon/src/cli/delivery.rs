//! `familiar-ai deliver` / `report` — publish, check, merge, deploy, and
//! smoke-test one reviewed worktree, and render a session report.

use super::database;

pub fn deliver_command(ownership_record: &std::path::Path, to: Option<&str>) -> Result<(), String> {
    match crate::delivery::execute_configured(ownership_record, to)? {
        crate::delivery::ConfiguredDeliveryOutcome::Environment {
            session_id,
            prd_id,
            role,
            target,
            revision,
        } => println!(
            "delivery_session={session_id} prd={prd_id} role={role} target={target} revision={revision} smoke=passed"
        ),
        crate::delivery::ConfiguredDeliveryOutcome::Standard(result) => println!(
            "delivery_session={} prd={} phase={} pr={}",
            result.session_id,
            result.prd_id,
            result.phase,
            result
                .pr_number
                .map(|number| number.to_string())
                .unwrap_or_else(|| "unknown".into())
        ),
    }
    Ok(())
}

/// Read-only: renders recorded rows and constructs no agents.
pub fn report_command(session_id: Option<&str>) -> Result<(), String> {
    let db = database()?;
    let rendered = crate::report::render(&db, session_id).map_err(|e| e.to_string())?;
    print!("{rendered}");
    Ok(())
}
