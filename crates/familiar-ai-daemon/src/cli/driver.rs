//! `familiar-ai` driver/run execution surface: `preflight`, `next`, `run`,
//! `resume`, `scope-decisions`, and `drive`.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use familiar_ai_core::{
    load_manifest, validate_graph, AppPaths, BacklogDiscovery, BacklogManager, BacklogStatusStore,
    BootstrapApplyResult, Config, FilesystemBacklogDiscovery, ProfiledFilesystemBacklogDiscovery,
};
use familiar_ai_storage::{Database, SqliteBacklogRepository, SqliteBootstrapRepository};

use crate::drive::DriveSummary;
use crate::run::{build_agent, resolved_agent_entries, resolved_remediation_entry, AgentSet};

use super::{database, effective_repository_config};

pub fn resume_command(prd: &str, dry_run: bool) -> Result<(), String> {
    let lines = crate::resume::execute_configured(
        prd,
        dry_run,
        |error, worktree, config, paths, agents| {
            handle_attached_review(Err(error), worktree, config, paths, agents)
        },
    )?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn scope_decisions(
    finding_hash: Option<String>,
    candidate_hash: Option<String>,
    approve: bool,
    reject: bool,
    actor: Option<String>,
    reason: Option<String>,
) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|e| e.to_string())?;
    let mut db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    let repo = familiar_ai_storage::OrchestrationRepository::new(db.conn());
    let pending = repo
        .pending_scope_decisions(&repository.key)
        .map_err(|e| e.to_string())?;
    if finding_hash.is_none() {
        for item in &pending {
            println!(
                "{}",
                serde_json::to_string(item).map_err(|e| e.to_string())?
            );
        }
        if !pending.is_empty() && io::stdin().is_terminal() && io::stderr().is_terminal() {
            eprint!("Decide finding hash (blank to preserve): ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut hash = String::new();
            io::stdin()
                .read_line(&mut hash)
                .map_err(|e| e.to_string())?;
            let hash = hash.trim();
            if hash.is_empty() {
                return Ok(());
            }
            let item = pending
                .iter()
                .find(|p| p.finding_hash == hash)
                .ok_or_else(|| "pending finding hash not found".to_string())?;
            eprint!("Approve or reject [a/r]: ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut choice = String::new();
            io::stdin()
                .read_line(&mut choice)
                .map_err(|e| e.to_string())?;
            eprint!("Actor (human:<identity>): ");
            io::stderr().flush().map_err(|e| e.to_string())?;
            let mut who = String::new();
            io::stdin().read_line(&mut who).map_err(|e| e.to_string())?;
            let checkpoint = repo
                .decide_scope(
                    &repository.key,
                    hash,
                    &item.candidate_hash,
                    choice.trim().eq_ignore_ascii_case("a"),
                    who.trim(),
                    "interactive scope decision",
                )
                .map_err(|e| e.to_string())?;
            continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
        }
        return Ok(());
    }
    if approve == reject {
        return Err("supply exactly one of --approve or --reject".into());
    }
    let actor = actor.ok_or_else(|| "--actor is required for a decision".to_string())?;
    if !actor.starts_with("human:") {
        return Err("--actor must be human:<identity>".into());
    }
    let checkpoint = repo
        .decide_scope(
            &repository.key,
            &finding_hash.unwrap(),
            &candidate_hash.ok_or_else(|| "--candidate-hash is required".to_string())?,
            approve,
            &actor,
            &reason
                .filter(|r| !r.trim().is_empty())
                .ok_or_else(|| "--reason is required".to_string())?,
        )
        .map_err(|e| e.to_string())?;
    continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
    Ok(())
}

fn continue_scope_decision(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    config: &Config,
    checkpoint_id: &str,
) -> Result<(), String> {
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .all(&repository.key)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
        .ok_or_else(|| format!("checkpoint {checkpoint_id} disappeared"))?;
    if checkpoint.phase != "reviewed" {
        return Ok(());
    }
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|error| error.to_string())?;
    let target = FilesystemBacklogDiscovery
        .discover_with_layout(repository, &repository_config.layout())
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|target| target.id.to_string() == checkpoint.prd_id)
        .ok_or_else(|| format!("{} is no longer discoverable", checkpoint.prd_id))?;
    crate::drive::continue_scope_approved_candidate(db, repository, &target, config)
}

pub fn preflight_command() -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|error| error.to_string())?;
    let (implementation_entry, reviewer_entry) = resolved_agent_entries(&config)?;
    let implementation = build_agent(&implementation_entry);
    let reviewer = build_agent(&reviewer_entry);
    let remediation = build_agent(&resolved_remediation_entry(&config)?);
    let report = crate::preflight::run(
        &AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        },
        &config,
        &repository.worktree,
    );
    for check in &report.checks {
        let status = match check.status {
            crate::preflight::PreflightStatus::Passed => "passed",
            crate::preflight::PreflightStatus::Failed => "failed",
            crate::preflight::PreflightStatus::EnvironmentDenied => "environment_denied",
        };
        println!("{status}\t{}\t{}", check.check_id, check.detail);
    }
    if report.is_valid() {
        Ok(())
    } else {
        Err(format!("preflight failed: {}", report.failure_summary()))
    }
}

/// The CLI composition root: read validated configuration and construct the
/// implementation and reviewer agents deterministically.
pub fn run(prd_path: &std::path::Path) -> Result<(), crate::run::RunError> {
    let prepared = crate::run::PreparedRun::acquire()?;
    let result = prepared.execute(prd_path);
    handle_attached_review(
        result,
        &prepared.repository,
        &prepared.config,
        &prepared.paths,
        &prepared.agents(),
    )
}

pub fn handle_attached_review(
    mut result: Result<crate::run::RunWorkflowResult, crate::run::RunError>,
    worktree: &std::path::Path,
    config: &Config,
    paths: &AppPaths,
    agents: &AgentSet<'_>,
) -> Result<(), crate::run::RunError> {
    loop {
        match result {
            Ok(_) => return Ok(()),
            Err(crate::run::RunError::HumanReviewRequired {
                result: implementation,
                cycle,
                prd_id,
            }) => {
                eprintln!("HumanReviewRequired prd={prd_id}");
                eprintln!(
                    "stop_reasons={}",
                    serde_json::to_string(&cycle.stop_reasons).unwrap_or_else(|_| "[]".into())
                );
                if let Some(review) = &cycle.review_result {
                    for finding in &review.findings {
                        eprintln!(
                            "finding {} {:?}: {}",
                            finding.finding_id, finding.severity, finding.title
                        );
                    }
                }
                for finding in cycle
                    .scope_evaluations
                    .iter()
                    .flat_map(|evaluation| &evaluation.findings)
                {
                    eprintln!("scope_finding {}: {}", finding.rule_id, finding.rule_detail);
                }
                if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
                    eprintln!("non-interactive input: preserving checkpoint");
                    return Err(crate::run::RunError::HumanReviewRequired {
                        result: implementation,
                        cycle,
                        prd_id,
                    });
                }
                // Keystrokes pressed during the long silent phases would
                // otherwise be consumed as the choice; drop anything buffered
                // before asking.
                #[cfg(unix)]
                unsafe {
                    libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
                }
                eprint!("Choose [r]etry remediation, [a]ccept reviewed risk, or [p]reserve checkpoint: ");
                let _ = io::stderr().flush();
                let mut choice = String::new();
                if io::stdin().read_line(&mut choice).unwrap_or(0) == 0 {
                    eprintln!("EOF: preserving checkpoint");
                    return Err(crate::run::RunError::HumanReviewRequired {
                        result: implementation,
                        cycle,
                        prd_id,
                    });
                }
                match choice.trim().to_ascii_lowercase().as_str() {
                    "r" | "retry" => {
                        result = crate::run::resume_implemented_checkpoint(
                            worktree, &prd_id, agents, config, paths,
                        );
                    }
                    "a" | "accept" | "accept-risk" => {
                        eprint!("Actor accepting this exact risk (human:<identity>): ");
                        let _ = io::stderr().flush();
                        let mut actor = String::new();
                        if io::stdin().read_line(&mut actor).unwrap_or(0) == 0
                            || actor.trim().is_empty()
                        {
                            eprintln!("missing actor: preserving checkpoint");
                            return Err(crate::run::RunError::HumanReviewRequired {
                                result: implementation,
                                cycle,
                                prd_id,
                            });
                        }
                        crate::run::accept_review_risk(
                            worktree,
                            &prd_id,
                            actor.trim(),
                            &cycle,
                            config,
                            paths,
                        )?;
                        return Ok(());
                    }
                    "p" | "preserve" => {
                        return Err(crate::run::RunError::HumanReviewRequired {
                            result: implementation,
                            cycle,
                            prd_id,
                        })
                    }
                    _ => {
                        eprintln!("unknown choice: preserving checkpoint");
                        return Err(crate::run::RunError::HumanReviewRequired {
                            result: implementation,
                            cycle,
                            prd_id,
                        });
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn drive_command(
    max_prds: Option<u64>,
    max_cost_microusd: Option<u64>,
    max_duration_ms: Option<u64>,
    max_parallel_components: Option<usize>,
    worktree_root: Option<PathBuf>,
    prd_flags: Vec<String>,
) -> Result<DriveSummary, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let summary = crate::drive::execute_configured(
        &paths,
        &current,
        max_prds,
        max_cost_microusd,
        max_duration_ms,
        max_parallel_components,
        worktree_root.as_deref(),
        &prd_flags,
    )?;
    println!(
        "session={} termination={} attempted={} completed={} known_cost_microusd={}",
        summary.session_id,
        summary.termination.as_str(),
        summary.attempted,
        summary.completed,
        summary.known_cost_microusd
    );
    Ok(summary)
}

pub fn next() -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let config = effective_repository_config(&paths, &cwd)?;
    // Resolve Git before opening or migrating storage, preserving the domain's
    // required operation order for invalid working directories.
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| e.to_string())?;
    if discovered.is_empty() {
        return Err("backlog is empty".into());
    }
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    let mut db = database()?;
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&repository, &discovered)
        .map_err(|e| e.to_string())?;
    let manifest = load_manifest(&repository, &discovered).map_err(|e| e.to_string())?;
    let applied = SqliteBootstrapRepository::new(db.conn_mut())
        .apply(&repository, &discovered, manifest.as_ref())
        .map_err(|e| e.to_string())?;
    if let BootstrapApplyResult::Applied(run) = applied {
        eprintln!(
            "historical backlog bootstrap applied: run={} items={} manifest={}",
            run.run_id, run.item_count, run.canonical_hash
        );
    }
    let store = SqliteBacklogRepository::new(db.conn_mut());
    let mut manager = BacklogManager::new(
        ProfiledFilesystemBacklogDiscovery {
            layout: repository_config.layout(),
        },
        store,
    );
    let selected = manager.next(&cwd).map_err(|e| e.to_string())?;
    println!(
        "{}\t{}\t{}\t{}",
        selected.id,
        selected.path,
        selected.status.as_str(),
        selected.title
    );
    Ok(())
}
