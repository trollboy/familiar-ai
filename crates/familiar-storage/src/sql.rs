//! Centralized SQL query strings for all repositories.
//!
//! Putting every query here makes schema churn manageable: when a column
//! changes, you find every affected query in one file instead of grepping
//! through repos.

// --- projects ---

pub(crate) const INSERT_PROJECT: &str = "INSERT INTO projects \
    (name, repo_root, active, last_used_at, ignored_paths_json, token_budget, created_at, updated_at) \
    VALUES (?1, ?2, 1, ?3, ?4, ?5, ?3, ?3)";

pub(crate) const SELECT_PROJECT_BY_ID: &str = "SELECT * FROM projects WHERE id = ?1";

pub(crate) const SELECT_PROJECT_BY_REPO_ROOT: &str = "SELECT * FROM projects WHERE repo_root = ?1";

pub(crate) const SELECT_ACTIVE_PROJECTS: &str =
    "SELECT * FROM projects WHERE active = 1 ORDER BY last_used_at DESC";

pub(crate) const UPDATE_PROJECT: &str = "UPDATE projects SET name = ?1, repo_root = ?2, \
    active = ?3, last_used_at = ?4, ignored_paths_json = ?5, token_budget = ?6, updated_at = ?7 \
    WHERE id = ?8";

pub(crate) const DELETE_PROJECT: &str = "DELETE FROM projects WHERE id = ?1";

// --- file_summaries ---

pub(crate) const UPSERT_FILE_SUMMARY: &str = "INSERT INTO file_summaries \
    (project_id, path, summary, tags_json, extracted_symbols_json, last_known_mtime, last_known_size, last_updated_at, created_at, updated_at) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?8) \
    ON CONFLICT(project_id, path) DO UPDATE SET \
        summary = excluded.summary, \
        tags_json = excluded.tags_json, \
        extracted_symbols_json = excluded.extracted_symbols_json, \
        last_known_mtime = excluded.last_known_mtime, \
        last_known_size = excluded.last_known_size, \
        last_updated_at = excluded.last_updated_at, \
        updated_at = excluded.updated_at";

pub(crate) const SELECT_FILE_SUMMARY_BY_PATH: &str =
    "SELECT * FROM file_summaries WHERE project_id = ?1 AND path = ?2";

pub(crate) const SELECT_FILE_SUMMARIES_BY_PROJECT: &str =
    "SELECT * FROM file_summaries WHERE project_id = ?1 ORDER BY path";

pub(crate) const SELECT_FILE_SUMMARIES_UNDER: &str =
    "SELECT * FROM file_summaries WHERE project_id = ?1 AND path LIKE ?2 || '%' \
     ORDER BY path LIMIT ?3";

pub(crate) const COUNT_FILE_SUMMARIES_UNDER: &str =
    "SELECT COUNT(*) FROM file_summaries WHERE project_id = ?1 AND path LIKE ?2 || '%'";

pub(crate) const DELETE_FILE_SUMMARY: &str = "DELETE FROM file_summaries WHERE id = ?1";

// --- decisions ---

pub(crate) const INSERT_DECISION: &str = "INSERT INTO decisions \
    (project_id, title, summary, related_files_json, source_session, confidence, created_at, updated_at) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)";

pub(crate) const SELECT_DECISION_BY_ID: &str = "SELECT * FROM decisions WHERE id = ?1";

pub(crate) const SELECT_DECISIONS_BY_PROJECT: &str =
    "SELECT * FROM decisions WHERE project_id = ?1 ORDER BY created_at DESC LIMIT ?2";

pub(crate) const DELETE_DECISION: &str = "DELETE FROM decisions WHERE id = ?1";

// --- session_rollups ---

pub(crate) const INSERT_SESSION_ROLLUP: &str = "INSERT INTO session_rollups \
    (project_id, summary, related_files_json, next_steps_json, created_at, updated_at) \
    VALUES (?1, ?2, ?3, ?4, ?5, ?5)";

pub(crate) const SELECT_SESSION_ROLLUP_BY_ID: &str = "SELECT * FROM session_rollups WHERE id = ?1";

pub(crate) const SELECT_SESSION_ROLLUPS_BY_PROJECT: &str =
    "SELECT * FROM session_rollups WHERE project_id = ?1 ORDER BY created_at DESC LIMIT ?2";

pub(crate) const DELETE_SESSION_ROLLUP: &str = "DELETE FROM session_rollups WHERE id = ?1";

// --- stats / dashboard ---

pub(crate) const COUNT_PROJECTS: &str = "SELECT COUNT(*) FROM projects";
pub(crate) const COUNT_ACTIVE_PROJECTS: &str = "SELECT COUNT(*) FROM projects WHERE active = 1";
pub(crate) const COUNT_FILE_SUMMARIES: &str = "SELECT COUNT(*) FROM file_summaries";
pub(crate) const COUNT_DECISIONS: &str = "SELECT COUNT(*) FROM decisions";
pub(crate) const COUNT_SESSION_ROLLUPS: &str = "SELECT COUNT(*) FROM session_rollups";

pub(crate) const SELECT_PROJECTS_WITH_COUNTS: &str = "\
    SELECT p.id, p.name, p.repo_root, p.active, p.last_used_at, p.created_at, p.updated_at, \
        p.ignored_paths_json, p.token_budget, \
        (SELECT COUNT(*) FROM file_summaries WHERE project_id = p.id) AS file_count, \
        (SELECT COUNT(*) FROM decisions WHERE project_id = p.id) AS decision_count, \
        (SELECT COUNT(*) FROM session_rollups WHERE project_id = p.id) AS rollup_count \
    FROM projects p WHERE p.active = 1 ORDER BY p.last_used_at DESC";

pub(crate) const SELECT_RECENT_FILE_SUMMARIES: &str =
    "SELECT * FROM file_summaries ORDER BY updated_at DESC LIMIT ?1";

pub(crate) const SELECT_RECENT_DECISIONS: &str =
    "SELECT * FROM decisions ORDER BY created_at DESC LIMIT ?1";

pub(crate) const SELECT_RECENT_SESSION_ROLLUPS: &str =
    "SELECT * FROM session_rollups ORDER BY created_at DESC LIMIT ?1";

// --- search ---

pub(crate) const SEARCH_FILE_SUMMARIES: &str =
    "SELECT * FROM file_summaries WHERE project_id = ?1 \
    AND (LOWER(summary) LIKE '%' || LOWER(?2) || '%' \
      OR LOWER(extracted_symbols_json) LIKE '%' || LOWER(?2) || '%' \
      OR LOWER(path) LIKE '%' || LOWER(?2) || '%') \
    ORDER BY path LIMIT ?3";

pub(crate) const SEARCH_DECISIONS: &str = "SELECT * FROM decisions WHERE project_id = ?1 \
    AND (LOWER(title) LIKE '%' || LOWER(?2) || '%' \
      OR LOWER(summary) LIKE '%' || LOWER(?2) || '%') \
    ORDER BY created_at DESC LIMIT ?3";
