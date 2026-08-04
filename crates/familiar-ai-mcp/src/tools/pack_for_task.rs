use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use familiar_ai_core::config::BudgetProfile;
use familiar_ai_tokens::{estimate_tokens, truncate_to_tokens};

use crate::tool::{Tool, ToolContext, ToolError};
use crate::tools::keywords::extract_keywords;
use crate::tools::scoring::{score_decision, score_file_summary};

/// Budget allocation per profile: (task_pct, files_pct, decisions_pct, history_pct, cap_tokens)
fn profile_params(profile: &BudgetProfile) -> (f64, f64, f64, f64, usize) {
    match profile {
        BudgetProfile::Minimal => (0.20, 0.45, 0.20, 0.15, 1500),
        BudgetProfile::Balanced => (0.15, 0.45, 0.20, 0.20, 3000),
        BudgetProfile::Aggressive => (0.10, 0.50, 0.20, 0.20, 5000),
        BudgetProfile::MaxAccuracy => (0.05, 0.55, 0.20, 0.20, 10000),
    }
}

fn parse_profile(s: &str) -> Result<BudgetProfile, ToolError> {
    match s.to_lowercase().replace('-', "_").as_str() {
        "minimal" => Ok(BudgetProfile::Minimal),
        "balanced" => Ok(BudgetProfile::Balanced),
        "aggressive" => Ok(BudgetProfile::Aggressive),
        "max_accuracy" | "maxaccuracy" => Ok(BudgetProfile::MaxAccuracy),
        other => Err(ToolError::InvalidParams(format!(
            "unknown budget profile: {other}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
struct Args {
    #[serde(default)]
    project_id: Option<i64>,
    #[serde(default)]
    repo_root: Option<String>,
    task: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    max_tokens: Option<usize>,
}

pub struct PackForTaskTool;

#[async_trait]
impl Tool for PackForTaskTool {
    fn name(&self) -> &'static str {
        "context.pack_for_task"
    }

    fn description(&self) -> &'static str {
        "Assembles a token-budgeted context pack for a Claude task. \
         Gathers relevant file summaries, decisions, and session history \
         within a configurable budget profile."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {"type": "integer"},
                "repo_root": {"type": "string"},
                "task": {"type": "string"},
                "profile": {"type": "string", "enum": ["minimal", "balanced", "aggressive", "max_accuracy"]},
                "max_tokens": {"type": "integer", "minimum": 100},
            },
            "required": ["task"],
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

        // Resolve profile
        let profile = match &parsed.profile {
            Some(s) => parse_profile(s)?,
            None => ctx.config.packer.default_profile.clone(),
        };

        let (task_pct, files_pct, decisions_pct, history_pct, profile_cap) =
            profile_params(&profile);

        let hard_ceiling = ctx.config.packer.hard_ceiling_tokens;
        let total_budget = parsed
            .max_tokens
            .unwrap_or(profile_cap)
            .min(hard_ceiling)
            .max(100);

        let task_budget = (total_budget as f64 * task_pct) as usize;
        let files_budget = (total_budget as f64 * files_pct) as usize;
        let decisions_budget = (total_budget as f64 * decisions_pct) as usize;
        let history_budget = (total_budget as f64 * history_pct) as usize;

        let terms = extract_keywords(&parsed.task);
        let mut warnings: Vec<String> = Vec::new();

        // --- Task section ---
        let (task_text, task_truncated) = truncate_to_tokens(&parsed.task, task_budget);
        if task_truncated {
            warnings.push("Task text truncated to fit budget".into());
        }

        // --- Files section ---
        let all_summaries = ctx
            .storage
            .list_file_summaries_under(project_id, "", 100)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let mut scored_files: Vec<_> = all_summaries
            .iter()
            .map(|s| score_file_summary(s, &terms, &parsed.task))
            .collect();

        // Sort by score descending; if top score == 0, fall back to ID order
        // (recency fallback so packs are never empty)
        let top_score = scored_files.iter().map(|s| s.score).max().unwrap_or(0);
        if top_score > 0 {
            scored_files.sort_by(|a, b| b.score.cmp(&a.score));
        }
        // If top_score == 0 keep original order (already sorted by path from DB)

        let total_file_candidates = scored_files.len();
        let mut files_used = Vec::new();
        let mut files_tokens = 0;
        for scored in &scored_files {
            let entry = format!(
                "### {}\n{}\nSymbols: {}\n",
                scored.item.path,
                scored.item.summary,
                scored.item.extracted_symbols.join(", ")
            );
            let entry_tokens = estimate_tokens(&entry);
            if files_tokens + entry_tokens > files_budget && !files_used.is_empty() {
                break;
            }
            files_tokens += entry_tokens;
            files_used.push(entry);
        }
        if files_used.len() < total_file_candidates {
            warnings.push(format!(
                "{} files found, top {} included",
                total_file_candidates,
                files_used.len()
            ));
        }

        // --- Decisions section ---
        let all_decisions = ctx
            .storage
            .list_decisions_by_project(project_id, 50)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let mut scored_decisions: Vec<_> = all_decisions
            .iter()
            .map(|d| score_decision(d, &terms, &parsed.task))
            .collect();

        let dec_top = scored_decisions.iter().map(|s| s.score).max().unwrap_or(0);
        if dec_top > 0 {
            scored_decisions.sort_by(|a, b| b.score.cmp(&a.score));
        }

        if all_decisions.is_empty() {
            warnings.push("No decisions found for project".into());
        }

        let mut decisions_used = Vec::new();
        let mut decisions_tokens = 0;
        for scored in &scored_decisions {
            let entry = format!("- **{}**: {}\n", scored.item.title, scored.item.summary);
            let entry_tokens = estimate_tokens(&entry);
            if decisions_tokens + entry_tokens > decisions_budget && !decisions_used.is_empty() {
                break;
            }
            decisions_tokens += entry_tokens;
            decisions_used.push(entry);
        }

        // --- History section ---
        let all_rollups = ctx
            .storage
            .list_session_rollups_by_project(project_id, 20)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let mut history_used = Vec::new();
        let mut history_tokens = 0;
        for rollup in &all_rollups {
            let entry = format!("- {}\n", rollup.summary);
            let entry_tokens = estimate_tokens(&entry);
            if history_tokens + entry_tokens > history_budget && !history_used.is_empty() {
                break;
            }
            history_tokens += entry_tokens;
            history_used.push(entry);
        }
        if history_used.len() < all_rollups.len() {
            warnings.push("History truncated to fit token budget".into());
        }

        // --- Assemble context ---
        let mut context = format!("## Task\n{task_text}\n\n");
        if !files_used.is_empty() {
            context.push_str("## Relevant Files\n");
            for entry in &files_used {
                context.push_str(entry);
                context.push('\n');
            }
        }
        if !decisions_used.is_empty() {
            context.push_str("## Decisions\n");
            for entry in &decisions_used {
                context.push_str(entry);
            }
            context.push('\n');
        }
        if !history_used.is_empty() {
            context.push_str("## Session History\n");
            for entry in &history_used {
                context.push_str(entry);
            }
        }

        let total_estimated = estimate_tokens(&context);

        let profile_name = match &profile {
            BudgetProfile::Minimal => "minimal",
            BudgetProfile::Balanced => "balanced",
            BudgetProfile::Aggressive => "aggressive",
            BudgetProfile::MaxAccuracy => "max_accuracy",
        };

        Ok(json!({
            "profile": profile_name,
            "budget_limit": total_budget,
            "estimated_tokens": total_estimated,
            "included": {
                "files": files_used.len(),
                "decisions": decisions_used.len(),
                "history": history_used.len(),
            },
            "warnings": warnings,
            "context": context,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{SqliteStorage, Storage};
    use familiar_ai_core::config::Config;
    use familiar_ai_core::models::{NewDecision, NewFileSummary, NewProject, NewSessionRollup};
    use familiar_ai_core::AppStatus;
    use familiar_ai_storage::{
        Database, DecisionRepository, FileSummaryRepository, ProjectRepository,
        SessionRollupRepository,
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

        // Some file summaries
        for (path, summary, symbols) in [
            (
                "src/auth/token.rs",
                "JWT token validation and refresh",
                &["validate_token", "TokenStore"][..],
            ),
            (
                "src/auth/middleware.rs",
                "Auth middleware for request handling",
                &["AuthMiddleware"][..],
            ),
            (
                "src/db/schema.rs",
                "Database schema definitions",
                &["Schema", "Migration"][..],
            ),
            ("src/main.rs", "Application entry point", &["main"][..]),
        ] {
            db.create_or_update_file_summary(&NewFileSummary {
                project_id: pid,
                path: path.into(),
                summary: summary.into(),
                tags: vec![],
                extracted_symbols: symbols.iter().map(|s| s.to_string()).collect(),
                last_known_mtime: None,
                last_known_size: None,
            })
            .unwrap();
        }

        // Some decisions
        db.create_decision(&NewDecision {
            project_id: pid,
            title: "Keep auth stateless".into(),
            summary: "JWT remains the auth mechanism".into(),
            related_files: vec!["src/auth/token.rs".into()],
            source_session: None,
            confidence: Some("high".into()),
        })
        .unwrap();

        // A session rollup
        db.create_session_rollup(&NewSessionRollup {
            project_id: pid,
            summary: "Implemented auth token rotation".into(),
            related_files: vec![],
            next_steps: vec!["Add integration tests".into()],
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
    async fn pack_with_all_data() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool
            .call(
                json!({"project_id": pid, "task": "continue auth token refresh"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["profile"], "balanced");
        assert!(result["estimated_tokens"].as_u64().unwrap() > 0);
        let ctx_str = result["context"].as_str().unwrap();
        assert!(ctx_str.contains("## Task"));
        assert!(ctx_str.contains("## Relevant Files"));
        assert!(ctx_str.contains("## Decisions"));
        assert!(ctx_str.contains("## Session History"));
        // Auth files should rank high
        assert!(ctx_str.contains("auth"));
    }

    #[tokio::test]
    async fn pack_with_empty_project() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let pid = db
            .create_project(&NewProject {
                name: "empty".into(),
                repo_root: "/empty".into(),
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

        let tool = PackForTaskTool;
        let result = tool
            .call(json!({"project_id": pid, "task": "do stuff"}), &ctx)
            .await
            .unwrap();
        let warnings = result["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("No decisions")));
    }

    #[tokio::test]
    async fn profile_override() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool
            .call(
                json!({"project_id": pid, "task": "x", "profile": "minimal"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["profile"], "minimal");
        assert!(result["budget_limit"].as_u64().unwrap() <= 1500);
    }

    #[tokio::test]
    async fn max_tokens_override() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool
            .call(
                json!({"project_id": pid, "task": "x", "max_tokens": 500}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(result["budget_limit"], 500);
    }

    #[tokio::test]
    async fn hard_ceiling_enforced() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool
            .call(
                json!({"project_id": pid, "task": "x", "max_tokens": 999999}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result["budget_limit"].as_u64().unwrap() <= 15000);
    }

    #[tokio::test]
    async fn unknown_profile_errors() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool
            .call(
                json!({"project_id": pid, "task": "x", "profile": "turbo"}),
                &ctx,
            )
            .await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn by_repo_root() {
        let (ctx, _pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool
            .call(
                json!({"repo_root": "/test/repo", "task": "continue work"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result["estimated_tokens"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn missing_project_errors() {
        let (ctx, _pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool.call(json!({"task": "x"}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidParams(_))));
    }

    #[tokio::test]
    async fn keyword_scoring_influences_ranking() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        let result = tool
            .call(json!({"project_id": pid, "task": "auth token"}), &ctx)
            .await
            .unwrap();
        let ctx_str = result["context"].as_str().unwrap();
        // Auth files should appear before schema/main
        let auth_pos = ctx_str.find("auth/token.rs").unwrap_or(usize::MAX);
        let schema_pos = ctx_str.find("db/schema.rs").unwrap_or(usize::MAX);
        assert!(auth_pos < schema_pos);
    }

    #[tokio::test]
    async fn vague_task_still_produces_content() {
        let (ctx, pid) = make_ctx_with_data();
        let tool = PackForTaskTool;
        // "continue" produces no meaningful keywords that match any file
        let result = tool
            .call(json!({"project_id": pid, "task": "continue"}), &ctx)
            .await
            .unwrap();
        // Should still include files (recency fallback)
        assert!(result["included"]["files"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn reconciled_conflict_is_excluded_from_packed_context() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let pid = db
            .create_project(&NewProject {
                name: "reconciled".into(),
                repo_root: "/test/repo".into(),
                ignored_paths: vec![],
                token_budget: None,
            })
            .unwrap()
            .id;
        db.create_or_update_file_summary(&NewFileSummary {
            project_id: pid,
            path: "src/main.rs".into(),
            summary: "canonical active payload".into(),
            tags: vec![],
            extracted_symbols: vec![],
            last_known_mtime: None,
            last_known_size: None,
        })
        .unwrap();
        db.conn()
            .execute(
                "INSERT INTO file_summaries \
                 (project_id, path, summary, tags_json, extracted_symbols_json, \
                  last_updated_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, '[]', '[]', ?4, ?4, ?4)",
                (
                    pid,
                    "/test/repo/src/main.rs",
                    "archived conflicting payload",
                    "2026-01-02T03:04:05Z",
                ),
            )
            .unwrap();
        let reconciliation = db.reconcile_file_summary_identities(pid).unwrap();
        assert_eq!(reconciliation.conflicts, 1);

        let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::new(Arc::new(Mutex::new(db))));
        let ctx = ToolContext {
            storage,
            status: Arc::new(Mutex::new(AppStatus::new())),
            config: Arc::new(Config::default()),
            router: None,
        };
        let result = PackForTaskTool
            .call(
                json!({"project_id": pid, "task": "main payload", "max_tokens": 1000}),
                &ctx,
            )
            .await
            .unwrap();
        let context = result["context"].as_str().unwrap();
        assert!(context.contains("canonical active payload"));
        assert!(!context.contains("archived conflicting payload"));
        assert!(!context.contains("/test/repo/src/main.rs"));
    }
}
