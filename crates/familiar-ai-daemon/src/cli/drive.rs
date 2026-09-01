//! `familiar-ai drive` — execute eligible backlog PRDs unattended until the
//! backlog is empty, nothing is eligible, or the budget warrant is
//! exhausted.

use std::path::PathBuf;

use familiar_ai_core::AppPaths;

use crate::drive::DriveSummary;

pub fn drive_command(
    max_prds: Option<u64>,
    max_cost_microusd: Option<u64>,
    max_duration_ms: Option<u64>,
    max_parallel_components: Option<usize>,
    worktree_root: Option<PathBuf>,
    prd_flags: Vec<String>,
) -> Result<DriveSummary, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let summary = crate::drive::execute_configured(
        &paths,
        &current,
        max_prds,
        max_cost_microusd,
        max_duration_ms,
        max_parallel_components,
        worktree_root.as_deref(),
        &prd_flags,
    )?;
    println!(
        "session={} termination={} attempted={} completed={} known_cost_microusd={}",
        summary.session_id,
        summary.termination.as_str(),
        summary.attempted,
        summary.completed,
        summary.known_cost_microusd
    );
    Ok(summary)
}
