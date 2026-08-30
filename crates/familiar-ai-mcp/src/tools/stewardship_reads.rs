//! Read tools over Familiar's durable execution-era state (PRD-035):
//! backlog graph, driver sessions/attempts, worktrees (checkpoints), review
//! findings/verification, budgets, delivery decisions, recovery events, and
//! pending human gates. Every tool is scoped to exactly one repository,
//! resolved the same way the `familiar-ai` CLI resolves it, so a caller can
//! never read another repository's state by supplying its identifiers.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tool::{Tool, ToolContext, ToolError};
use crate::tools::stewardship_support::{clamp_limit, redact_json_field, resolve_repository};

fn repo_schema_properties() -> Value {
    json!({
        "repository_path": {
            "type": "string",
            "description": "Path within the target repository; defaults to the server's current working directory."
        },
        "cursor": {"type": "string"},
        "limit": {"type": "integer", "minimum": 1},
    })
}

#[derive(Debug, Deserialize, Default)]
struct RepoPageArgs {
    #[serde(default)]
    repository_path: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

fn parse<T: for<'de> Deserialize<'de> + Default>(args: Value) -> Result<T, ToolError> {
    if args.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(args).map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))
}

pub struct ListBacklogTool;

#[async_trait]
impl Tool for ListBacklogTool {
    fn name(&self) -> &'static str {
        "stewardship.list_backlog"
    }
    fn description(&self) -> &'static str {
        "Lists the backlog graph (PRD status, discovery, and audit timestamps) for one repository."
    }
    fn input_schema(&self) -> Value {
        let mut properties = repo_schema_properties();
        properties["status"] = json!({
            "type": "string",
            "enum": ["pending", "in_progress", "completed", "blocked"],
        });
        json!({"type": "object", "properties": properties})
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        #[derive(Debug, Deserialize, Default)]
        struct Args {
            #[serde(default)]
            repository_path: Option<String>,
            #[serde(default)]
            status: Option<String>,
            #[serde(default)]
            cursor: Option<String>,
            #[serde(default)]
            limit: Option<usize>,
        }
        let parsed: Args = parse(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let limit = clamp_limit(parsed.limit);
        let items = ctx
            .storage
            .list_backlog(
                &repository.key,
                parsed.status.as_deref(),
                parsed.cursor.as_deref(),
                limit,
            )
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let next_cursor = (items.len() == limit)
            .then(|| items.last().map(|item| item.prd_path.clone()))
            .flatten();
        Ok(json!({
            "repository_key": repository.key,
            "items": items,
            "next_cursor": next_cursor,
        }))
    }
}

pub struct ListDriverSessionsTool;

#[async_trait]
impl Tool for ListDriverSessionsTool {
    fn name(&self) -> &'static str {
        "stewardship.list_sessions"
    }
    fn description(&self) -> &'static str {
        "Lists driver sessions (drive-loop runs) for one repository, most recent first."
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": repo_schema_properties()})
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: RepoPageArgs = parse(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let limit = clamp_limit(parsed.limit);
        let items = ctx
            .storage
            .list_driver_sessions(&repository.key, parsed.cursor.as_deref(), limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let next_cursor = (items.len() == limit)
            .then(|| items.last().map(|item| item.session_id.clone()))
            .flatten();
        Ok(json!({
            "repository_key": repository.key,
            "items": items,
            "next_cursor": next_cursor,
        }))
    }
}

pub struct ListDriverAttemptsTool;

#[async_trait]
impl Tool for ListDriverAttemptsTool {
    fn name(&self) -> &'static str {
        "stewardship.list_attempts"
    }
    fn description(&self) -> &'static str {
        "Lists one driver session's attempts, including worktree/branch identity, in order."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repository_path": {"type": "string"},
                "session_id": {"type": "string"},
                "cursor": {"type": "integer"},
                "limit": {"type": "integer", "minimum": 1},
            },
            "required": ["session_id"],
        })
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        #[derive(Debug, Deserialize)]
        struct Args {
            #[serde(default)]
            repository_path: Option<String>,
            session_id: String,
            #[serde(default)]
            cursor: Option<i64>,
            #[serde(default)]
            limit: Option<usize>,
        }
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let session = ctx
            .storage
            .get_driver_session(&parsed.session_id)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .filter(|session| session.repository_key == repository.key)
            .ok_or_else(|| ToolError::InvalidParams("no such session in this repository".into()))?;
        let limit = clamp_limit(parsed.limit);
        let items = ctx
            .storage
            .list_driver_attempts(&session.session_id, parsed.cursor, limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let next_cursor = (items.len() == limit)
            .then(|| items.last().map(|item| item.sequence))
            .flatten();
        Ok(json!({
            "repository_key": repository.key,
            "session_id": session.session_id,
            "items": items,
            "next_cursor": next_cursor,
        }))
    }
}

pub struct ListCheckpointsTool;

#[async_trait]
impl Tool for ListCheckpointsTool {
    fn name(&self) -> &'static str {
        "stewardship.list_checkpoints"
    }
    fn description(&self) -> &'static str {
        "Lists execution checkpoints (worktree/branch identity and recovery phase) for one repository."
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": repo_schema_properties()})
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: RepoPageArgs = parse(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let limit = clamp_limit(parsed.limit);
        let items = ctx
            .storage
            .list_checkpoints(&repository.key, parsed.cursor.as_deref(), limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let next_cursor = (items.len() == limit)
            .then(|| items.last().map(|item| item.prd_id.clone()))
            .flatten();
        let items: Vec<Value> = items
            .into_iter()
            .map(|checkpoint| {
                json!({
                    "checkpoint_id": checkpoint.checkpoint_id,
                    "prd_id": checkpoint.prd_id,
                    "prd_path": checkpoint.prd_path,
                    "execution_id": checkpoint.execution_id,
                    "phase": checkpoint.phase,
                    "base_revision": checkpoint.base_revision,
                    "worktree_path": checkpoint.worktree_path,
                    "branch_name": checkpoint.branch_name,
                    "diff_hash": checkpoint.diff_hash,
                    "changed_files": redact_json_field(&checkpoint.changed_files_json),
                    "agent_identity": checkpoint.agent_identity,
                    "usage": redact_json_field(&checkpoint.usage_json),
                    "test_evidence": redact_json_field(&checkpoint.test_evidence_json),
                    "invalid_reason": checkpoint.invalid_reason,
                })
            })
            .collect();
        Ok(json!({
            "repository_key": repository.key,
            "items": items,
            "next_cursor": next_cursor,
        }))
    }
}

pub struct ListRecoveryEventsTool;

#[async_trait]
impl Tool for ListRecoveryEventsTool {
    fn name(&self) -> &'static str {
        "stewardship.list_recovery_events"
    }
    fn description(&self) -> &'static str {
        "Lists audited backlog recovery events (release/complete/record-complete) for one repository."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repository_path": {"type": "string"},
                "cursor": {"type": "integer"},
                "limit": {"type": "integer", "minimum": 1},
            },
        })
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        #[derive(Debug, Deserialize, Default)]
        struct Args {
            #[serde(default)]
            repository_path: Option<String>,
            #[serde(default)]
            cursor: Option<i64>,
            #[serde(default)]
            limit: Option<usize>,
        }
        let parsed: Args = parse(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let limit = clamp_limit(parsed.limit);
        let items = ctx
            .storage
            .list_recovery_events(&repository.key, parsed.cursor, limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let next_cursor = (items.len() == limit)
            .then(|| items.last().map(|item| item.event_id))
            .flatten();
        Ok(json!({
            "repository_key": repository.key,
            "items": items,
            "next_cursor": next_cursor,
        }))
    }
}

pub struct ListDeliveryDecisionsTool;

#[async_trait]
impl Tool for ListDeliveryDecisionsTool {
    fn name(&self) -> &'static str {
        "stewardship.list_delivery_decisions"
    }
    fn description(&self) -> &'static str {
        "Lists delivery authority decisions (mode, actor, disposition, warrant consumption) for one repository."
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": repo_schema_properties()})
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed: RepoPageArgs = parse(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let limit = clamp_limit(parsed.limit);
        let items = ctx
            .storage
            .list_delivery_decisions(&repository.key, parsed.cursor.as_deref(), limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let next_cursor = (items.len() == limit)
            .then(|| items.last().map(|item| item.decision_id.clone()))
            .flatten();
        let items: Vec<Value> = items
            .into_iter()
            .map(|decision| {
                json!({
                    "decision_id": decision.decision_id,
                    "session_id": decision.session_id,
                    "prd_id": decision.prd_id,
                    "mode": decision.mode,
                    "actor": decision.actor,
                    "decision": decision.decision,
                    "assurance_label": decision.assurance_label,
                    "findings": redact_json_field(&decision.findings_json),
                    "stop_reasons": redact_json_field(&decision.stop_reasons_json),
                    "warrant": decision.warrant_json.as_deref().map(redact_json_field),
                    "warrant_consumed": decision.warrant_consumed,
                    "created_at": decision.created_at,
                })
            })
            .collect();
        Ok(json!({
            "repository_key": repository.key,
            "items": items,
            "next_cursor": next_cursor,
        }))
    }
}

pub struct GetBudgetTool;

#[async_trait]
impl Tool for GetBudgetTool {
    fn name(&self) -> &'static str {
        "stewardship.get_budget"
    }
    fn description(&self) -> &'static str {
        "Returns one driver session's warrant plus known/unknown attempt cost and delivery warrant consumption."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repository_path": {"type": "string"},
                "session_id": {"type": "string"},
            },
            "required": ["session_id"],
        })
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        #[derive(Debug, Deserialize)]
        struct Args {
            #[serde(default)]
            repository_path: Option<String>,
            session_id: String,
        }
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let summary = ctx
            .storage
            .get_budget_summary(&parsed.session_id)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .filter(|summary| summary.repository_key == repository.key)
            .ok_or_else(|| ToolError::InvalidParams("no such session in this repository".into()))?;
        Ok(json!({
            "session_id": summary.session_id,
            "repository_key": summary.repository_key,
            "warrant": redact_json_field(&summary.warrant_json),
            "known_cost_microusd": summary.known_cost_microusd,
            "known_cost_attempts": summary.known_cost_attempts,
            "unknown_cost_attempts": summary.unknown_cost_attempts,
            "delivery_warrant_consumed": summary.delivery_warrant_consumed,
        }))
    }
}

pub struct ListReviewFindingsTool;

#[async_trait]
impl Tool for ListReviewFindingsTool {
    fn name(&self) -> &'static str {
        "stewardship.list_review_findings"
    }
    fn description(&self) -> &'static str {
        "Lists review disposition and blocking scope findings for every attempt in one driver session."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repository_path": {"type": "string"},
                "session_id": {"type": "string"},
            },
            "required": ["session_id"],
        })
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        #[derive(Debug, Deserialize)]
        struct Args {
            #[serde(default)]
            repository_path: Option<String>,
            session_id: String,
        }
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let items = ctx
            .storage
            .list_review_findings(&repository.key, &parsed.session_id)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        Ok(json!({
            "repository_key": repository.key,
            "session_id": parsed.session_id,
            "items": items,
        }))
    }
}

pub struct ListPendingHumanGatesTool;

#[async_trait]
impl Tool for ListPendingHumanGatesTool {
    fn name(&self) -> &'static str {
        "stewardship.list_pending_human_gates"
    }
    fn description(&self) -> &'static str {
        "Lists stopped attempts and blocked checkpoints awaiting a human recovery decision, with the exact recovery command(s) for each."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repository_path": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1},
            },
        })
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        #[derive(Debug, Deserialize, Default)]
        struct Args {
            #[serde(default)]
            repository_path: Option<String>,
            #[serde(default)]
            limit: Option<usize>,
        }
        let parsed: Args = parse(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let limit = clamp_limit(parsed.limit);
        let items = ctx
            .storage
            .list_pending_human_gates(&repository.key, limit)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        Ok(json!({
            "repository_key": repository.key,
            "items": items,
        }))
    }
}
