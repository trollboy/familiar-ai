use std::collections::BTreeSet;

use thiserror::Error;

use crate::{ContextDocument, DocumentKind, ExecutionContext, InclusionReason};

#[derive(Debug, Default, Clone, Copy)]
pub struct ContextBudgeter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub hard_ceiling_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetedExecutionContext {
    pub context: ExecutionContext,
    pub report: ContextBudgetReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudgetReport {
    pub hard_ceiling_tokens: u64,
    pub input_estimated_tokens: u64,
    pub included_estimated_tokens: u64,
    pub excluded_estimated_tokens: u64,
    pub decisions: Vec<ContextBudgetDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBudgetDecision {
    pub path: String,
    pub kind: DocumentKind,
    pub estimated_tokens: u64,
    pub priority: ContextPriority,
    pub selection_index: u64,
    pub outcome: ContextBudgetOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContextPriority {
    RequestedPrd,
    DirectContract,
    DirectAdr,
    DirectSupporting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextBudgetOutcome {
    Included(ContextInclusionReason),
    Excluded(ContextExclusionReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextInclusionReason {
    RequestedPrdMandatory,
    FitsWithinRemainingBudget {
        remaining_before_tokens: u64,
        remaining_after_tokens: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextExclusionReason {
    ExceedsRemainingBudget {
        required_tokens: u64,
        remaining_tokens: u64,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContextBudgetError {
    #[error("PRD {path} requires {estimated_tokens} estimated tokens, exceeding hard ceiling {hard_ceiling_tokens}")]
    PrdExceedsHardCeiling {
        path: String,
        estimated_tokens: u64,
        hard_ceiling_tokens: u64,
    },
    #[error("invalid PRD kind or inclusion provenance for {path}")]
    InvalidPrd { path: String },
    #[error("invalid directly referenced document kind or inclusion provenance for {path}")]
    InvalidDirectDocument { path: String },
    #[error("duplicate context document path {path}")]
    DuplicatePath { path: String },
    #[error("context document has malformed repository-relative path {path:?}")]
    MalformedPath { path: String },
    #[error("token estimate for {path} is {actual}, expected {expected}")]
    InconsistentDocumentEstimate {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("aggregate token estimate is {actual}, expected {expected}")]
    InconsistentAggregateEstimate { expected: u64, actual: u64 },
    #[error("token arithmetic overflow")]
    TokenArithmeticOverflow,
    #[error("report selection index cannot be represented as u64")]
    ReportIndexOverflow,
}

impl ContextBudgeter {
    pub fn new() -> Self {
        Self
    }

    pub fn budget(
        &self,
        context: ExecutionContext,
        budget: ContextBudget,
    ) -> Result<BudgetedExecutionContext, ContextBudgetError> {
        validate_context(&context)?;
        if context.prd.estimated_tokens > budget.hard_ceiling_tokens {
            return Err(ContextBudgetError::PrdExceedsHardCeiling {
                path: context.prd.path.clone(),
                estimated_tokens: context.prd.estimated_tokens,
                hard_ceiling_tokens: budget.hard_ceiling_tokens,
            });
        }

        let mut decisions = Vec::with_capacity(context.documents.len() + 1);
        decisions.push(decision(
            &context.prd,
            ContextPriority::RequestedPrd,
            0,
            ContextBudgetOutcome::Included(ContextInclusionReason::RequestedPrdMandatory),
        )?);
        let mut used = context.prd.estimated_tokens;
        let mut excluded = 0_u64;
        let mut selected = BTreeSet::new();
        let mut ranked: Vec<&ContextDocument> = context.documents.iter().collect();
        ranked.sort_by(|left, right| {
            priority(left.kind)
                .cmp(&priority(right.kind))
                .then_with(|| left.path.cmp(&right.path))
        });

        for document in ranked {
            let remaining = budget
                .hard_ceiling_tokens
                .checked_sub(used)
                .ok_or(ContextBudgetError::TokenArithmeticOverflow)?;
            let outcome = if document.estimated_tokens <= remaining {
                let after = remaining
                    .checked_sub(document.estimated_tokens)
                    .ok_or(ContextBudgetError::TokenArithmeticOverflow)?;
                used = used
                    .checked_add(document.estimated_tokens)
                    .ok_or(ContextBudgetError::TokenArithmeticOverflow)?;
                selected.insert(document.path.clone());
                ContextBudgetOutcome::Included(ContextInclusionReason::FitsWithinRemainingBudget {
                    remaining_before_tokens: remaining,
                    remaining_after_tokens: after,
                })
            } else {
                excluded = excluded
                    .checked_add(document.estimated_tokens)
                    .ok_or(ContextBudgetError::TokenArithmeticOverflow)?;
                ContextBudgetOutcome::Excluded(ContextExclusionReason::ExceedsRemainingBudget {
                    required_tokens: document.estimated_tokens,
                    remaining_tokens: remaining,
                })
            };
            decisions.push(decision(
                document,
                priority(document.kind),
                decisions.len(),
                outcome,
            )?);
        }

        let reconciled = used
            .checked_add(excluded)
            .ok_or(ContextBudgetError::TokenArithmeticOverflow)?;
        if reconciled != context.estimated_tokens {
            return Err(ContextBudgetError::InconsistentAggregateEstimate {
                expected: reconciled,
                actual: context.estimated_tokens,
            });
        }
        let documents = context
            .documents
            .into_iter()
            .filter(|document| selected.contains(&document.path))
            .collect();
        Ok(BudgetedExecutionContext {
            context: ExecutionContext {
                repository: context.repository,
                prd: context.prd,
                documents,
                estimated_tokens: used,
            },
            report: ContextBudgetReport {
                hard_ceiling_tokens: budget.hard_ceiling_tokens,
                input_estimated_tokens: context.estimated_tokens,
                included_estimated_tokens: used,
                excluded_estimated_tokens: excluded,
                decisions,
            },
        })
    }
}

fn validate_context(context: &ExecutionContext) -> Result<(), ContextBudgetError> {
    validate_path(&context.prd.path)?;
    if context.prd.kind != DocumentKind::Prd
        || context.prd.inclusion != InclusionReason::RequestedPrd
    {
        return Err(ContextBudgetError::InvalidPrd {
            path: context.prd.path.clone(),
        });
    }
    let mut paths = BTreeSet::new();
    paths.insert(context.prd.path.clone());
    let mut total = validate_estimate(&context.prd)?;
    for document in &context.documents {
        validate_path(&document.path)?;
        if !paths.insert(document.path.clone()) {
            return Err(ContextBudgetError::DuplicatePath {
                path: document.path.clone(),
            });
        }
        let valid_kind = matches!(
            document.kind,
            DocumentKind::Contract | DocumentKind::Adr | DocumentKind::Supporting
        );
        let valid_inclusion = document.inclusion
            == (InclusionReason::DirectReference {
                referenced_by: context.prd.path.clone(),
            });
        if !valid_kind || !valid_inclusion {
            return Err(ContextBudgetError::InvalidDirectDocument {
                path: document.path.clone(),
            });
        }
        total = total
            .checked_add(validate_estimate(document)?)
            .ok_or(ContextBudgetError::TokenArithmeticOverflow)?;
    }
    if total != context.estimated_tokens {
        return Err(ContextBudgetError::InconsistentAggregateEstimate {
            expected: total,
            actual: context.estimated_tokens,
        });
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), ContextBudgetError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(ContextBudgetError::MalformedPath { path: path.into() });
    }
    Ok(())
}

fn validate_estimate(document: &ContextDocument) -> Result<u64, ContextBudgetError> {
    let expected = u64::try_from(familiar_ai_tokens::estimate_tokens(&document.content))
        .map_err(|_| ContextBudgetError::TokenArithmeticOverflow)?;
    if expected != document.estimated_tokens {
        return Err(ContextBudgetError::InconsistentDocumentEstimate {
            path: document.path.clone(),
            expected,
            actual: document.estimated_tokens,
        });
    }
    Ok(expected)
}

fn priority(kind: DocumentKind) -> ContextPriority {
    match kind {
        DocumentKind::Contract => ContextPriority::DirectContract,
        DocumentKind::Adr => ContextPriority::DirectAdr,
        DocumentKind::Supporting => ContextPriority::DirectSupporting,
        DocumentKind::Prd => ContextPriority::RequestedPrd,
    }
}

fn decision(
    document: &ContextDocument,
    priority: ContextPriority,
    index: usize,
    outcome: ContextBudgetOutcome,
) -> Result<ContextBudgetDecision, ContextBudgetError> {
    Ok(ContextBudgetDecision {
        path: document.path.clone(),
        kind: document.kind,
        estimated_tokens: document.estimated_tokens,
        priority,
        selection_index: u64::try_from(index)
            .map_err(|_| ContextBudgetError::ReportIndexOverflow)?,
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RepositoryContext;
    use std::path::PathBuf;

    fn document(path: &str, kind: DocumentKind, content: &str) -> ContextDocument {
        ContextDocument {
            path: path.into(),
            kind,
            content: content.into(),
            inclusion: if kind == DocumentKind::Prd {
                InclusionReason::RequestedPrd
            } else {
                InclusionReason::DirectReference {
                    referenced_by: "docs/prds/work.md".into(),
                }
            },
            estimated_tokens: familiar_ai_tokens::estimate_tokens(content) as u64,
        }
    }

    fn context(documents: Vec<ContextDocument>) -> ExecutionContext {
        let prd = document("docs/prds/work.md", DocumentKind::Prd, "required prd");
        let estimated_tokens = prd.estimated_tokens
            + documents
                .iter()
                .map(|item| item.estimated_tokens)
                .sum::<u64>();
        ExecutionContext {
            repository: RepositoryContext {
                repository: PathBuf::from("repo"),
                worktree: PathBuf::from("worktree"),
                git_commit: None,
            },
            prd,
            documents,
            estimated_tokens,
        }
    }

    #[test]
    fn selects_by_priority_but_preserves_input_render_order_and_reports_every_document() {
        let adr = document("docs/adr/a.md", DocumentKind::Adr, "adr");
        let contract = document("docs/contracts/z.md", DocumentKind::Contract, "contract");
        let support = document("docs/supporting/b.md", DocumentKind::Supporting, "support");
        let input = context(vec![adr.clone(), contract.clone(), support.clone()]);
        let ceiling = input.prd.estimated_tokens + contract.estimated_tokens + adr.estimated_tokens;
        let result = ContextBudgeter
            .budget(
                input,
                ContextBudget {
                    hard_ceiling_tokens: ceiling,
                },
            )
            .unwrap();
        assert_eq!(
            result.context.documents,
            vec![adr.clone(), contract.clone()]
        );
        assert_eq!(
            result
                .report
                .decisions
                .iter()
                .map(|item| item.path.as_str())
                .collect::<Vec<_>>(),
            [
                "docs/prds/work.md",
                "docs/contracts/z.md",
                "docs/adr/a.md",
                "docs/supporting/b.md"
            ]
        );
        assert!(matches!(
            result.report.decisions[3].outcome,
            ContextBudgetOutcome::Excluded(_)
        ));
        assert_eq!(
            result.report.included_estimated_tokens,
            result.context.estimated_tokens
        );
        assert_eq!(
            result.report.included_estimated_tokens + result.report.excluded_estimated_tokens,
            result.report.input_estimated_tokens
        );
    }

    #[test]
    fn skips_oversized_document_and_continues_to_later_one() {
        let large = document(
            "docs/contracts/a.md",
            DocumentKind::Contract,
            "one two three four five six",
        );
        let small = document("docs/adr/a.md", DocumentKind::Adr, "x");
        let input = context(vec![large, small.clone()]);
        let ceiling = input.prd.estimated_tokens + small.estimated_tokens;
        let result = ContextBudgeter
            .budget(
                input,
                ContextBudget {
                    hard_ceiling_tokens: ceiling,
                },
            )
            .unwrap();
        assert_eq!(result.context.documents, vec![small]);
    }

    #[test]
    fn exact_fit_is_deterministic_and_preserves_values() {
        let documents = vec![
            document("docs/contracts/a.md", DocumentKind::Contract, "héllo"),
            document("docs/supporting/empty.md", DocumentKind::Supporting, ""),
        ];
        let input = context(documents);
        let expected = input.clone();
        let first = ContextBudgeter
            .budget(
                input.clone(),
                ContextBudget {
                    hard_ceiling_tokens: input.estimated_tokens,
                },
            )
            .unwrap();
        let second = ContextBudgeter
            .budget(
                input,
                ContextBudget {
                    hard_ceiling_tokens: expected.estimated_tokens,
                },
            )
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.context.prd, expected.prd);
        assert_eq!(first.context.documents, expected.documents);
    }

    #[test]
    fn oversized_prd_and_inconsistent_inputs_are_categorized() {
        let input = context(Vec::new());
        assert!(matches!(
            ContextBudgeter.budget(
                input.clone(),
                ContextBudget {
                    hard_ceiling_tokens: input.prd.estimated_tokens - 1
                }
            ),
            Err(ContextBudgetError::PrdExceedsHardCeiling { .. })
        ));
        let mut bad = input.clone();
        bad.prd.estimated_tokens += 1;
        bad.estimated_tokens += 1;
        assert!(matches!(
            ContextBudgeter.budget(
                bad,
                ContextBudget {
                    hard_ceiling_tokens: u64::MAX
                }
            ),
            Err(ContextBudgetError::InconsistentDocumentEstimate { .. })
        ));
        let mut bad = input;
        bad.estimated_tokens += 1;
        assert!(matches!(
            ContextBudgeter.budget(
                bad,
                ContextBudget {
                    hard_ceiling_tokens: u64::MAX
                }
            ),
            Err(ContextBudgetError::InconsistentAggregateEstimate { .. })
        ));
    }

    #[test]
    fn rejects_wrong_provenance_kind_duplicate_and_malformed_paths() {
        let mut wrong = document("docs/adr/a.md", DocumentKind::Prd, "x");
        wrong.inclusion = InclusionReason::DirectReference {
            referenced_by: "docs/prds/work.md".into(),
        };
        assert!(matches!(
            ContextBudgeter.budget(
                context(vec![wrong]),
                ContextBudget {
                    hard_ceiling_tokens: u64::MAX
                }
            ),
            Err(ContextBudgetError::InvalidDirectDocument { .. })
        ));
        let duplicate = document("docs/prds/work.md", DocumentKind::Adr, "x");
        assert!(matches!(
            ContextBudgeter.budget(
                context(vec![duplicate]),
                ContextBudget {
                    hard_ceiling_tokens: u64::MAX
                }
            ),
            Err(ContextBudgetError::DuplicatePath { .. })
        ));
        let malformed = document("docs/adr/../a.md", DocumentKind::Adr, "x");
        assert!(matches!(
            ContextBudgeter.budget(
                context(vec![malformed]),
                ContextBudget {
                    hard_ceiling_tokens: u64::MAX
                }
            ),
            Err(ContextBudgetError::MalformedPath { .. })
        ));
    }
}
