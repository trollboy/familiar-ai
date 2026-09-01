//! `familiar-ai plan` — draft or decide a human-reviewed PRD proposal batch.

use std::path::PathBuf;

use clap::Subcommand;
use familiar_ai_core::{AppPaths, BacklogDiscovery, FilesystemBacklogDiscovery};

use crate::plan::{
    approve as approve_plan, generate as generate_plan, print_summary, reject as reject_plan,
};
use crate::run::build_agent;

use super::shared::{database, effective_repository_config};

#[derive(Debug, Subcommand)]
pub enum PlanCommand {
    /// Re-validate and admit a proposal batch to the ordinary backlog.
    Approve {
        batch_id: String,
        #[arg(long)]
        actor: String,
    },
    /// Record a rejection and remove its proposal files.
    Reject {
        batch_id: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
}

pub fn plan(command: Option<PlanCommand>, design_docs: &[PathBuf]) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    paths.ensure_dirs().map_err(|e| e.to_string())?;
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &root)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&root)
        .map_err(|e| e.to_string())?;
    let mut db = database()?;
    let limits = config.planner.as_ref().ok_or("[planner] is required")?;
    match command {
        None => {
            let agent = build_agent(&limits.agent);
            let (id, summary) = generate_plan(
                &repository.worktree,
                design_docs,
                &config,
                &paths,
                &db,
                agent.as_ref(),
            )?;
            print_summary(&id, &summary);
        }
        Some(PlanCommand::Approve { batch_id, actor }) => {
            if !design_docs.is_empty() {
                return Err("design documents are not accepted by plan approve".into());
            }
            let summary = approve_plan(
                &repository.worktree,
                &batch_id,
                &actor,
                limits,
                &repository,
                &mut db,
            )?;
            print_summary(&batch_id, &summary);
        }
        Some(PlanCommand::Reject {
            batch_id,
            actor,
            reason,
        }) => {
            if !design_docs.is_empty() {
                return Err("design documents are not accepted by plan reject".into());
            }
            reject_plan(
                &repository.worktree,
                &batch_id,
                &actor,
                &reason,
                &repository,
                &mut db,
            )?;
            println!("Batch {batch_id} rejected");
        }
    }
    Ok(())
}
