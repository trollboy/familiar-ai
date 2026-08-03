pub mod create_decision;
pub mod get_file_summary;
pub mod get_module_summary;
pub mod get_project_status;
pub mod get_recent_changes;
pub mod get_recent_decisions;
pub mod get_session_rollups;
pub mod keywords;
pub mod pack_for_task;
pub mod remember_result;
pub mod scoring;
pub mod search;
pub mod session_rollup_query;

#[cfg(test)]
pub mod test_helpers;

use std::sync::Arc;

use crate::tool::ToolRegistry;

/// Register all default tools into the registry.
pub fn register_default_tools(registry: &mut ToolRegistry) {
    registry.register(Arc::new(get_project_status::GetProjectStatusTool));
    registry.register(Arc::new(get_recent_decisions::GetRecentDecisionsTool));
    registry.register(Arc::new(remember_result::RememberResultTool));
    registry.register(Arc::new(create_decision::CreateDecisionTool));
    registry.register(Arc::new(search::SearchTool));
    registry.register(Arc::new(pack_for_task::PackForTaskTool));
    registry.register(Arc::new(get_file_summary::GetFileSummaryTool));
    registry.register(Arc::new(get_module_summary::GetModuleSummaryTool));
    registry.register(Arc::new(get_recent_changes::GetRecentChangesTool));
    registry.register(Arc::new(get_session_rollups::GetSessionRollupsTool));
}
