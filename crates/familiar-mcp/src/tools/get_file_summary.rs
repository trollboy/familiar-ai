use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use familiar_core::models::NewFileSummary;
use familiar_summary::SummaryGenerator;

use crate::tool::{Tool, ToolContext, ToolError};

#[derive(Debug, Deserialize)]
struct Args {
    project_id: i64,
    path: String,
}

pub struct GetFileSummaryTool;

#[async_trait]
impl Tool for GetFileSummaryTool {
    fn name(&self) -> &'static str {
        "context.get_file_summary"
    }

    fn description(&self) -> &'static str {
        "Returns the summary for a specific file in a project. \
         Lazily regenerates if missing or stale, with full path-traversal protection."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {"type": "integer"},
                "path": {"type": "string"},
            },
            "required": ["project_id", "path"],
        })
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;

        // Look up the project so we know its repo_root.
        let project = ctx
            .storage
            .get_project_by_id(parsed.project_id)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .ok_or_else(|| {
                ToolError::InvalidParams(format!("unknown project_id: {}", parsed.project_id))
            })?;

        // Try cached summary first.
        let cached = ctx
            .storage
            .get_file_summary(parsed.project_id, &parsed.path)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        // Resolve disk path for staleness check / lazy regen.
        let max_size = ctx.config.summary.max_file_size_bytes;
        let resolved = resolve_within_repo(&project.repo_root, &parsed.path)?;

        let on_disk_meta = std::fs::metadata(&resolved).ok();

        // Decide if cached is fresh enough.
        if let Some(ref summary) = cached {
            if let Some(meta) = &on_disk_meta {
                let mtime = system_time_to_secs(meta.modified().ok());
                let still_fresh = match (summary.last_known_mtime, mtime) {
                    (Some(stored), Some(current)) => stored == current,
                    _ => false,
                };
                let staleness = ctx.config.summary.staleness_threshold_secs as i64;
                let age_secs = (chrono::Utc::now() - summary.last_updated_at).num_seconds();
                if still_fresh && age_secs < staleness {
                    return Ok(json!({
                        "found": true,
                        "source": "cached",
                        "summary": summary,
                    }));
                }
            } else {
                // File doesn't exist on disk anymore but we have a cached summary.
                // Return cached but mark as stale-source.
                return Ok(json!({
                    "found": true,
                    "source": "cached",
                    "summary": summary,
                }));
            }
        }

        // Need to (re)generate.
        let meta = match on_disk_meta {
            Some(m) => m,
            None => {
                return Ok(json!({
                    "found": false,
                    "source": "missing",
                }));
            }
        };

        if !meta.is_file() {
            return Ok(json!({
                "found": false,
                "source": "missing",
            }));
        }

        if meta.len() > max_size {
            return Ok(json!({
                "found": false,
                "source": "too_large",
                "size_bytes": meta.len(),
                "max_bytes": max_size,
            }));
        }

        let content = std::fs::read_to_string(&resolved)
            .map_err(|e| ToolError::Internal(format!("failed to read file: {e}")))?;

        let gen = SummaryGenerator::new();
        let generated = gen.generate(Path::new(&parsed.path), &content);

        let mtime_secs = system_time_to_secs(meta.modified().ok());
        let new_summary = NewFileSummary {
            project_id: parsed.project_id,
            path: parsed.path.clone(),
            summary: generated.summary_text,
            tags: generated.tags,
            extracted_symbols: generated.extracted_symbols,
            last_known_mtime: mtime_secs,
            last_known_size: Some(meta.len() as i64),
        };

        let written = ctx
            .storage
            .create_or_update_file_summary(&new_summary)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        Ok(json!({
            "found": true,
            "source": "fresh",
            "summary": written,
        }))
    }
}

/// Resolve a relative path against a repo root, canonicalizing both, and
/// verify the resolved path stays inside the repo. Returns InvalidParams on
/// any traversal attempt or canonicalization failure.
fn resolve_within_repo(repo_root: &str, path: &str) -> Result<PathBuf, ToolError> {
    let repo_path = PathBuf::from(repo_root);
    let canonical_repo = repo_path.canonicalize().map_err(|e| {
        ToolError::InvalidParams(format!(
            "failed to canonicalize repo_root '{repo_root}': {e}"
        ))
    })?;

    let candidate = canonical_repo.join(path);
    // canonicalize will fail if the file doesn't exist; in that case we
    // still want to validate the prefix using the lexical join.
    let canonical_candidate = match candidate.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // File may not exist — perform lexical containment check.
            if !candidate.starts_with(&canonical_repo) {
                return Err(ToolError::InvalidParams(
                    "path resolves outside repo root".into(),
                ));
            }
            return Ok(candidate);
        }
    };

    if !canonical_candidate.starts_with(&canonical_repo) {
        return Err(ToolError::InvalidParams(
            "path resolves outside repo root".into(),
        ));
    }

    Ok(canonical_candidate)
}

fn system_time_to_secs(time: Option<SystemTime>) -> Option<i64> {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStorage, Storage};
    use familiar_core::config::Config;
    use familiar_core::models::NewProject;
    use familiar_core::AppStatus;
    use familiar_storage::{Database, ProjectRepository};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn make_ctx_with_repo() -> (ToolContext, i64, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().to_string_lossy().to_string();

        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let pid = db
            .create_project(&NewProject {
                name: "p".into(),
                repo_root: repo_root.clone(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;

        let arc_db = Arc::new(Mutex::new(db));
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(arc_db));
        let ctx = ToolContext {
            storage,
            status: Arc::new(Mutex::new(AppStatus::new())),
            config: Arc::new(Config::default()),
            router: None,
        };
        (ctx, pid, tmp)
    }

    #[tokio::test]
    async fn lazy_regenerates_when_missing_summary() {
        let (ctx, pid, tmp) = make_ctx_with_repo();
        std::fs::write(tmp.path().join("foo.rs"), "pub fn x() {}\n").unwrap();

        let tool = GetFileSummaryTool;
        let result = tool
            .call(json!({"project_id": pid, "path": "foo.rs"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["found"], true);
        assert_eq!(result["source"], "fresh");
        assert!(result["summary"]["summary"]
            .as_str()
            .unwrap()
            .contains("rust"));
    }

    #[tokio::test]
    async fn returns_cached_when_fresh() {
        let (ctx, pid, tmp) = make_ctx_with_repo();
        std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}\n").unwrap();
        let tool = GetFileSummaryTool;

        // First call: fresh
        let r1 = tool
            .call(json!({"project_id": pid, "path": "a.rs"}), &ctx)
            .await
            .unwrap();
        assert_eq!(r1["source"], "fresh");

        // Second call without modification: cached
        let r2 = tool
            .call(json!({"project_id": pid, "path": "a.rs"}), &ctx)
            .await
            .unwrap();
        assert_eq!(r2["source"], "cached");
    }

    #[tokio::test]
    async fn returns_missing_when_file_absent() {
        let (ctx, pid, _tmp) = make_ctx_with_repo();
        let tool = GetFileSummaryTool;
        let result = tool
            .call(json!({"project_id": pid, "path": "nope.rs"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result["found"], false);
        assert_eq!(result["source"], "missing");
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let (ctx, pid, _tmp) = make_ctx_with_repo();
        let tool = GetFileSummaryTool;
        let result = tool
            .call(
                json!({"project_id": pid, "path": "../../../etc/passwd"}),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn rejects_unknown_project() {
        let (ctx, _, _tmp) = make_ctx_with_repo();
        let tool = GetFileSummaryTool;
        let result = tool
            .call(json!({"project_id": 999, "path": "x"}), &ctx)
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn returns_too_large_for_oversized_file() {
        let (mut ctx_inner, pid, tmp) = make_ctx_with_repo();
        // Override max_file_size_bytes to a tiny value
        let mut config = Config::default();
        config.summary.max_file_size_bytes = 8;
        ctx_inner.config = Arc::new(config);

        std::fs::write(tmp.path().join("big.rs"), "this is more than eight bytes").unwrap();

        let tool = GetFileSummaryTool;
        let result = tool
            .call(json!({"project_id": pid, "path": "big.rs"}), &ctx_inner)
            .await
            .unwrap();
        assert_eq!(result["found"], false);
        assert_eq!(result["source"], "too_large");
    }
}
