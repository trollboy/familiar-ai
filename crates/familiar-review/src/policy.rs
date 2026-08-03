use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockingPolicy {
    pub blocking_severities: BTreeSet<FindingSeverity>,
    pub blocking_categories: BTreeSet<FindingCategory>,
}

impl Default for BlockingPolicy {
    fn default() -> Self {
        Self {
            blocking_severities: [FindingSeverity::Critical, FindingSeverity::High].into(),
            blocking_categories: [
                FindingCategory::InvariantViolation,
                FindingCategory::ArchitecturalDrift,
                FindingCategory::SecurityIssue,
                FindingCategory::ScopeViolation,
            ]
            .into(),
        }
    }
}

impl BlockingPolicy {
    pub fn is_blocking(&self, category: FindingCategory, severity: FindingSeverity) -> bool {
        self.blocking_categories.contains(&category) || self.blocking_severities.contains(&severity)
    }
    pub fn apply_and_validate(
        &self,
        request: &ReviewRequest,
        mut result: ReviewResult,
    ) -> Result<ReviewResult, ReviewValidationError> {
        if result.review_id != request.review_id {
            return Err(ReviewValidationError::ReviewIdMismatch);
        }
        if result.reviewed_manifest_hash != request.manifest.manifest_hash {
            return Err(ReviewValidationError::ManifestMismatch);
        }
        if request
            .verification
            .iter()
            .any(|evidence| evidence.tested_identity != request.diff.content_hash)
        {
            return Err(ReviewValidationError::StaleVerification);
        }
        let paths: BTreeSet<_> = request
            .changed_files
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        let checks: BTreeSet<_> = request
            .verification
            .iter()
            .map(|v| v.check_id.as_str())
            .collect();
        let mut ids = BTreeSet::new();
        for finding in &mut result.findings {
            if finding.finding_id.is_empty() || !ids.insert(finding.finding_id.clone()) {
                return Err(ReviewValidationError::DuplicateFindingId);
            }
            if finding.title.trim().is_empty()
                || finding.claim.trim().is_empty()
                || finding.remediation.trim().is_empty()
                || finding.evidence.is_empty()
            {
                return Err(ReviewValidationError::UnsupportedFinding(
                    finding.finding_id.clone(),
                ));
            }
            for evidence in &finding.evidence {
                validate_evidence(evidence, &paths, &checks)?;
            }
            validate_category_evidence(finding)?;
            if finding.status == FindingStatus::AcceptedRisk {
                return Err(ReviewValidationError::AgentAcceptedRisk);
            }
            finding.blocking = self.is_blocking(finding.category, finding.severity);
        }
        for prior in &request.prior_findings {
            let finding = result
                .findings
                .iter()
                .find(|finding| finding.finding_id == prior.finding_id)
                .ok_or(ReviewValidationError::PriorFindingNotDisposed)?;
            if !matches!(
                finding.status,
                FindingStatus::Open
                    | FindingStatus::Resolved
                    | FindingStatus::Superseded
                    | FindingStatus::Invalid
            ) {
                return Err(ReviewValidationError::PriorFindingNotDisposed);
            }
            if finding.status == FindingStatus::Superseded && finding.supersedes.is_none() {
                return Err(ReviewValidationError::PriorFindingNotDisposed);
            }
        }
        result.disposition = if result
            .findings
            .iter()
            .any(|f| f.blocking && f.status == FindingStatus::Open)
        {
            ReviewDisposition::RemediationRequired
        } else {
            ReviewDisposition::ReadyForHumanApproval
        };
        Ok(result)
    }
}

fn validate_category_evidence(finding: &ReviewFinding) -> Result<(), ReviewValidationError> {
    let has_file = finding.evidence.iter().any(|e| {
        matches!(
            e,
            FindingEvidence::FileRange { .. } | FindingEvidence::DiffHunk { .. }
        )
    });
    let has_check = finding
        .evidence
        .iter()
        .any(|e| matches!(e, FindingEvidence::Verification { .. }));
    let has_authority = finding.evidence.iter().any(|e| {
        matches!(
            e,
            FindingEvidence::Invariant { .. } | FindingEvidence::Contract { .. }
        )
    });
    let has_artifact = finding
        .evidence
        .iter()
        .any(|e| matches!(e, FindingEvidence::Artifact { .. }));
    let valid = match finding.category {
        FindingCategory::CorrectnessDefect => has_file && has_check,
        FindingCategory::InvariantViolation | FindingCategory::ArchitecturalDrift => {
            has_authority && (has_file || has_check)
        }
        FindingCategory::SecurityIssue => has_file || has_artifact,
        FindingCategory::TestGap => has_file || has_check,
        FindingCategory::MaintainabilityIssue => has_file && (has_authority || has_check),
        FindingCategory::ScopeViolation => has_file,
        FindingCategory::UnverifiableClaim => has_check || has_artifact,
    };
    if valid {
        Ok(())
    } else {
        Err(ReviewValidationError::CategoryEvidenceMismatch(
            finding.finding_id.clone(),
        ))
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
}
fn validate_evidence(
    e: &FindingEvidence,
    paths: &BTreeSet<&str>,
    checks: &BTreeSet<&str>,
) -> Result<(), ReviewValidationError> {
    match e {
        FindingEvidence::FileRange { path, range }
            if valid_path(path)
                && paths.contains(path.as_str())
                && range.start > 0
                && range.end >= range.start =>
        {
            Ok(())
        }
        FindingEvidence::DiffHunk { path, hunk }
            if valid_path(path) && paths.contains(path.as_str()) && hunk.starts_with("@@") =>
        {
            Ok(())
        }
        FindingEvidence::Verification { check_id, output }
            if checks.contains(check_id.as_str()) && !output.content_hash.is_empty() =>
        {
            Ok(())
        }
        FindingEvidence::Invariant {
            source,
            section,
            source_hash,
        }
        | FindingEvidence::Contract {
            source,
            section,
            source_hash,
        } if valid_path(source) && !section.trim().is_empty() && !source_hash.is_empty() => Ok(()),
        FindingEvidence::Artifact { artifact, field }
            if !artifact.content_hash.is_empty() && !field.trim().is_empty() =>
        {
            Ok(())
        }
        _ => Err(ReviewValidationError::InvalidEvidence),
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReviewValidationError {
    #[error("review id does not match request")]
    ReviewIdMismatch,
    #[error("review manifest hash does not match request")]
    ManifestMismatch,
    #[error("verification evidence is not bound to the reviewed diff")]
    StaleVerification,
    #[error("duplicate or empty finding id")]
    DuplicateFindingId,
    #[error("finding {0} lacks a required claim or exact evidence")]
    UnsupportedFinding(String),
    #[error("finding evidence is not resolvable in the review package")]
    InvalidEvidence,
    #[error("an agent cannot accept blocking risk")]
    AgentAcceptedRisk,
    #[error("a prior blocking finding was omitted instead of explicitly disposed")]
    PriorFindingNotDisposed,
    #[error("finding {0} does not contain the minimum evidence required for its category")]
    CategoryEvidenceMismatch(String),
}

pub fn check_independence(
    implementation: &AgentAssignment,
    reviewer: &AgentAssignment,
    allow_same: bool,
    isolation: Option<&AdapterIsolationEvidence>,
) -> Option<ReviewerIndependence> {
    if implementation.adapter_id == reviewer.adapter_id
        && implementation.session_id.is_some()
        && implementation.session_id == reviewer.session_id
    {
        return None;
    }
    let distinct = implementation.provider != reviewer.provider
        || implementation.requested_model != reviewer.requested_model;
    if distinct {
        return Some(ReviewerIndependence {
            kind: IndependenceKind::IndependentProviderOrModel,
            evidence: vec!["configured provider or model differs".into()],
        });
    }
    if allow_same
        && isolation.is_some_and(|evidence| {
            evidence.fresh_process_per_execution && evidence.adapter_id == reviewer.adapter_id
        })
    {
        return Some(ReviewerIndependence {
            kind: IndependenceKind::IsolatedSameModelFallback,
            evidence: vec![isolation
                .expect("checked isolation evidence")
                .detail
                .clone()],
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_matrix_blocks_required_values() {
        let p = BlockingPolicy::default();
        for category in [
            FindingCategory::InvariantViolation,
            FindingCategory::ArchitecturalDrift,
            FindingCategory::SecurityIssue,
            FindingCategory::ScopeViolation,
        ] {
            assert!(p.is_blocking(category, FindingSeverity::Informational));
        }
        assert!(p.is_blocking(FindingCategory::TestGap, FindingSeverity::High));
        assert!(!p.is_blocking(FindingCategory::TestGap, FindingSeverity::Medium));
    }
}
