//! Mutation tools over Familiar's stewardship state (PRD-035). Each tool
//! wraps exactly one existing audited human command — `familiar-ai backlog
//! release`, `backlog complete`, `backlog record-complete`, and `backlog
//! bootstrap rollback` — reusing the identical repository resolution,
//! backlog discovery, actor/reason validation, and storage transaction the
//! CLI uses. No new mutation semantics are introduced here.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use familiar_ai_core::BacklogRecoveryAction;

use crate::tool::{Tool, ToolContext, ToolError};
use crate::tools::stewardship_support::{discover_prds, resolve_repository, resolve_target};

#[derive(Debug, Deserialize)]
struct RecoveryArgs {
    #[serde(default)]
    repository_path: Option<String>,
    prd_path: String,
    actor: String,
    reason: String,
}

fn recovery_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repository_path": {"type": "string"},
            "prd_path": {"type": "string"},
            "actor": {"type": "string"},
            "reason": {"type": "string"},
        },
        "required": ["prd_path", "actor", "reason"],
    })
}

fn parse_recovery_args(args: Value) -> Result<RecoveryArgs, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))
}

pub struct BacklogReleaseTool;

#[async_trait]
impl Tool for BacklogReleaseTool {
    fn name(&self) -> &'static str {
        "stewardship.backlog_release"
    }
    fn description(&self) -> &'static str {
        "Releases one retained in_progress PRD back to pending (PRD-012 explicit backlog recovery). Requires --actor and a non-empty --reason, exactly like `familiar-ai backlog release`."
    }
    fn input_schema(&self) -> Value {
        recovery_schema()
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed = parse_recovery_args(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let discovered = discover_prds(&repository, &ctx.config)?;
        let target = resolve_target(&repository, &discovered, &parsed.prd_path)?;
        let result = ctx
            .storage
            .backlog_recover(
                &repository,
                target,
                BacklogRecoveryAction::Release,
                &parsed.actor,
                &parsed.reason,
            )
            .await
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        Ok(json!({
            "prd_id": result.prd.id.to_string(),
            "prd_path": result.prd.path.as_str(),
            "action": BacklogRecoveryAction::Release.as_str(),
            "old_status": "in_progress",
            "new_status": result.status.as_str(),
            "actor": parsed.actor.trim(),
            "reason": parsed.reason.trim(),
        }))
    }
}

pub struct BacklogCompleteTool;

#[async_trait]
impl Tool for BacklogCompleteTool {
    fn name(&self) -> &'static str {
        "stewardship.backlog_complete"
    }
    fn description(&self) -> &'static str {
        "MANUAL OVERRIDE: force-completes one retained in_progress PRD (PRD-012 explicit backlog recovery). Requires an explicit human:<identity> actor and a non-empty reason, exactly like `familiar-ai backlog complete`."
    }
    fn input_schema(&self) -> Value {
        recovery_schema()
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed = parse_recovery_args(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let discovered = discover_prds(&repository, &ctx.config)?;
        let target = resolve_target(&repository, &discovered, &parsed.prd_path)?;
        let result = ctx
            .storage
            .backlog_recover(
                &repository,
                target,
                BacklogRecoveryAction::ManualCompleteOverride,
                &parsed.actor,
                &parsed.reason,
            )
            .await
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        Ok(json!({
            "manual_override": true,
            "prd_id": result.prd.id.to_string(),
            "prd_path": result.prd.path.as_str(),
            "action": BacklogRecoveryAction::ManualCompleteOverride.as_str(),
            "old_status": "in_progress",
            "new_status": result.status.as_str(),
            "actor": parsed.actor.trim(),
            "reason": parsed.reason.trim(),
        }))
    }
}

pub struct BacklogRecordCompleteTool;

#[async_trait]
impl Tool for BacklogRecordCompleteTool {
    fn name(&self) -> &'static str {
        "stewardship.backlog_record_complete"
    }
    fn description(&self) -> &'static str {
        "Declares one pending PRD completed outside Familiar's tracking (PRD-022), once every declared dependency is itself completed. Requires an explicit human:<identity> actor and a non-empty reason, exactly like `familiar-ai backlog record-complete`."
    }
    fn input_schema(&self) -> Value {
        recovery_schema()
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let parsed = parse_recovery_args(args)?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let discovered = discover_prds(&repository, &ctx.config)?;
        let target = resolve_target(&repository, &discovered, &parsed.prd_path)?;
        let result = ctx
            .storage
            .backlog_record_complete(
                &repository,
                &discovered,
                target,
                &parsed.actor,
                &parsed.reason,
            )
            .await
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        Ok(json!({
            "prd_id": result.prd.id.to_string(),
            "prd_path": result.prd.path.as_str(),
            "action": "recorded_complete",
            "old_status": "pending",
            "new_status": result.status.as_str(),
            "actor": parsed.actor.trim(),
            "reason": parsed.reason.trim(),
        }))
    }
}

pub struct BootstrapRollbackTool;

#[async_trait]
impl Tool for BootstrapRollbackTool {
    fn name(&self) -> &'static str {
        "stewardship.bootstrap_rollback"
    }
    fn description(&self) -> &'static str {
        "Rolls back a backlog bootstrap run (PRD-010), restoring the prior backlog snapshot. Requires --actor and a non-empty --reason, exactly like `familiar-ai backlog bootstrap rollback`."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repository_path": {"type": "string"},
                "run_id": {"type": "string"},
                "actor": {"type": "string"},
                "reason": {"type": "string"},
            },
            "required": ["run_id", "actor", "reason"],
        })
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        #[derive(Debug, Deserialize)]
        struct Args {
            #[serde(default)]
            repository_path: Option<String>,
            run_id: String,
            actor: String,
            reason: String,
        }
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidParams(format!("invalid args: {e}")))?;
        let repository = resolve_repository(parsed.repository_path.as_deref())?;
        let discovered = discover_prds(&repository, &ctx.config)?;
        let result = ctx
            .storage
            .bootstrap_rollback(
                &repository,
                &discovered,
                &parsed.run_id,
                &parsed.actor,
                &parsed.reason,
            )
            .await
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        Ok(json!({
            "rollback_run_id": result.rollback_run_id,
            "item_count": result.item_count,
            "actor": parsed.actor.trim(),
            "reason": parsed.reason.trim(),
        }))
    }
}
