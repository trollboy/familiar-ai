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

                // One entry per check, newest last: verification_history
                // accumulates an entry per attempt, so listing it raw repeats
                // the same failure once per attempt.
                let mut latest: Vec<&familiar_ai_review::VerificationEvidence> = Vec::new();
                for check in &cycle.verification_history {
                    match latest
                        .iter()
                        .position(|seen| seen.check_id == check.check_id)
                    {
                        Some(index) => latest[index] = check,
                        None => latest.push(check),
                    }
                }
                let failed: Vec<_> = latest
                    .into_iter()
                    .filter(|check| check.status != familiar_ai_review::VerificationStatus::Passed)
                    .collect();
                let mut any_deterministic = false;
                let mut any_varying = false;
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
                    match classify_failure(&cycle.verification_history, check) {
                        FailureShape::First => {}
                        FailureShape::Deterministic { occurrences } => {
                            any_deterministic = true;
                            eprintln!(
                                "\n      Failed {occurrences} times with byte-identical output — \
                                 this is deterministic."
                            );
                        }
                        FailureShape::Varying { previously_passed } => {
                            any_varying = true;
                            if previously_passed {
                                eprintln!(
                                    "\n      This check passed on an earlier attempt — it may be flaky."
                                );
                            } else {
                                eprintln!(
                                    "\n      Earlier attempts failed differently — the output is not stable."
                                );
                            }
                        }
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
                if any_deterministic {
                    eprintln!(
                        "      A failure above is deterministic — the same input has produced"
                    );
                    eprintln!(
                        "      the same output every attempt, so retrying it unchanged cannot"
                    );
                    eprintln!(
                        "      succeed. Only new input (a fix, or different direction) can.\n"
                    );
                } else if any_varying {
                    eprintln!("      A failure above is not stable across attempts, so it may be");
                    eprintln!("      transient. Retrying is reasonable.\n");
                } else {
                    eprintln!(
                        "      Send the implementer back at the failures above. Usually right"
                    );
                    eprintln!("      when the failure is specific and a fix is known.\n");
                }
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

/// Whether a failing check is failing the same way every time.
///
/// `[r]etry` is the right answer for a flaky external check and useless for
/// a deterministic defect, and the prompt presented both identically. The
/// evidence to tell them apart is already on the cycle: verification_history
/// carries one entry per attempt, each with a content hash of its output.
#[derive(Debug)]
enum FailureShape {
    First,
    Deterministic { occurrences: usize },
    Varying { previously_passed: bool },
}

fn classify_failure(
    history: &[familiar_ai_review::VerificationEvidence],
    current: &familiar_ai_review::VerificationEvidence,
) -> FailureShape {
    let same: Vec<_> = history
        .iter()
        .filter(|entry| entry.check_id == current.check_id)
        .collect();
    if same.len() < 2 {
        return FailureShape::First;
    }
    if same
        .iter()
        .any(|entry| entry.status == familiar_ai_review::VerificationStatus::Passed)
    {
        return FailureShape::Varying {
            previously_passed: true,
        };
    }
    // No hash means nothing to compare; do not claim determinism from absence.
    let Some(hash) = current.stdout.as_ref().map(|out| &out.content_hash) else {
        return FailureShape::Varying {
            previously_passed: false,
        };
    };
    let identical = same
        .iter()
        .filter(|entry| {
            entry
                .stdout
                .as_ref()
                .is_some_and(|out| &out.content_hash == hash)
        })
        .count();
    if identical == same.len() {
        FailureShape::Deterministic {
            occurrences: identical,
        }
    } else {
        FailureShape::Varying {
            previously_passed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_review::{EvidenceRef, VerificationEvidence, VerificationStatus};

    fn evidence(
        check_id: &str,
        status: VerificationStatus,
        hash: Option<&str>,
    ) -> VerificationEvidence {
        VerificationEvidence {
            check_id: check_id.into(),
            argv: vec!["cargo".into(), "test".into()],
            working_directory: ".".into(),
            environment_identity: Default::default(),
            tool_identity: None,
            tested_identity: "rev".into(),
            started_at: "2026-09-06T00:00:00Z".into(),
            ended_at: "2026-09-06T00:00:01Z".into(),
            duration_ms: 1,
            exit_code: Some(if status == VerificationStatus::Passed {
                0
            } else {
                1
            }),
            signal: None,
            status,
            required: true,
            summary: String::new(),
            stdout: hash.map(|hash| EvidenceRef {
                content_hash: hash.into(),
                media_type: "text/plain".into(),
                byte_size: 1,
                repository: ".".into(),
                revision: "rev".into(),
                storage_ref: "/nonexistent".into(),
                truncated: false,
                omitted_bytes: 0,
            }),
            stderr: None,
            truncated: false,
        }
    }

    /// The case that cost the owner an evening: the same assertion failing
    /// identically on every attempt while the prompt still recommended retry.
    #[test]
    fn identical_failures_across_attempts_are_deterministic() {
        let history = vec![
            evidence("tests", VerificationStatus::Failed, Some("sha256:aaa")),
            evidence("tests", VerificationStatus::Failed, Some("sha256:aaa")),
            evidence("tests", VerificationStatus::Failed, Some("sha256:aaa")),
        ];
        match classify_failure(&history, history.last().unwrap()) {
            FailureShape::Deterministic { occurrences } => assert_eq!(occurrences, 3),
            other => panic!("expected deterministic, got {other:?}"),
        }
    }

    /// A check that passed before and fails now is the flaky-external case
    /// retry exists for; claiming determinism there would be wrong.
    #[test]
    fn a_check_that_previously_passed_is_reported_as_possibly_transient() {
        let history = vec![
            evidence("scan", VerificationStatus::Passed, Some("sha256:aaa")),
            evidence("scan", VerificationStatus::Failed, Some("sha256:bbb")),
        ];
        match classify_failure(&history, history.last().unwrap()) {
            FailureShape::Varying { previously_passed } => assert!(previously_passed),
            other => panic!("expected varying, got {other:?}"),
        }
    }

    #[test]
    fn differing_output_across_failures_is_not_deterministic() {
        let history = vec![
            evidence("tests", VerificationStatus::Failed, Some("sha256:aaa")),
            evidence("tests", VerificationStatus::Failed, Some("sha256:bbb")),
        ];
        match classify_failure(&history, history.last().unwrap()) {
            FailureShape::Varying { previously_passed } => assert!(!previously_passed),
            other => panic!("expected varying, got {other:?}"),
        }
    }

    /// Never claim determinism from a single data point, or from absence of
    /// evidence — both would be an assertion the data does not support.
    #[test]
    fn a_first_failure_makes_no_claim() {
        let history = vec![evidence(
            "tests",
            VerificationStatus::Failed,
            Some("sha256:aaa"),
        )];
        assert!(matches!(
            classify_failure(&history, history.last().unwrap()),
            FailureShape::First
        ));
    }

    #[test]
    fn missing_output_hashes_never_yield_a_determinism_claim() {
        let history = vec![
            evidence("tests", VerificationStatus::Failed, None),
            evidence("tests", VerificationStatus::Failed, None),
        ];
        assert!(matches!(
            classify_failure(&history, history.last().unwrap()),
            FailureShape::Varying { .. }
        ));
    }
}
