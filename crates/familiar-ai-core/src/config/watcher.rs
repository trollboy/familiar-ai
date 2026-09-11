use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    #[serde(default = "default_watcher_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_ignore_patterns")]
    pub ignore_patterns: Vec<String>,
    #[serde(default = "default_respect_gitignore")]
    pub respect_gitignore: bool,
}

fn default_watcher_enabled() -> bool {
    true
}

fn default_debounce_ms() -> u64 {
    1000
}

fn default_ignore_patterns() -> Vec<String> {
    vec!["target/**".into(), "node_modules/**".into()]
}

fn default_respect_gitignore() -> bool {
    true
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            enabled: default_watcher_enabled(),
            paths: Vec::new(),
            debounce_ms: default_debounce_ms(),
            ignore_patterns: default_ignore_patterns(),
            respect_gitignore: default_respect_gitignore(),
        }
    }
}
