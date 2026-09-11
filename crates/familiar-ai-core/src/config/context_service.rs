use std::path::Path;

use serde::{Deserialize, Serialize};

use super::ReviewConfig;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContextConfig {
    pub hard_ceiling_tokens: Option<u64>,
    #[serde(default)]
    pub repository_map_enabled: bool,
    /// Enables adapter-specific prompt cache controls. Unsupported adapters
    /// retain their native behavior and receive no fabricated flags.
    #[serde(default = "default_prompt_cache_enabled")]
    pub prompt_cache_enabled: bool,
}

const fn default_prompt_cache_enabled() -> bool {
    true
}

impl Default for ExecutionContextConfig {
    fn default() -> Self {
        Self {
            hard_ceiling_tokens: None,
            repository_map_enabled: false,
            prompt_cache_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigurationSource {
    Global,
    Repository,
}

impl ConfigurationSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Repository => "repository",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveExecutionConfig {
    pub review: ReviewConfig,
    pub review_source: ConfigurationSource,
    pub execution_context: ExecutionContextConfig,
    pub execution_context_source: ConfigurationSource,
}

/// The canonical Git common-directory identity of a worktree — the same key
/// `FilesystemBacklogDiscovery::resolve` computes — or None when the path is
/// not inside a Git repository (or git is unavailable). PRD-087: this used
/// to be an independent computation kept in sync with the backlog copy only
/// by comment; it now delegates to the single minting site.
pub(super) fn git_common_directory(path: &Path) -> Option<String> {
    crate::repository_path::git_common_directory(path)
}
