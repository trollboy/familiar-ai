use std::collections::HashSet;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{Tool, ToolContext, ToolError};
use crate::tools::keywords::extract_keywords;
use crate::tools::scoring::{score_decision, score_file_summary};

const DEFAULT_LIMIT: usize = 20;
const HARD_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
struct Args {
    #[serde(default)]
    project_id: Option<i64>,
    #[serde(default)]
    repo_root: Option<String>,
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "context.search"
    }

    fn description(&self) -> &'static str {
        "Keyword search across project file summaries and decisions. \
         Returns results scored by relevance."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {"type": "integer"},
                "repo_root": {"type": "string"},
                "query": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1},
            },
            "required": ["query"],
            "anyOf": [
                {"required": ["project_id"]},
                {"required": ["repo_root"]},
            ],
        })
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;

        // Resolve project
        let project_id = match (parsed.project_id, parsed.repo_root.as_deref()) {
            (Some(id), _) => id,
            (None, Some(repo)) => {
                let p = ctx
                    .storage
                    .get_project_by_repo_root(repo)
                    .await
                    .map_err(|e| ToolError::Internal(e.to_string()))?
                    .ok_or_else(|| {
                        ToolError::InvalidParams(format!("no project found for repo_root: {repo}"))
                    })?;
                p.id
            }
            (None, None) => {
                return Err(ToolError::InvalidParams(
                    "must provide project_id or repo_root".into(),
                ));
            }
        };

        let terms = extract_keywords(&parsed.query);
        let limit = parsed.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, HARD_LIMIT);

        // Per-term search to gather candidates
        let mut file_candidates_set: HashSet<String> = HashSet::new();
        let mut file_candidates = Vec::new();
        let mut decision_candidates_set: HashSet<i64> = HashSet::new();
        let mut decision_candidates = Vec::new();

        // Search with each term separately, then merge
        let search_terms: Vec<&str> = if terms.is_empty() {
            // If no keywords extracted, search with the raw query
            vec![parsed.query.as_str()]
        } else {
            terms.iter().map(|s| s.as_str()).collect()
        };

        for term in &search_terms {
            let file_hits = ctx
                .storage
                .search_file_summaries(project_id, term, 50)
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;

            for fs in file_hits {
                if file_candidates_set.insert(fs.path.clone()) {
                    file_candidates.push(fs);
                }
            }

            let dec_hits = ctx
                .storage
                .search_decisions(project_id, term, 50)
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;

            for d in dec_hits {
                if decision_candidates_set.insert(d.id) {
                    decision_candidates.push(d);
                }
            }
        }

        // Re-score all candidates using the full term list + original query
        let mut results: Vec<Value> = Vec::new();

        for fs in &file_candidates {
            let scored = score_file_summary(fs, &terms, &parsed.query);
            if scored.score > 0 || terms.is_empty() {
                results.push(json!({
                    "type": "file_summary",
                    "path": fs.path,
                    "summary": fs.summary,
                    "score": scored.score,
                    "matched_terms": scored.matched_terms,
                }));
            }
        }

        for d in &decision_candidates {
            let scored = score_decision(d, &terms, &parsed.query);
            if scored.score > 0 || terms.is_empty() {
                results.push(json!({
                    "type": "decision",
                    "id": d.id,
                    "title": d.title,
                    "summary": d.summary,
                    "score": scored.score,
                    "matched_terms": scored.matched_terms,
                }));
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| {
            let sa = a["score"].as_u64().unwrap_or(0);
            let sb = b["score"].as_u64().unwrap_or(0);
            sb.cmp(&sa)
        });

        results.truncate(limit);

        Ok(json!({
            "query": parsed.query,
            "terms": terms,
            "results": results,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStorage, Storage};
    use familiar_ai_core::config::Config;
    use familiar_ai_core::models::{NewDecision, NewFileSummary, NewProject};
    use familiar_ai_core::AppStatus;
    use familiar_ai_storage::{
        Database, DecisionRepository, FileSummaryRepository, ProjectRepository,
    };
    use std::sync::{Arc, Mutex};

    fn make_ctx_with_data() -> (ToolContext, i64) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let pid = db
            .create_project(&NewProject {
                name: "test".into(),
                repo_root: "/test/repo".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;

        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid,
            path: "src/auth/token.rs".into(),
            summary: "JWT token validation and refresh".into(),
            tags: vec![],
            extracted_symbols: vec!["validate_token".into()],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid,
            path: "src/db/schema.rs".into(),
            summary: "Database schema definitions".into(),
            tags: vec![],
            extracted_symbols: vec!["Schema".into()],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();

        db.create_decision(&NewDecision {
            project_id: pid,
            title: "Keep auth stateless".into(),
            summary: "JWT remains the auth mechanism".into(),
            related_files: vec!["src/auth/token.rs".into()],
            source_session: None,
            confidence: Some("high".into()),
        })
        .unwrap();

        db.create_decision(&NewDecision {
            project_id: pid,
            title: "Use PostgreSQL".into(),
            summary: "Primary data store".into(),
            related_files: vec![],
            source_session: None,
            confidence: None,
        })
        .unwrap();

        let arc_db = Arc::new(Mutex::new(db));
        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(arc_db));
        let ctx = ToolContext {
            storage,
            status: Arc::new(Mutex::new(AppStatus::new())),
            config: Arc::new(Config::default()),
            router: None,
        };
        (ctx, pid)
    }

    #[tokio::test]
    async fn search_finds_matching_files_and_decisions() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool
            .call(json!({"project_id": pid, "query": "auth token"}), &ctx)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        assert!(!results.is_empty());
        // Should find both file summary and decision related to auth
        let types: Vec<&str> = results
            .iter()
            .map(|r| r["type"].as_str().unwrap())
            .collect();
        assert!(types.contains(&"file_summary"));
        assert!(types.contains(&"decision"));
    }

    #[tokio::test]
    async fn search_no_matches() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool
            .call(
                json!({"project_id": pid, "query": "nonexistent_xyzzy_foobar"}),
                &ctx,
            )
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn limit_enforced() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool
            .call(
                json!({"project_id": pid, "query": "auth", "limit": 1}),
                &ctx,
            )
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        assert!(results.len() <= 1);
    }

    #[tokio::test]
    async fn dedup_across_terms() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = SearchTool;
        // "auth token" has two terms; the auth file matches both
        let result = tool
            .call(json!({"project_id": pid, "query": "auth token"}), &ctx)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        let paths: Vec<&str> = results
            .iter()
            .filter(|r| r["type"].as_str().unwrap() == "file_summary")
            .map(|r| r["path"].as_str().unwrap())
            .collect();
        // auth/token.rs should appear only once despite matching both terms
        assert_eq!(
            paths.iter().filter(|p| **p == "src/auth/token.rs").count(),
            1
        );
    }

    #[tokio::test]
    async fn score_ordering() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool
            .call(json!({"project_id": pid, "query": "auth"}), &ctx)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        let scores: Vec<u64> = results
            .iter()
            .map(|r| r["score"].as_u64().unwrap())
            .collect();
        // Should be sorted descending
        for w in scores.windows(2) {
            assert!(w[0] >= w[1]);
        }
    }

    #[tokio::test]
    async fn matched_terms_present() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool
            .call(json!({"project_id": pid, "query": "auth token"}), &ctx)
            .await
            .unwrap();
        let results = result["results"].as_array().unwrap();
        // At least one result should have matched_terms
        assert!(results
            .iter()
            .any(|r| !r["matched_terms"].as_array().unwrap().is_empty()));
    }

    #[tokio::test]
    async fn by_repo_root() {
        let (ctx, _pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool
            .call(json!({"repo_root": "/test/repo", "query": "auth"}), &ctx)
            .await
            .unwrap();
        assert!(!result["results"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_project_errors() {
        let (ctx, _pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool.call(json!({"query": "auth"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn terms_in_response() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = SearchTool;
        let result = tool
            .call(json!({"project_id": pid, "query": "auth token"}), &ctx)
            .await
            .unwrap();
        let terms = result["terms"].as_array().unwrap();
        assert!(terms.iter().any(|t| t.as_str().unwrap() == "auth"));
        assert!(terms.iter().any(|t| t.as_str().unwrap() == "token"));
    }
}
