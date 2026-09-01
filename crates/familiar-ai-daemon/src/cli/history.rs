//! `familiar-ai history` — list recent standalone executions.

use familiar_ai_storage::ExecutionHistoryRepository;

use super::shared::database;

pub fn history(limit: u8, verbose: bool) -> Result<(), String> {
    let db = database()?;
    let rows = ExecutionHistoryRepository::new(db.conn())
        .recent(limit)
        .map_err(|e| e.to_string())?;
    if rows.is_empty() {
        println!("No execution history.");
        return Ok(());
    }
    for row in rows {
        let duration = row
            .duration_ms
            .map(|v| format!("{v}ms"))
            .unwrap_or_else(|| "—".into());
        let model = row.model.as_deref().unwrap_or("—");
        let status = match (row.exit_code, row.signal) {
            (Some(code), _) => format!("{} ({code})", row.outcome),
            (None, Some(signal)) => format!("{} (signal {signal})", row.outcome),
            _ if row.outcome == "running" => "running/incomplete".into(),
            _ => row.outcome.clone(),
        };
        println!(
            "{}  {}  {}  {}  {}  {}  {}  {}  {}",
            row.execution_id,
            row.started_at,
            duration,
            row.agent,
            model,
            status,
            row.repository,
            row.worktree,
            row.prd_path
        );
        if verbose {
            for (field, reason) in row.unavailable_fields {
                println!("  {field}: — ({reason})");
            }
        }
    }
    Ok(())
}
