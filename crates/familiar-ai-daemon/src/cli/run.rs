//! `familiar-ai run` — execute a repository PRD with the configured coding
//! agent.

use std::io::{self, IsTerminal, Write};

use familiar_ai_core::{AppPaths, Config};

use crate::run::AgentSet;

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

pub(crate) fn handle_attached_review(
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
