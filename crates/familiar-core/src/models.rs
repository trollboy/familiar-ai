use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- Project ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub repo_root: String,
    pub active: bool,
    pub last_used_at: DateTime<Utc>,
    pub ignored_paths: Vec<String>,
    pub token_budget: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewProject {
    pub name: String,
    pub repo_root: String,
    pub ignored_paths: Vec<String>,
    pub token_budget: Option<i64>,
}

// --- FileSummary ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSummary {
    pub id: i64,
    pub project_id: i64,
    pub path: String,
    pub summary: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub extracted_symbols: Vec<String>,
    #[serde(default)]
    pub last_known_mtime: Option<i64>,
    #[serde(default)]
    pub last_known_size: Option<i64>,
    pub last_updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewFileSummary {
    pub project_id: i64,
    pub path: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub extracted_symbols: Vec<String>,
    pub last_known_mtime: Option<i64>,
    pub last_known_size: Option<i64>,
}

// --- Decision ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub id: i64,
    pub project_id: i64,
    pub title: String,
    pub summary: String,
    pub related_files: Vec<String>,
    pub source_session: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewDecision {
    pub project_id: i64,
    pub title: String,
    pub summary: String,
    pub related_files: Vec<String>,
    pub source_session: Option<String>,
    pub confidence: Option<String>,
}

// --- SessionRollup ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRollup {
    pub id: i64,
    pub project_id: i64,
    pub summary: String,
    pub related_files: Vec<String>,
    pub next_steps: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewSessionRollup {
    pub project_id: i64,
    pub summary: String,
    pub related_files: Vec<String>,
    pub next_steps: Vec<String>,
}
