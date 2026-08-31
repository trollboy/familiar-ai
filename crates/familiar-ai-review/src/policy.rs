use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::*;

pub const SCOPE_POLICY_SCHEMA_VERSION: &str = "scope-policy/1";
pub const BUILTIN_SCOPE_RULES_VERSION: &str = "builtin-scope-rules/1";

const BUILTIN_DEPENDENCY_MANIFESTS: [&str; 5] = [
    "Cargo.toml",
    "package.json",
    "go.mod",
    "requirements.txt",
    "pyproject.toml",
];
const BUILTIN_DEPENDENCY_LOCKFILES: [&str; 6] = [
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.sum",
    "poetry.lock",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePolicyInput {
    pub prd_path: String,
    pub prd_content_hash: String,
    pub contract: Vec<ExpectedFileEntry>,
    pub allowed_paths: Vec<String>,
    pub allow_prd_expected_file_expansion: bool,
    pub declaration_mode: ScopeDeclarationMode,
    pub prohibited_rules: Vec<ProhibitedRule>,
    pub file_class_policies: ScopeFileClassPolicies,
    pub classification_rules: Vec<ScopeClassificationRule>,
    pub baseline_revision: String,
    pub config_provenance: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScopePolicyError {
    #[error("scope policy requires allowed paths or enabled PRD expansion with a non-empty Expected Files contract")]
    NoAuthoritySource,
    #[error("invalid allowed path '{value}': {rule}")]
    InvalidAllowedPath { value: String, rule: ScopePathRule },
    #[error("duplicate allowed path '{normalized}'")]
    DuplicateAllowedPath { normalized: String },
    #[error("allowed path '{inner}' is shadowed by allowed directory '{outer}'")]
    ShadowedAllowedPath { outer: String, inner: String },
    #[error("duplicate scope rule id '{rule_id}'")]
    DuplicateRuleId { rule_id: String },
    #[error("prohibited rules '{first}' and '{second}' declare the same subject")]
    DuplicateProhibitedRule { first: String, second: String },
    #[error("classification rules '{first}' and '{second}' assign conflicting classes to '{entry}' without unique precedence")]
    ConflictingClassificationRules {
        first: String,
        second: String,
        entry: String,
    },
    #[error("scope policy serialization failed: {0}")]
    Serialization(String),
}

pub fn compile_scope_policy(
    input: ScopePolicyInput,
) -> Result<ScopePolicySnapshot, ScopePolicyError> {
    let mut allowed = Vec::new();
    for value in &input.allowed_paths {
        let (normalized, match_kind) =
            normalize_scope_path(value).map_err(|rule| ScopePolicyError::InvalidAllowedPath {
                value: value.clone(),
                rule,
            })?;
        if allowed
            .iter()
            .any(|entry: &ScopePathEntry| entry.normalized == normalized)
        {
            return Err(ScopePolicyError::DuplicateAllowedPath { normalized });
        }
        allowed.push(ScopePathEntry {
            normalized,
            match_kind,
        });
    }
    allowed.sort_by(|a, b| a.normalized.cmp(&b.normalized));
    for outer in &allowed {
        if outer.match_kind != ExpectedMatchKind::Directory {
            continue;
        }
        if let Some(inner) = allowed
            .iter()
            .find(|inner| inner.normalized != outer.normalized && outer.matches(&inner.normalized))
        {
            return Err(ScopePolicyError::ShadowedAllowedPath {
                outer: outer.normalized.clone(),
                inner: inner.normalized.clone(),
            });
        }
    }
    if allowed.is_empty() && (!input.allow_prd_expected_file_expansion || input.contract.is_empty())
    {
        return Err(ScopePolicyError::NoAuthoritySource);
    }
    let mut rule_ids = BTreeSet::new();
    for rule_id in input
        .prohibited_rules
        .iter()
        .map(|rule| &rule.rule_id)
        .chain(input.classification_rules.iter().map(|rule| &rule.rule_id))
    {
        if !rule_ids.insert(rule_id.clone()) {
            return Err(ScopePolicyError::DuplicateRuleId {
                rule_id: rule_id.clone(),
            });
        }
    }
    let mut prohibited = input.prohibited_rules.clone();
    prohibited.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
    for (index, first) in prohibited.iter().enumerate() {
        if let Some(second) = prohibited[index + 1..]
            .iter()
            .find(|second| second.rule == first.rule && second.change_kinds == first.change_kinds)
        {
            return Err(ScopePolicyError::DuplicateProhibitedRule {
                first: first.rule_id.clone(),
                second: second.rule_id.clone(),
            });
        }
    }
    let mut classification = input.classification_rules.clone();
    classification.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
    for (index, first) in classification.iter().enumerate() {
        if let Some(second) = classification[index + 1..].iter().find(|second| {
            second.entry == first.entry
                && second.class != first.class
                && second.precedence == first.precedence
        }) {
            return Err(ScopePolicyError::ConflictingClassificationRules {
                first: first.rule_id.clone(),
                second: second.rule_id.clone(),
                entry: first.entry.normalized.clone(),
            });
        }
    }
    let mut snapshot = ScopePolicySnapshot {
        schema_version: SCOPE_POLICY_SCHEMA_VERSION.into(),
        prd_path: input.prd_path,
        prd_content_hash: input.prd_content_hash,
        contract: input.contract,
        allowed_paths: allowed,
        allow_prd_expected_file_expansion: input.allow_prd_expected_file_expansion,
        declaration_mode: input.declaration_mode,
        prohibited_rules: prohibited,
        file_class_policies: input.file_class_policies,
        classification_rules: classification,
        builtin_rules_version: BUILTIN_SCOPE_RULES_VERSION.into(),
        baseline_revision: input.baseline_revision,
        config_provenance: input.config_provenance,
        snapshot_hash: String::new(),
    };
    let bytes = serde_json::to_vec(&snapshot)
        .map_err(|error| ScopePolicyError::Serialization(error.to_string()))?;
    snapshot.snapshot_hash = content_hash(&bytes);
    Ok(snapshot)
}

/// Deterministic filesystem metadata collected for scope evaluation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeEvidence {
    pub symlink_paths: BTreeSet<String>,
}

struct Classified {
    class: ScopeFileClass,
    rule_id: String,
    source: ScopeRuleSource,
    detail: String,
}

fn classify(snapshot: &ScopePolicySnapshot, path: &str) -> Classified {
    let configured: Vec<&ScopeClassificationRule> = snapshot
        .classification_rules
        .iter()
        .filter(|rule| rule.entry.matches(path))
        .collect();
    if !configured.is_empty() {
        let classes: BTreeSet<ScopeFileClass> = configured.iter().map(|rule| rule.class).collect();
        if classes.len() == 1 {
            let rule = configured[0];
            return Classified {
                class: rule.class,
                rule_id: rule.rule_id.clone(),
                source: ScopeRuleSource::Configuration,
                detail: format!(
                    "configured classification rule matched '{}'",
                    rule.entry.normalized
                ),
            };
        }
        let best = configured
            .iter()
            .max_by_key(|rule| rule.precedence.unwrap_or(0))
            .expect("non-empty configured rules");
        let top = best.precedence.unwrap_or(0);
        let contenders: Vec<_> = configured
            .iter()
            .filter(|rule| rule.precedence.unwrap_or(0) == top)
            .collect();
        let top_classes: BTreeSet<ScopeFileClass> =
            contenders.iter().map(|rule| rule.class).collect();
        if top_classes.len() == 1 {
            return Classified {
                class: best.class,
                rule_id: best.rule_id.clone(),
                source: ScopeRuleSource::Configuration,
                detail: format!(
                    "configured precedence {top} resolved conflicting classification for '{}'",
                    best.entry.normalized
                ),
            };
        }
        return Classified {
            class: ScopeFileClass::Ambiguous,
            rule_id: "classification:conflict".into(),
            source: ScopeRuleSource::Configuration,
            detail: format!(
                "conflicting configured classes {top_classes:?} match without unique precedence"
            ),
        };
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    let mut matches: Vec<(ScopeFileClass, String)> = Vec::new();
    if BUILTIN_DEPENDENCY_MANIFESTS.contains(&basename) {
        matches.push((
            ScopeFileClass::DependencyManifest,
            format!("builtin:dependency_manifest:{basename}"),
        ));
    }
    if BUILTIN_DEPENDENCY_LOCKFILES.contains(&basename) {
        matches.push((
            ScopeFileClass::DependencyLockfile,
            format!("builtin:dependency_lockfile:{basename}"),
        ));
    }
    if path.split('/').rev().skip(1).any(|part| part == "tests") {
        matches.push((ScopeFileClass::Test, "builtin:test_directory".into()));
    }
    match matches.len() {
        0 => Classified {
            class: ScopeFileClass::OrdinarySource,
            rule_id: "classification:ordinary_source".into(),
            source: ScopeRuleSource::BuiltIn,
            detail: "no special-class rule matched".into(),
        },
        1 => {
            let (class, rule_id) = matches.remove(0);
            Classified {
                class,
                rule_id,
                source: ScopeRuleSource::BuiltIn,
                detail: format!("built-in exact rule matched basename '{basename}'"),
            }
        }
        _ => Classified {
            class: ScopeFileClass::Ambiguous,
            rule_id: "classification:conflict".into(),
            source: ScopeRuleSource::BuiltIn,
            detail: format!(
                "conflicting built-in classes {:?} match",
                matches.iter().map(|(class, _)| *class).collect::<Vec<_>>()
            ),
        },
    }
}

struct Subject {
    change_id: String,
    side: &'static str,
    path: String,
    old_path: Option<String>,
    kind: GitChangeKind,
}

fn file_class_label(class: ScopeFileClass) -> &'static str {
    match class {
        ScopeFileClass::OrdinarySource => "ordinary_source",
        ScopeFileClass::DependencyManifest => "dependency_manifest",
        ScopeFileClass::DependencyLockfile => "dependency_lockfile",
        ScopeFileClass::Migration => "migration",
        ScopeFileClass::Configuration => "configuration",
        ScopeFileClass::Test => "test",
        ScopeFileClass::GeneratedArtifact => "generated_artifact",
        ScopeFileClass::Ambiguous => "ambiguous",
    }
}

fn change_kind_label(kind: GitChangeKind) -> &'static str {
    match kind {
        GitChangeKind::Added => "added",
        GitChangeKind::Modified => "modified",
        GitChangeKind::Deleted => "deleted",
        GitChangeKind::Renamed => "renamed",
        GitChangeKind::Copied => "copied",
        GitChangeKind::TypeChanged => "type_changed",
        GitChangeKind::Unmerged => "unmerged",
    }
}

fn precedence_step(decision: ScopeDecision, rule_id: &str) -> u8 {
    if rule_id.starts_with("evidence:") || rule_id.starts_with("classification:conflict") {
        1
    } else if rule_id.starts_with("prohibited:") {
        2
    } else if rule_id.starts_with("file_class:") {
        3
    } else if rule_id == "static_allowed_path_ceiling" {
        4
    } else {
        match decision {
            ScopeDecision::AllowedChange | ScopeDecision::JustifiedExpectedFileChange => 5,
            _ => 7,
        }
    }
}

pub fn evaluate_scope(
    snapshot: &ScopePolicySnapshot,
    files: &[ChangedFile],
    evidence: &ScopeEvidence,
) -> ScopeEvaluation {
    let mut findings = Vec::new();
    for file in files {
        let change_id = format!(
            "{}:{}:{}",
            change_kind_label(file.kind),
            file.old_path.as_deref().unwrap_or("-"),
            file.path
        );
        let mut subjects = vec![Subject {
            change_id: change_id.clone(),
            side: "new",
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            kind: file.kind,
        }];
        if matches!(file.kind, GitChangeKind::Renamed | GitChangeKind::Copied) {
            if let Some(old_path) = &file.old_path {
                subjects.push(Subject {
                    change_id: change_id.clone(),
                    side: "old",
                    path: old_path.clone(),
                    old_path: file.old_path.clone(),
                    kind: file.kind,
                });
            }
        }
        for subject in subjects {
            findings.push(evaluate_subject(snapshot, evidence, &subject));
        }
    }
    findings.sort_by(|a, b| {
        let key = |finding: &ScopeFinding| {
            (
                finding.path.clone(),
                finding.old_path.clone(),
                finding.change_kind,
                precedence_step(finding.decision, &finding.rule_id),
                finding.rule_id.clone(),
                finding.finding_id.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
    let disposition = if findings.iter().any(|finding| {
        matches!(
            finding.decision,
            ScopeDecision::ProhibitedChange | ScopeDecision::UndeclaredScopeExpansion
        )
    }) {
        ScopeDisposition::Broadened
    } else if findings
        .iter()
        .any(|finding| finding.decision == ScopeDecision::AmbiguousHumanReview)
    {
        ScopeDisposition::HumanReviewRequired
    } else {
        ScopeDisposition::Contained
    };
    ScopeEvaluation {
        findings,
        disposition,
    }
}

fn evaluate_subject(
    snapshot: &ScopePolicySnapshot,
    evidence: &ScopeEvidence,
    subject: &Subject,
) -> ScopeFinding {
    let classified = classify(snapshot, &subject.path);
    let expected_match = snapshot
        .contract
        .iter()
        .find(|entry| entry.matches(&subject.path))
        .map(|entry| ExpectedFileMatch {
            normalized: entry.normalized.clone(),
            source_line: entry.source_line,
            match_kind: entry.match_kind,
        });
    let allowed_match = snapshot
        .allowed_paths
        .iter()
        .find(|entry| entry.matches(&subject.path))
        .map(|entry| entry.normalized.clone());
    let finding = |decision: ScopeDecision,
                   rule_id: String,
                   rule_source: ScopeRuleSource,
                   rule_detail: String,
                   prohibited: Option<String>| ScopeFinding {
        finding_id: format!("{}#{}", subject.change_id, subject.side),
        change_id: subject.change_id.clone(),
        path: subject.path.clone(),
        old_path: subject.old_path.clone(),
        change_kind: subject.kind,
        file_class: classified.class,
        decision,
        rule_id,
        rule_source,
        rule_detail,
        expected_file_match: expected_match.clone(),
        allowed_path_match: allowed_match.clone(),
        prohibited_rule_match: prohibited,
        policy_snapshot_hash: snapshot.snapshot_hash.clone(),
    };

    // Step 1: invalid or incomplete evidence.
    if !valid_repository_path(&subject.path) {
        return finding(
            ScopeDecision::AmbiguousHumanReview,
            "evidence:unsafe_path".into(),
            ScopeRuleSource::EvidenceValidation,
            "path is not a safe repository-relative path".into(),
            None,
        );
    }
    if matches!(subject.kind, GitChangeKind::Renamed | GitChangeKind::Copied)
        && subject.old_path.is_none()
    {
        return finding(
            ScopeDecision::AmbiguousHumanReview,
            "evidence:missing_old_path".into(),
            ScopeRuleSource::EvidenceValidation,
            "rename or copy without a recorded source path".into(),
            None,
        );
    }
    if subject.kind == GitChangeKind::TypeChanged {
        return finding(
            ScopeDecision::AmbiguousHumanReview,
            "evidence:type_changed".into(),
            ScopeRuleSource::EvidenceValidation,
            "file type changes require human review".into(),
            None,
        );
    }
    if subject.kind == GitChangeKind::Unmerged {
        return finding(
            ScopeDecision::AmbiguousHumanReview,
            "evidence:unmerged".into(),
            ScopeRuleSource::EvidenceValidation,
            "unmerged paths require human review".into(),
            None,
        );
    }
    if evidence.symlink_paths.contains(&subject.path) {
        return finding(
            ScopeDecision::AmbiguousHumanReview,
            "evidence:symlink".into(),
            ScopeRuleSource::EvidenceValidation,
            "symlinks are never authorized by path declarations".into(),
            None,
        );
    }
    if classified.class == ScopeFileClass::Ambiguous {
        return finding(
            ScopeDecision::AmbiguousHumanReview,
            classified.rule_id.clone(),
            classified.source,
            classified.detail.clone(),
            None,
        );
    }

    // Step 2: explicit prohibition. Expected Files and allowed paths cannot override it.
    for rule in &snapshot.prohibited_rules {
        if !rule.change_kinds.is_empty() && !rule.change_kinds.contains(&subject.kind) {
            continue;
        }
        let matched = match &rule.rule {
            ProhibitedRuleKind::Path { entry } => entry.matches(&subject.path),
            ProhibitedRuleKind::FileClass { class } => *class == classified.class,
        };
        if matched {
            return finding(
                ScopeDecision::ProhibitedChange,
                format!("prohibited:{}", rule.rule_id),
                ScopeRuleSource::Configuration,
                rule.description
                    .clone()
                    .unwrap_or_else(|| "explicitly prohibited change".into()),
                Some(rule.rule_id.clone()),
            );
        }
    }

    // Copy sources are read, not changed; past prohibition and evidence checks they are authorized.
    if subject.kind == GitChangeKind::Copied && subject.side == "old" {
        return finding(
            ScopeDecision::AllowedChange,
            "copy_source_read".into(),
            ScopeRuleSource::BuiltIn,
            "copy source is read-only and not prohibited".into(),
            None,
        );
    }

    // Step 3: special-file class policy gate.
    if classified.class != ScopeFileClass::OrdinarySource {
        let policy = match classified.class {
            ScopeFileClass::DependencyManifest => snapshot.file_class_policies.dependency_manifest,
            ScopeFileClass::DependencyLockfile => snapshot.file_class_policies.dependency_lockfile,
            ScopeFileClass::Migration => snapshot.file_class_policies.migration,
            ScopeFileClass::Configuration => snapshot.file_class_policies.configuration,
            ScopeFileClass::Test => snapshot.file_class_policies.test,
            ScopeFileClass::GeneratedArtifact => snapshot.file_class_policies.generated_artifact,
            ScopeFileClass::OrdinarySource | ScopeFileClass::Ambiguous => {
                unreachable!("handled above")
            }
        };
        let class_label = file_class_label(classified.class);
        match policy {
            ScopeClassPolicy::Deny => {
                return finding(
                    ScopeDecision::ProhibitedChange,
                    format!("file_class:{class_label}:deny"),
                    ScopeRuleSource::Configuration,
                    format!("class policy denies changes to {class_label} files"),
                    None,
                );
            }
            ScopeClassPolicy::HumanReview => {
                return finding(
                    ScopeDecision::AmbiguousHumanReview,
                    format!("file_class:{class_label}:human_review"),
                    ScopeRuleSource::Configuration,
                    format!("class policy requires human review for {class_label} files"),
                    None,
                );
            }
            ScopeClassPolicy::AllowWhenExpected if expected_match.is_none() => {
                return finding(
                    ScopeDecision::UndeclaredScopeExpansion,
                    format!("file_class:{class_label}:allow_when_expected"),
                    ScopeRuleSource::Configuration,
                    format!(
                        "class policy permits {class_label} files only when declared in Expected Files"
                    ),
                    None,
                );
            }
            ScopeClassPolicy::AllowWhenConfigured if allowed_match.is_none() => {
                return finding(
                    ScopeDecision::UndeclaredScopeExpansion,
                    format!("file_class:{class_label}:allow_when_configured"),
                    ScopeRuleSource::Configuration,
                    format!(
                        "class policy permits {class_label} files only within configured allowed paths"
                    ),
                    None,
                );
            }
            _ => {}
        }
    }

    // Steps 4-7: static ceiling, PRD declaration, configured-only path, no match.
    match (&allowed_match, &expected_match) {
        (Some(allowed), Some(_)) => finding(
            ScopeDecision::AllowedChange,
            "expected_file_declaration".into(),
            ScopeRuleSource::ExpectedFiles,
            format!("declared in Expected Files within allowed path '{allowed}'"),
            None,
        ),
        (Some(allowed), None) => match snapshot.declaration_mode {
            ScopeDeclarationMode::ExpectedOrConfigured => finding(
                ScopeDecision::AllowedChange,
                "configured_allowed_path".into(),
                ScopeRuleSource::Configuration,
                format!("within configured allowed path '{allowed}'"),
                None,
            ),
            ScopeDeclarationMode::ExpectedRequired => finding(
                ScopeDecision::UndeclaredScopeExpansion,
                "expected_file_declaration_required".into(),
                ScopeRuleSource::Configuration,
                "declaration mode requires an Expected Files match".into(),
                None,
            ),
        },
        (None, Some(matched)) if snapshot.allow_prd_expected_file_expansion => finding(
            ScopeDecision::JustifiedExpectedFileChange,
            "prd_expected_file_expansion".into(),
            ScopeRuleSource::ExpectedFiles,
            format!(
                "outside static allowed paths but declared at Expected Files line {}",
                matched.source_line
            ),
            None,
        ),
        (None, Some(matched)) => finding(
            ScopeDecision::UndeclaredScopeExpansion,
            "static_allowed_path_ceiling".into(),
            ScopeRuleSource::Configuration,
            format!(
                "declared at Expected Files line {} but PRD expansion is disabled",
                matched.source_line
            ),
            None,
        ),
        (None, None) => finding(
            ScopeDecision::UndeclaredScopeExpansion,
            "undeclared_change".into(),
            ScopeRuleSource::Configuration,
            "matches no Expected Files entry and no configured allowed path".into(),
            None,
        ),
    }
}

fn valid_repository_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

/// Build the persisted check result for one evaluation phase, retaining the
/// legacy path summary alongside the canonical findings.
pub fn scope_check_result(
    files: &[ChangedFile],
    evaluation: &ScopeEvaluation,
    snapshot: &ScopePolicySnapshot,
    phase: &str,
) -> ScopeCheckResult {
    let mut result = ScopeCheckResult {
        added: vec![],
        modified: vec![],
        deleted: vec![],
        renamed: vec![],
        disposition: evaluation.disposition,
        findings: evaluation.findings.clone(),
        policy_snapshot_hash: snapshot.snapshot_hash.clone(),
        phase: phase.into(),
    };
    for file in files {
        match file.kind {
            GitChangeKind::Added => result.added.push(file.path.clone()),
            GitChangeKind::Deleted => result.deleted.push(file.path.clone()),
            GitChangeKind::Renamed => result
                .renamed
                .push((file.old_path.clone().unwrap_or_default(), file.path.clone())),
            _ => result.modified.push(file.path.clone()),
        }
    }
    result
}

pub fn scope_rule_summary(
    snapshot: &ScopePolicySnapshot,
    blocking_scope_findings: Vec<ScopeFinding>,
) -> ScopeRuleSummary {
    ScopeRuleSummary {
        policy_snapshot_hash: snapshot.snapshot_hash.clone(),
        authorized_paths: snapshot.allowed_paths.clone(),
        expected_files: snapshot.contract.clone(),
        prohibited_rules: snapshot.prohibited_rules.clone(),
        file_class_policies: snapshot.file_class_policies.clone(),
        blocking_scope_findings,
    }
}

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
        let failed_required_check = request
            .verification
            .iter()
            .find(|e| e.required && e.status != VerificationStatus::Passed);
        let durable_disposition = if failed_required_check.is_some()
            || result.findings.iter().any(|f| {
                f.status == FindingStatus::Open
                    && (f.blocking || f.acceptance_criterion_id.is_some())
            }) {
            ReviewDisposition::RemediationRequired
        } else {
            ReviewDisposition::ReadyForHumanApproval
        };
        if result.disposition != ReviewDisposition::Pending
            && result.disposition != durable_disposition
        {
            let check = failed_required_check.map_or("review findings", |e| e.check_id.as_str());
            return Err(ReviewValidationError::NarrationContradiction(check.into()));
        }
        result.disposition = durable_disposition;
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
    #[error("agent narration contradicts durable check {0}")]
    NarrationContradiction(String),
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

    fn contract_entry(normalized: &str) -> ExpectedFileEntry {
        let match_kind = if normalized.ends_with('/') {
            ExpectedMatchKind::Directory
        } else {
            ExpectedMatchKind::ExactFile
        };
        ExpectedFileEntry {
            source_line: 1,
            bullet_text: format!("- `{normalized}`"),
            normalized: normalized.into(),
            match_kind,
        }
    }
    fn base_input() -> ScopePolicyInput {
        ScopePolicyInput {
            prd_path: "docs/prds/PRD-000.md".into(),
            prd_content_hash: "sha256:prd".into(),
            contract: vec![
                contract_entry("docs/design.md"),
                contract_entry("crates/extra/"),
            ],
            allowed_paths: vec!["src/".into()],
            allow_prd_expected_file_expansion: false,
            declaration_mode: ScopeDeclarationMode::ExpectedOrConfigured,
            prohibited_rules: vec![
                ProhibitedRule {
                    rule_id: "no_secrets".into(),
                    rule: ProhibitedRuleKind::Path {
                        entry: ScopePathEntry {
                            normalized: "secrets/".into(),
                            match_kind: ExpectedMatchKind::Directory,
                        },
                    },
                    change_kinds: vec![],
                    description: Some("secret material".into()),
                },
                ProhibitedRule {
                    rule_id: "no_manifests".into(),
                    rule: ProhibitedRuleKind::FileClass {
                        class: ScopeFileClass::DependencyManifest,
                    },
                    change_kinds: vec![],
                    description: None,
                },
            ],
            file_class_policies: ScopeFileClassPolicies::default(),
            classification_rules: vec![ScopeClassificationRule {
                rule_id: "storage_migrations".into(),
                class: ScopeFileClass::Migration,
                entry: ScopePathEntry {
                    normalized: "migrations/".into(),
                    match_kind: ExpectedMatchKind::Directory,
                },
                source: ScopeRuleSource::Configuration,
                precedence: None,
            }],
            baseline_revision: "base".into(),
            config_provenance: "test".into(),
        }
    }
    fn snapshot(mutator: impl FnOnce(&mut ScopePolicyInput)) -> ScopePolicySnapshot {
        let mut input = base_input();
        mutator(&mut input);
        compile_scope_policy(input).unwrap()
    }
    fn file(path: &str, kind: GitChangeKind, old_path: Option<&str>) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            kind,
            old_path: old_path.map(Into::into),
            line_summary: vec![],
        }
    }
    fn decide(
        snapshot: &ScopePolicySnapshot,
        files: &[ChangedFile],
    ) -> Vec<(String, ScopeDecision, String)> {
        evaluate_scope(snapshot, files, &ScopeEvidence::default())
            .findings
            .iter()
            .map(|f| (f.finding_id.clone(), f.decision, f.rule_id.clone()))
            .collect()
    }

    #[test]
    fn all_five_decisions_are_reachable() {
        let policy = snapshot(|input| input.allow_prd_expected_file_expansion = true);
        let evidence = ScopeEvidence::default();
        let evaluation = evaluate_scope(
            &policy,
            &[
                file("src/lib.rs", GitChangeKind::Modified, None),
                file("crates/extra/mod.rs", GitChangeKind::Added, None),
                file("secrets/token", GitChangeKind::Added, None),
                file("random/other.rs", GitChangeKind::Added, None),
                file("src/conflict.rs", GitChangeKind::Unmerged, None),
            ],
            &evidence,
        );
        let by_path: std::collections::BTreeMap<_, _> = evaluation
            .findings
            .iter()
            .map(|f| (f.path.as_str(), f.decision))
            .collect();
        assert_eq!(by_path["src/lib.rs"], ScopeDecision::AllowedChange);
        assert_eq!(
            by_path["crates/extra/mod.rs"],
            ScopeDecision::JustifiedExpectedFileChange
        );
        assert_eq!(by_path["secrets/token"], ScopeDecision::ProhibitedChange);
        assert_eq!(
            by_path["random/other.rs"],
            ScopeDecision::UndeclaredScopeExpansion
        );
        assert_eq!(
            by_path["src/conflict.rs"],
            ScopeDecision::AmbiguousHumanReview
        );
        assert_eq!(evaluation.disposition, ScopeDisposition::Broadened);
    }

    #[test]
    fn prohibition_beats_expected_files_and_allowed_paths_for_every_kind() {
        let policy = snapshot(|input| {
            input.allow_prd_expected_file_expansion = true;
            input.allowed_paths = vec!["secrets/".into()];
            input.contract = vec![contract_entry("secrets/")];
        });
        for kind in [
            GitChangeKind::Added,
            GitChangeKind::Modified,
            GitChangeKind::Deleted,
            GitChangeKind::Renamed,
            GitChangeKind::Copied,
        ] {
            let old = matches!(kind, GitChangeKind::Renamed | GitChangeKind::Copied)
                .then_some("secrets/old");
            let findings = decide(&policy, &[file("secrets/token", kind, old)]);
            assert!(
                findings.iter().all(|(_, decision, rule)| *decision
                    == ScopeDecision::ProhibitedChange
                    && rule == "prohibited:no_secrets"),
                "kind {kind:?} produced {findings:?}"
            );
        }
    }

    #[test]
    fn expansion_disabled_fails_under_static_ceiling_and_retains_expected_match() {
        let policy = snapshot(|_| {});
        let evaluation = evaluate_scope(
            &policy,
            &[file("crates/extra/mod.rs", GitChangeKind::Added, None)],
            &ScopeEvidence::default(),
        );
        let finding = &evaluation.findings[0];
        assert_eq!(finding.decision, ScopeDecision::UndeclaredScopeExpansion);
        assert_eq!(finding.rule_id, "static_allowed_path_ceiling");
        assert_eq!(
            finding.expected_file_match.as_ref().unwrap().normalized,
            "crates/extra/"
        );
        assert_eq!(evaluation.disposition, ScopeDisposition::Broadened);
    }

    #[test]
    fn declaration_modes_gate_configured_only_paths() {
        let relaxed = snapshot(|_| {});
        assert_eq!(
            decide(
                &relaxed,
                &[file("src/only.rs", GitChangeKind::Modified, None)]
            )[0]
            .1,
            ScopeDecision::AllowedChange
        );
        let strict = snapshot(|input| {
            input.declaration_mode = ScopeDeclarationMode::ExpectedRequired;
        });
        let findings = decide(
            &strict,
            &[file("src/only.rs", GitChangeKind::Modified, None)],
        );
        assert_eq!(findings[0].1, ScopeDecision::UndeclaredScopeExpansion);
        assert_eq!(findings[0].2, "expected_file_declaration_required");
    }

    #[test]
    fn special_class_policies_gate_before_path_authority() {
        let policy = snapshot(|input| {
            input.allowed_paths = vec!["src/".into(), "migrations/".into(), "Cargo.lock".into()];
        });
        let lockfile = decide(
            &policy,
            &[file("Cargo.lock", GitChangeKind::Modified, None)],
        );
        assert_eq!(lockfile[0].1, ScopeDecision::AmbiguousHumanReview);
        assert_eq!(lockfile[0].2, "file_class:dependency_lockfile:human_review");
        let migration = decide(
            &policy,
            &[file("migrations/010_x.sql", GitChangeKind::Added, None)],
        );
        assert_eq!(migration[0].1, ScopeDecision::AmbiguousHumanReview);
        assert_eq!(migration[0].2, "file_class:migration:human_review");
        let manifest = decide(
            &policy,
            &[file("Cargo.toml", GitChangeKind::Modified, None)],
        );
        assert_eq!(manifest[0].1, ScopeDecision::ProhibitedChange);
        assert_eq!(manifest[0].2, "prohibited:no_manifests");
    }

    #[test]
    fn manifest_and_lockfile_decisions_are_independent() {
        let policy = snapshot(|input| {
            input.prohibited_rules = vec![];
            input.allowed_paths = vec!["Cargo.toml".into(), "Cargo.lock".into()];
            input.file_class_policies.dependency_manifest = ScopeClassPolicy::Allow;
            input.file_class_policies.dependency_lockfile = ScopeClassPolicy::Deny;
        });
        let findings = decide(
            &policy,
            &[
                file("Cargo.toml", GitChangeKind::Modified, None),
                file("Cargo.lock", GitChangeKind::Modified, None),
            ],
        );
        let by: std::collections::BTreeMap<_, _> =
            findings.iter().map(|(id, d, _)| (id.clone(), *d)).collect();
        assert_eq!(
            by["modified:-:Cargo.toml#new"],
            ScopeDecision::AllowedChange
        );
        assert_eq!(
            by["modified:-:Cargo.lock#new"],
            ScopeDecision::ProhibitedChange
        );
    }

    #[test]
    fn tests_class_allows_only_expected_declarations_by_default() {
        let policy = snapshot(|input| {
            input.contract = vec![contract_entry("crates/x/tests/")];
            input.allow_prd_expected_file_expansion = true;
            input.allowed_paths = vec!["src/".into(), "other/tests/".into()];
        });
        let declared = decide(
            &policy,
            &[file("crates/x/tests/it.rs", GitChangeKind::Added, None)],
        );
        assert_eq!(declared[0].1, ScopeDecision::JustifiedExpectedFileChange);
        let configured_only = decide(
            &policy,
            &[file("other/tests/it.rs", GitChangeKind::Added, None)],
        );
        assert_eq!(
            configured_only[0].1,
            ScopeDecision::UndeclaredScopeExpansion
        );
        assert_eq!(configured_only[0].2, "file_class:test:allow_when_expected");
    }

    #[test]
    fn rename_requires_both_sides_and_copy_source_is_read_only() {
        let policy = snapshot(|_| {});
        let rename = decide(
            &policy,
            &[file(
                "src/new.rs",
                GitChangeKind::Renamed,
                Some("outside/old.rs"),
            )],
        );
        assert_eq!(rename.len(), 2);
        let by: std::collections::BTreeMap<_, _> =
            rename.iter().map(|(id, d, _)| (id.clone(), *d)).collect();
        assert_eq!(
            by["renamed:outside/old.rs:src/new.rs#new"],
            ScopeDecision::AllowedChange
        );
        assert_eq!(
            by["renamed:outside/old.rs:src/new.rs#old"],
            ScopeDecision::UndeclaredScopeExpansion
        );
        let copy = decide(
            &policy,
            &[file(
                "src/copy.rs",
                GitChangeKind::Copied,
                Some("outside/src.rs"),
            )],
        );
        let by: std::collections::BTreeMap<_, _> = copy
            .iter()
            .map(|(id, d, r)| (id.clone(), (*d, r.clone())))
            .collect();
        assert_eq!(
            by["copied:outside/src.rs:src/copy.rs#old"],
            (ScopeDecision::AllowedChange, "copy_source_read".into())
        );
        let missing_old = decide(&policy, &[file("src/new.rs", GitChangeKind::Renamed, None)]);
        assert_eq!(missing_old[0].1, ScopeDecision::AmbiguousHumanReview);
        assert_eq!(missing_old[0].2, "evidence:missing_old_path");
    }

    #[test]
    fn unsafe_evidence_is_ambiguous_never_coerced() {
        let policy = snapshot(|_| {});
        let mut evidence = ScopeEvidence::default();
        evidence.symlink_paths.insert("src/link.rs".into());
        let evaluation = evaluate_scope(
            &policy,
            &[
                file("src/link.rs", GitChangeKind::Added, None),
                file("src/type.rs", GitChangeKind::TypeChanged, None),
                file("src/merge.rs", GitChangeKind::Unmerged, None),
            ],
            &evidence,
        );
        assert!(evaluation
            .findings
            .iter()
            .all(|f| f.decision == ScopeDecision::AmbiguousHumanReview));
        assert_eq!(
            evaluation.disposition,
            ScopeDisposition::HumanReviewRequired
        );
        let rules: Vec<_> = evaluation
            .findings
            .iter()
            .map(|f| f.rule_id.as_str())
            .collect();
        assert!(rules.contains(&"evidence:symlink"));
        assert!(rules.contains(&"evidence:type_changed"));
        assert!(rules.contains(&"evidence:unmerged"));
    }

    #[test]
    fn broadened_is_terminal_over_ambiguity_and_ambiguous_findings_are_retained() {
        let policy = snapshot(|_| {});
        let evaluation = evaluate_scope(
            &policy,
            &[
                file("secrets/token", GitChangeKind::Added, None),
                file("src/merge.rs", GitChangeKind::Unmerged, None),
            ],
            &ScopeEvidence::default(),
        );
        assert_eq!(evaluation.disposition, ScopeDisposition::Broadened);
        assert_eq!(evaluation.findings.len(), 2);
        assert!(evaluation
            .findings
            .iter()
            .any(|f| f.decision == ScopeDecision::AmbiguousHumanReview));
    }

    #[test]
    fn evaluation_is_independent_of_input_order() {
        let policy = snapshot(|input| input.allow_prd_expected_file_expansion = true);
        let mut files = vec![
            file("src/lib.rs", GitChangeKind::Modified, None),
            file("secrets/token", GitChangeKind::Added, None),
            file("crates/extra/mod.rs", GitChangeKind::Added, None),
            file("random/other.rs", GitChangeKind::Deleted, None),
            file("src/new.rs", GitChangeKind::Renamed, Some("src/old.rs")),
        ];
        let baseline =
            serde_json::to_vec(&evaluate_scope(&policy, &files, &ScopeEvidence::default()))
                .unwrap();
        for _ in 0..files.len() {
            files.rotate_left(1);
            let rotated =
                serde_json::to_vec(&evaluate_scope(&policy, &files, &ScopeEvidence::default()))
                    .unwrap();
            assert_eq!(baseline, rotated);
        }
        files.reverse();
        let reversed =
            serde_json::to_vec(&evaluate_scope(&policy, &files, &ScopeEvidence::default()))
                .unwrap();
        assert_eq!(baseline, reversed);
    }

    #[test]
    fn configured_classification_overrides_builtin_and_conflicts_are_ambiguous() {
        let policy = snapshot(|input| {
            input.prohibited_rules = vec![];
            input.allowed_paths = vec!["vendored/".into()];
            input.classification_rules = vec![ScopeClassificationRule {
                rule_id: "vendored_manifest_is_generated".into(),
                class: ScopeFileClass::GeneratedArtifact,
                entry: ScopePathEntry {
                    normalized: "vendored/Cargo.toml".into(),
                    match_kind: ExpectedMatchKind::ExactFile,
                },
                source: ScopeRuleSource::Configuration,
                precedence: None,
            }];
        });
        let findings = decide(
            &policy,
            &[file("vendored/Cargo.toml", GitChangeKind::Modified, None)],
        );
        assert_eq!(findings[0].2, "file_class:generated_artifact:human_review");
        let conflicted = decide(
            &policy,
            &[file(
                "crates/x/tests/Cargo.toml",
                GitChangeKind::Added,
                None,
            )],
        );
        assert_eq!(conflicted[0].1, ScopeDecision::AmbiguousHumanReview);
        assert_eq!(conflicted[0].2, "classification:conflict");
    }

    #[test]
    fn legacy_scope_check_result_json_still_deserializes() {
        let legacy = r#"{"added":["a.rs"],"modified":[],"deleted":[],"renamed":[["x","y"]],"disposition":"contained"}"#;
        let parsed: ScopeCheckResult = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.disposition, ScopeDisposition::Contained);
        assert!(parsed.findings.is_empty());
        assert_eq!(parsed.policy_snapshot_hash, "");
        assert_eq!(parsed.phase, "");
    }

    #[test]
    fn snapshot_hash_is_deterministic_and_input_sensitive() {
        let first = snapshot(|_| {});
        let second = snapshot(|_| {});
        assert_eq!(first.snapshot_hash, second.snapshot_hash);
        let changed = snapshot(|input| input.allow_prd_expected_file_expansion = true);
        assert_ne!(first.snapshot_hash, changed.snapshot_hash);
        assert_eq!(first.schema_version, SCOPE_POLICY_SCHEMA_VERSION);
        assert_eq!(first.builtin_rules_version, BUILTIN_SCOPE_RULES_VERSION);
    }

    #[test]
    fn compile_rejects_invalid_duplicate_shadowed_and_authorityless_inputs() {
        let mut input = base_input();
        input.allowed_paths = vec!["src/../x".into()];
        assert!(matches!(
            compile_scope_policy(input),
            Err(ScopePolicyError::InvalidAllowedPath { .. })
        ));
        let mut input = base_input();
        input.allowed_paths = vec!["src/**".into(), "src/".into()];
        assert!(matches!(
            compile_scope_policy(input),
            Err(ScopePolicyError::DuplicateAllowedPath { .. })
        ));
        let mut input = base_input();
        input.allowed_paths = vec!["src/".into(), "src/inner.rs".into()];
        assert!(matches!(
            compile_scope_policy(input),
            Err(ScopePolicyError::ShadowedAllowedPath { .. })
        ));
        let mut input = base_input();
        input.allowed_paths = vec![];
        assert!(matches!(
            compile_scope_policy(input),
            Err(ScopePolicyError::NoAuthoritySource)
        ));
        let mut input = base_input();
        input.allowed_paths = vec![];
        input.allow_prd_expected_file_expansion = true;
        assert!(compile_scope_policy(input).is_ok());
        let mut input = base_input();
        input.classification_rules = vec![
            ScopeClassificationRule {
                rule_id: "dup".into(),
                class: ScopeFileClass::Migration,
                entry: ScopePathEntry {
                    normalized: "migrations/".into(),
                    match_kind: ExpectedMatchKind::Directory,
                },
                source: ScopeRuleSource::Configuration,
                precedence: None,
            },
            ScopeClassificationRule {
                rule_id: "dup".into(),
                class: ScopeFileClass::Configuration,
                entry: ScopePathEntry {
                    normalized: "config/".into(),
                    match_kind: ExpectedMatchKind::Directory,
                },
                source: ScopeRuleSource::Configuration,
                precedence: None,
            },
        ];
        assert!(matches!(
            compile_scope_policy(input),
            Err(ScopePolicyError::DuplicateRuleId { .. })
        ));
        let mut input = base_input();
        input.classification_rules = vec![
            ScopeClassificationRule {
                rule_id: "a".into(),
                class: ScopeFileClass::Migration,
                entry: ScopePathEntry {
                    normalized: "shared/".into(),
                    match_kind: ExpectedMatchKind::Directory,
                },
                source: ScopeRuleSource::Configuration,
                precedence: None,
            },
            ScopeClassificationRule {
                rule_id: "b".into(),
                class: ScopeFileClass::Configuration,
                entry: ScopePathEntry {
                    normalized: "shared/".into(),
                    match_kind: ExpectedMatchKind::Directory,
                },
                source: ScopeRuleSource::Configuration,
                precedence: None,
            },
        ];
        assert!(matches!(
            compile_scope_policy(input),
            Err(ScopePolicyError::ConflictingClassificationRules { .. })
        ));
    }
}
