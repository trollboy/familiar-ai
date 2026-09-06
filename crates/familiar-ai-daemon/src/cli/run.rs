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
                let rule = "─".repeat(64);
                eprintln!("\n{rule}");
                eprintln!(
                    "{prd_id} needs you: {}",
                    describe_stops(&cycle.stop_reasons)
                );
                eprintln!("{rule}\n");
                eprintln!("Familiar implemented the change and ran the gates. It stopped");
                eprintln!("rather than land work it could not prove.\n");

                let failed: Vec<_> = cycle
                    .verification_history
                    .iter()
                    .filter(|check| check.status != familiar_ai_review::VerificationStatus::Passed)
                    .collect();
                for check in &failed {
                    let kind = if check.required {
                        "Required"
                    } else {
                        "Advisory"
                    };
                    eprintln!(
                        "  {kind} check `{}` did not pass ({:?}).",
                        check.check_id, check.status
                    );
                    // The assertion itself is captured on disk and was never
                    // shown; opening a sha256-named artifact by hand is not a
                    // thing an operator should have to do to learn what broke.
                    let failures = check
                        .stdout
                        .as_ref()
                        .map(|evidence| failing_tests(&evidence.storage_ref))
                        .unwrap_or_default();
                    for failure in failures.iter().take(3) {
                        eprintln!("\n{failure}");
                    }
                    if failures.len() > 3 {
                        eprintln!("\n      ... and {} more", failures.len() - 3);
                    }
                    if failures.is_empty() && !check.summary.trim().is_empty() {
                        eprintln!("      {}", check.summary.trim());
                    }
                    eprintln!();
                }

                if let Some(review) = &cycle.review_result {
                    let blocking: Vec<_> = review.findings.iter().filter(|f| f.blocking).collect();
                    if !blocking.is_empty() {
                        eprintln!("  A reviewer flagged this as blocking:");
                        for finding in blocking {
                            eprintln!(
                                "    {:?}  {}\n      {}",
                                finding.severity, finding.finding_id, finding.title
                            );
                        }
                        eprintln!();
                    }
                    let advisory = review.findings.iter().filter(|f| !f.blocking).count();
                    if advisory > 0 {
                        eprintln!(
                            "  {advisory} further non-blocking finding(s) were raised; they do"
                        );
                        eprintln!("  not stop this landing.\n");
                    }
                }

                let all_scope: Vec<_> = cycle
                    .scope_evaluations
                    .iter()
                    .flat_map(|evaluation| &evaluation.findings)
                    .collect();
                let undecided: Vec<_> = all_scope
                    .iter()
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
                    eprintln!(
                        "  Scope is clean: all {} changed paths were inside what the PRD",
                        all_scope.len()
                    );
                    eprintln!("  declared or the configuration allows.\n");
                } else {
                    eprintln!(
                        "  {} of {} changed paths need a scope decision from you:",
                        undecided.len(),
                        all_scope.len()
                    );
                    for finding in &undecided {
                        eprintln!("    {:?}  {}", finding.decision, finding.path);
                    }
                    eprintln!("\n  Decide them in one pass with:  familiar-ai scope-decisions\n");
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
                eprintln!("What you can do:\n");
                eprintln!("  [r] retry remediation");
                eprintln!("      Send the implementer back at the failures above. Usually right");
                eprintln!("      when the failure is specific and a fix is known.\n");
                eprintln!("  [a] accept reviewed risk");
                eprintln!("      Land it anyway, recording you as having reviewed and accepted");
                eprintln!("      those failures. The tool will not undo it.\n");
                eprintln!("  [p] preserve checkpoint");
                eprintln!("      Stop here. Nothing is lost — pick it up later with");
                eprintln!("      `familiar-ai resume {prd_id}`.\n");
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

/// Pulls the failing tests out of a captured verification stdout. The
/// assertion an operator needs is already on disk; without this it is
/// reachable only by opening a sha256-named artifact by hand.
fn failing_tests(storage_ref: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(storage_ref) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(rest) = line.strip_prefix("thread '") else {
            continue;
        };
        let Some((name, location)) = rest.split_once("' panicked at ") else {
            continue;
        };
        let mut block = format!(
            "      {name}\n        at {}",
            location.trim_end_matches(':')
        );
        for detail in lines.iter().skip(index + 1).take(3) {
            let detail = detail.trim();
            // Stop at the end of the message, and never relay captured log
            // spew — a JSON line from the daemon under test is not an
            // explanation of why the test failed.
            if detail.is_empty() || detail.starts_with("note:") || detail.starts_with('{') {
                break;
            }
            let detail = if detail.chars().count() > 110 {
                format!("{}...", detail.chars().take(107).collect::<String>())
            } else {
                detail.to_string()
            };
            block.push_str(&format!("\n        {detail}"));
        }
        out.push(block);
    }
    out
}
