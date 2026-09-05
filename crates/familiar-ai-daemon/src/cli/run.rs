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
                // A pause is a question put to a human, so it has to state
                // what is being asked. This block used to print the stop
                // reasons as raw JSON and then every scope finding including
                // the allowed ones — twenty-one lines of "this file was
                // fine" — leaving the operator to infer the cause from a
                // line that had nothing to do with it.
                eprintln!(
                    "\n{prd_id} stopped: {}",
                    describe_stops(&cycle.stop_reasons)
                );

                let failed: Vec<_> = cycle
                    .verification_history
                    .iter()
                    .filter(|check| check.status != familiar_ai_review::VerificationStatus::Passed)
                    .collect();
                if !failed.is_empty() {
                    eprintln!("\n  verification:");
                    for check in failed {
                        eprintln!(
                            "    {:22} {:?}{}",
                            check.check_id,
                            check.status,
                            if check.required {
                                "  (required)"
                            } else {
                                "  (advisory)"
                            }
                        );
                        if !check.summary.trim().is_empty() {
                            eprintln!("      {}", check.summary.trim());
                        }
                    }
                }

                if let Some(review) = &cycle.review_result {
                    let blocking: Vec<_> = review.findings.iter().filter(|f| f.blocking).collect();
                    let advisory: Vec<_> = review.findings.iter().filter(|f| !f.blocking).collect();
                    if !blocking.is_empty() {
                        eprintln!("\n  blocking findings:");
                        for finding in blocking {
                            eprintln!(
                                "    {:?}  {}  {}",
                                finding.severity, finding.finding_id, finding.title
                            );
                        }
                    }
                    if !advisory.is_empty() {
                        eprintln!("\n  non-blocking findings ({}):", advisory.len());
                        for finding in advisory {
                            eprintln!(
                                "    {:?}  {}  {}",
                                finding.severity, finding.finding_id, finding.title
                            );
                        }
                    }
                }

                // Only findings that actually need a decision. An allowed or
                // justified change is the policy working, not a question.
                let undecided: Vec<_> = cycle
                    .scope_evaluations
                    .iter()
                    .flat_map(|evaluation| &evaluation.findings)
                    .filter(|finding| {
                        matches!(
                            finding.decision,
                            familiar_ai_review::ScopeDecision::ProhibitedChange
                                | familiar_ai_review::ScopeDecision::UndeclaredScopeExpansion
                                | familiar_ai_review::ScopeDecision::AmbiguousHumanReview
                        )
                    })
                    .collect();
                if undecided.is_empty() {
                    eprintln!("\n  scope: clean");
                } else {
                    eprintln!("\n  scope needs a decision ({}):", undecided.len());
                    for finding in &undecided {
                        eprintln!("    {:?}  {}", finding.decision, finding.path);
                    }
                    eprintln!("    decide these with: familiar-ai scope-decisions");
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
                eprintln!(
                    "\n  [r] retry remediation   send the implementer back at the failures above"
                );
                eprintln!("  [a] accept reviewed risk  land it with those failures unresolved");
                eprintln!("  [p] preserve checkpoint   stop here and decide later\n");
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

/// Plain-language cause for a pause, so the prompt states the question
/// rather than serialising an enum at the operator.
fn describe_stops(stops: &[familiar_ai_review::ReviewStopReason]) -> String {
    use familiar_ai_review::ReviewStopReason as R;
    if stops.is_empty() {
        return "review incomplete".into();
    }
    stops
        .iter()
        .map(|stop| match stop {
            R::CleanReview => "review clean, awaiting approval",
            R::ScopeAmbiguous => "a change needs a scope decision",
            R::ScopeBroadened => "changes outside the declared scope",
            R::EvidenceFailure => "review evidence could not be captured",
            R::VerificationUnsuccessful => "verification failed",
            R::RetryLimitExhausted => "out of remediation attempts",
            R::TokenLimitExhausted => "out of tokens",
            R::CostLimitExhausted => "out of budget",
            R::DurationLimitExhausted => "out of time",
            R::ConflictingFindings => "reviewers disagree",
            R::ArchitecturalApprovalRequired => "an architectural decision needs a human",
            R::EnvironmentDenied => "the verification environment was unavailable",
            R::NarrationContradiction => "the implementer's account contradicts the diff",
            R::OpenFindingUnwaived => "a terminal review still carries an open finding",
            R::NoIndependentReviewer => "no reviewer independent of the implementer",
            R::MalformedReview => "the reviewer's output could not be parsed",
            R::AgentFailure => "an agent failed",
            R::Interrupted => "interrupted",
        })
        .collect::<Vec<_>>()
        .join("; ")
}
