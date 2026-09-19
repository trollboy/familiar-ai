use serde::{Deserialize, Serialize};

/// The unattended driver's budget warrant. Every ceiling is optional
/// individually (0 means unlimited), but `drive` refuses to start unless at
/// least one is finite: an unbounded unattended loop is not a warrant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriverConfig {
    #[serde(default)]
    pub max_prds_per_session: u64,
    #[serde(default)]
    pub max_session_cost_microusd: u64,
    #[serde(default)]
    pub max_session_tokens: u64,
    #[serde(default)]
    pub max_session_duration_ms: u64,
    #[serde(default = "default_driver_concurrency")]
    pub max_concurrency: usize,
    #[serde(default)]
    pub isolated_worktrees: bool,
    /// Maximum number of independent dependency components that may execute.
    /// One preserves the original serial driver and primary worktree exactly.
    #[serde(default = "default_driver_concurrency")]
    pub max_parallel_components: usize,
    /// Optional worktree parent. Empty uses the driver-owned state directory.
    #[serde(default)]
    pub worktree_root: String,
    /// Removed legacy implementation routes. Retained only so stale
    /// configuration can fail with an actionable replacement path.
    #[serde(default)]
    pub model_routes: Vec<DriverModelRouteConfig>,
    /// Finite implementation-stage token ceiling. Zero disables this ceiling.
    #[serde(default)]
    pub max_implementation_tokens: u64,
    /// PRD-085: the minimum percentage (0-100) of a session's completed
    /// PRDs that must finish with no human intervention before the session
    /// reports itself as an autonomy failure. An integer percentage rather
    /// than a fraction so this field stays exactly comparable, like every
    /// other ceiling in this struct. Zero (the default) disables the check
    /// — the floor is an opt-in target, not an assumed one.
    #[serde(default)]
    pub min_unattended_percent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DriverModelRouteConfig {
    pub max_expected_files: usize,
    pub model: String,
}

fn default_driver_concurrency() -> usize {
    1
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            max_prds_per_session: 0,
            max_session_cost_microusd: 0,
            max_session_tokens: 0,
            max_session_duration_ms: 0,
            max_concurrency: default_driver_concurrency(),
            isolated_worktrees: false,
            max_parallel_components: default_driver_concurrency(),
            worktree_root: String::new(),
            model_routes: Vec::new(),
            max_implementation_tokens: 0,
            min_unattended_percent: 0,
        }
    }
}

impl DriverConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_prds_per_session == 0
            && self.max_session_cost_microusd == 0
            && self.max_session_tokens == 0
            && self.max_session_duration_ms == 0
        {
            return Err(
                "unattended drive requires at least one finite ceiling in [driver]: \
                 max_prds_per_session, max_session_cost_microusd, max_session_tokens, or max_session_duration_ms"
                    .into(),
            );
        }
        if self.max_concurrency == 0 {
            return Err("driver.max_concurrency must be positive".into());
        }
        if self.max_parallel_components == 0 {
            return Err("driver.max_parallel_components must be positive".into());
        }
        if self.max_concurrency > 1 && !self.isolated_worktrees {
            return Err(
                "driver.isolated_worktrees must be true when max_concurrency is greater than one"
                    .into(),
            );
        }
        if !self.model_routes.is_empty() {
            return Err(
                "driver.model_routes has been removed; configure worker_registry.routing.rules instead"
                    .into(),
            );
        }
        if self.min_unattended_percent > 100 {
            return Err("driver.min_unattended_percent must be between 0 and 100".into());
        }
        Ok(())
    }
}
