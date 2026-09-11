use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: Option<PathBuf>,
}

impl DatabaseConfig {
    pub fn resolve_path(&self, data_dir: &Path) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| data_dir.join("familiar.db"))
    }
}

// --- Inference config (replaces old LlmConfig) ---
