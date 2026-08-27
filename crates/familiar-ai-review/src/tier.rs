use serde::{Deserialize, Serialize};

use crate::{ChangedFile, GitChangeKind, ScopeCheckResult, ScopeDecision, ScopeFileClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewTier {
    ChecksOnly,
    Standard,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewTierRule {
    pub id: String,
    pub tier: ReviewTier,
    pub path_prefixes: Vec<String>,
    pub max_changed_files: Option<u64>,
    pub max_changed_bytes: Option<u64>,
    pub change_kinds: Vec<GitChangeKind>,
    pub scope_classes: Vec<ScopeFileClass>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewTierPolicy {
    pub rules: Vec<ReviewTierRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFootprint {
    pub changed_files: u64,
    pub changed_bytes: u64,
    pub files: Vec<String>,
    pub change_kinds: Vec<GitChangeKind>,
    pub scope_classes: Vec<ScopeFileClass>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewTierSelection {
    pub tier: ReviewTier,
    pub selecting_rule: Option<String>,
    pub reason: String,
    pub footprint: ReviewFootprint,
}

pub fn select_review_tier(
    policy: &ReviewTierPolicy,
    files: &[ChangedFile],
    changed_bytes: u64,
    scope: &ScopeCheckResult,
) -> ReviewTierSelection {
    let mut kinds: Vec<_> = files.iter().map(|file| file.kind).collect();
    kinds.sort();
    kinds.dedup();
    let mut classes: Vec<_> = scope
        .findings
        .iter()
        .map(|finding| finding.file_class)
        .collect();
    classes.sort();
    classes.dedup();
    let footprint = ReviewFootprint {
        changed_files: u64::try_from(files.len()).unwrap_or(u64::MAX),
        changed_bytes,
        files: files.iter().map(|file| file.path.clone()).collect(),
        change_kinds: kinds,
        scope_classes: classes,
    };
    let unknown_or_risky = files.is_empty()
        || scope.findings.len() != files.len()
        || scope.findings.iter().any(|finding| {
            matches!(
                finding.file_class,
                ScopeFileClass::Ambiguous
                    | ScopeFileClass::DependencyManifest
                    | ScopeFileClass::DependencyLockfile
                    | ScopeFileClass::Migration
                    | ScopeFileClass::Configuration
                    | ScopeFileClass::GeneratedArtifact
            ) || !matches!(
                finding.decision,
                ScopeDecision::AllowedChange | ScopeDecision::JustifiedExpectedFileChange
            )
        });
    if unknown_or_risky {
        return ReviewTierSelection {
            tier: ReviewTier::Full,
            selecting_rule: None,
            reason: "unknown, ambiguous, or high-risk footprint".into(),
            footprint,
        };
    }
    for rule in &policy.rules {
        let matches = rule
            .max_changed_files
            .map_or(true, |max| footprint.changed_files <= max)
            && rule
                .max_changed_bytes
                .map_or(true, |max| footprint.changed_bytes <= max)
            && (rule.path_prefixes.is_empty()
                || files.iter().all(|file| {
                    rule.path_prefixes.iter().any(|prefix| {
                        file.path == *prefix
                            || (prefix.ends_with('/') && file.path.starts_with(prefix))
                    })
                }))
            && (rule.change_kinds.is_empty()
                || files
                    .iter()
                    .all(|file| rule.change_kinds.contains(&file.kind)))
            && (rule.scope_classes.is_empty()
                || footprint
                    .scope_classes
                    .iter()
                    .all(|class| rule.scope_classes.contains(class)));
        if matches {
            return ReviewTierSelection {
                tier: rule.tier,
                selecting_rule: Some(rule.id.clone()),
                reason: "first matching operator-authored rule".into(),
                footprint,
            };
        }
    }
    ReviewTierSelection {
        tier: ReviewTier::Full,
        selecting_rule: None,
        reason: "unconfigured or unmatched footprint".into(),
        footprint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScopeDisposition, ScopeFinding, ScopeRuleSource};

    fn file() -> ChangedFile {
        ChangedFile {
            path: "tests/small.rs".into(),
            kind: GitChangeKind::Modified,
            old_path: None,
            line_summary: vec![],
        }
    }
    fn scope(class: ScopeFileClass) -> ScopeCheckResult {
        ScopeCheckResult {
            added: vec![],
            modified: vec!["tests/small.rs".into()],
            deleted: vec![],
            renamed: vec![],
            disposition: ScopeDisposition::Contained,
            findings: vec![ScopeFinding {
                finding_id: "f".into(),
                change_id: "c".into(),
                path: "tests/small.rs".into(),
                old_path: None,
                change_kind: GitChangeKind::Modified,
                file_class: class,
                decision: ScopeDecision::AllowedChange,
                rule_id: "allowed".into(),
                rule_source: ScopeRuleSource::Configuration,
                rule_detail: "test".into(),
                expected_file_match: None,
                allowed_path_match: Some("tests/".into()),
                prohibited_rule_match: None,
                policy_snapshot_hash: "p".into(),
            }],
            policy_snapshot_hash: "p".into(),
            phase: "initial".into(),
        }
    }
    fn rule(tier: ReviewTier) -> ReviewTierRule {
        ReviewTierRule {
            id: "small-tests".into(),
            tier,
            path_prefixes: vec!["tests/".into()],
            max_changed_files: Some(1),
            max_changed_bytes: Some(100),
            change_kinds: vec![GitChangeKind::Modified],
            scope_classes: vec![ScopeFileClass::Test],
        }
    }

    #[test]
    fn absent_and_unmatched_policy_is_full() {
        let selected = select_review_tier(
            &ReviewTierPolicy::default(),
            &[file()],
            10,
            &scope(ScopeFileClass::Test),
        );
        assert_eq!(selected.tier, ReviewTier::Full);
        assert_eq!(selected.selecting_rule, None);
    }

    #[test]
    fn matching_rule_selects_checks_only_with_exact_footprint() {
        let selected = select_review_tier(
            &ReviewTierPolicy {
                rules: vec![rule(ReviewTier::ChecksOnly)],
            },
            &[file()],
            10,
            &scope(ScopeFileClass::Test),
        );
        assert_eq!(selected.tier, ReviewTier::ChecksOnly);
        assert_eq!(selected.selecting_rule.as_deref(), Some("small-tests"));
        assert_eq!(selected.footprint.changed_bytes, 10);
    }

    #[test]
    fn high_risk_class_overrides_matching_rule_to_full() {
        let mut permissive = rule(ReviewTier::Standard);
        permissive.scope_classes = vec![ScopeFileClass::Migration];
        let selected = select_review_tier(
            &ReviewTierPolicy {
                rules: vec![permissive],
            },
            &[file()],
            10,
            &scope(ScopeFileClass::Migration),
        );
        assert_eq!(selected.tier, ReviewTier::Full);
        assert_eq!(selected.selecting_rule, None);
    }
}
