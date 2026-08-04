use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::expected_files::{ExpectedFileEntry, ExpectedMatchKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAssignment {
    pub adapter_id: String,
    pub agent_id: String,
    pub provider: Option<String>,
    pub requested_model: Option<String>,
    pub role: AgentRole,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Implementation,
    Review,
    Remediation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentObservation {
    pub assignment: AgentAssignment,
    pub agent_version: Option<String>,
    pub reported_model: Option<String>,
    pub unavailable_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub estimated_cost_microusd: Option<u64>,
    pub pricing_provenance: Option<String>,
    pub unavailable_fields: BTreeMap<String, String>,
}

impl ExecutionUsage {
    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        fn add(a: Option<u64>, b: Option<u64>) -> Result<Option<u64>, ()> {
            match (a, b) {
                (Some(left), Some(right)) => left.checked_add(right).map(Some).ok_or(()),
                _ => Ok(None),
            }
        }
        Some(Self {
            input_tokens: add(self.input_tokens, other.input_tokens).ok()?,
            output_tokens: add(self.output_tokens, other.output_tokens).ok()?,
            cached_tokens: add(self.cached_tokens, other.cached_tokens).ok()?,
            total_tokens: add(self.total_tokens, other.total_tokens).ok()?,
            estimated_cost_microusd: add(
                self.estimated_cost_microusd,
                other.estimated_cost_microusd,
            )
            .ok()?,
            pricing_provenance: if self.pricing_provenance == other.pricing_provenance {
                self.pricing_provenance.clone()
            } else {
                None
            },
            unavailable_fields: self
                .unavailable_fields
                .clone()
                .into_iter()
                .chain(other.unavailable_fields.clone())
                .collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewTask {
    pub task_id: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub base_revision: String,
    pub allowed_paths: Vec<String>,
    pub prohibited_changes: Vec<String>,
    pub verification_plan_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub kind: GitChangeKind,
    pub old_path: Option<String>,
    pub line_summary: Vec<LineRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub content_hash: String,
    pub media_type: String,
    pub byte_size: u64,
    pub repository: String,
    pub revision: String,
    pub storage_ref: String,
    pub truncated: bool,
    pub omitted_bytes: u64,
}

pub type ArtifactRef = EvidenceRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedDocument {
    pub source: String,
    pub content: String,
    pub content_hash: String,
    pub selection_reason: String,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundedInvariant {
    pub source: String,
    pub section: String,
    pub content: String,
    pub content_hash: String,
    pub selection_reason: String,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationEvidence {
    pub check_id: String,
    pub argv: Vec<String>,
    pub working_directory: String,
    pub environment_identity: BTreeMap<String, String>,
    pub tool_identity: Option<String>,
    pub tested_identity: String,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub status: VerificationStatus,
    pub required: bool,
    pub summary: String,
    pub stdout: Option<EvidenceRef>,
    pub stderr: Option<EvidenceRef>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    TimedOut,
    Signaled,
    Unavailable,
    Inconclusive,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingReference {
    pub finding_id: String,
    pub status: FindingStatus,
    pub claim: String,
    pub category: FindingCategory,
    pub evidence: Vec<FindingEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewPackageBudget {
    pub max_bytes: u64,
    pub max_estimated_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewPackageManifest {
    pub manifest_hash: String,
    pub diff_hash: String,
    pub included_sources: Vec<String>,
    pub omissions: Vec<PackageOmission>,
    pub total_bytes: u64,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageOmission {
    pub source: String,
    pub content_hash: String,
    pub byte_size: u64,
    pub reason: String,
    pub retained_ref: Option<EvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub review_id: String,
    pub task: ReviewTask,
    pub implementation: AgentAssignment,
    pub reviewer: AgentAssignment,
    pub base_revision: String,
    pub candidate_revision: Option<String>,
    pub changed_files: Vec<ChangedFile>,
    pub diff: EvidenceRef,
    /// Exact bounded, redacted diff text disclosed to the reviewer.
    pub disclosed_diff: String,
    pub contracts: Vec<BoundedDocument>,
    pub invariants: Vec<BoundedInvariant>,
    pub verification: Vec<VerificationEvidence>,
    pub prior_findings: Vec<FindingReference>,
    pub budget: ReviewPackageBudget,
    pub manifest: ReviewPackageManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub finding_id: String,
    pub category: FindingCategory,
    pub severity: FindingSeverity,
    pub blocking: bool,
    pub title: String,
    pub claim: String,
    pub evidence: Vec<FindingEvidence>,
    pub remediation: String,
    pub status: FindingStatus,
    pub supersedes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCategory {
    CorrectnessDefect,
    InvariantViolation,
    ArchitecturalDrift,
    SecurityIssue,
    TestGap,
    MaintainabilityIssue,
    ScopeViolation,
    UnverifiableClaim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Critical,
    High,
    Medium,
    Low,
    Informational,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingEvidence {
    FileRange {
        path: String,
        range: LineRange,
    },
    DiffHunk {
        path: String,
        hunk: String,
    },
    Verification {
        check_id: String,
        output: EvidenceRef,
    },
    Invariant {
        source: String,
        section: String,
        source_hash: String,
    },
    Contract {
        source: String,
        section: String,
        source_hash: String,
    },
    Artifact {
        artifact: EvidenceRef,
        field: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Open,
    Resolved,
    Superseded,
    AcceptedRisk,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewResult {
    pub review_id: String,
    pub reviewer: AgentObservation,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: u64,
    pub findings: Vec<ReviewFinding>,
    pub reviewed_manifest_hash: String,
    pub usage: ExecutionUsage,
    pub disposition: ReviewDisposition,
    pub unavailable_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationRequest {
    pub remediation_id: String,
    pub cycle_id: String,
    pub task: ReviewTask,
    pub implementation: AgentAssignment,
    pub base_revision: String,
    pub allowed_paths: Vec<String>,
    pub prohibited_paths: Vec<String>,
    pub blocking_findings: Vec<ReviewFinding>,
    pub relevant_diff: EvidenceRef,
    pub relevant_contracts: Vec<BoundedDocument>,
    pub relevant_invariants: Vec<BoundedInvariant>,
    pub verification_failures: Vec<VerificationEvidence>,
    pub acceptance_checks: Vec<RemediationCheck>,
    pub budget: RemediationBudget,
    #[serde(default)]
    pub scope_rules: Option<ScopeRuleSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationCheck {
    pub check_id: String,
    pub description: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationBudget {
    pub max_tokens: Option<u64>,
    pub max_cost_microusd: Option<u64>,
    pub max_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionObservation {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingResolution {
    pub finding_id: String,
    pub claimed_outcome: String,
    pub evidence: Vec<FindingEvidence>,
    pub reviewer_status: Option<FindingStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemediationResult {
    pub remediation_id: String,
    pub implementation: AgentObservation,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: u64,
    pub execution: ExecutionObservation,
    pub addressed_findings: Vec<FindingResolution>,
    pub changed_files: Vec<ChangedFile>,
    pub resulting_diff: EvidenceRef,
    pub scope_check: ScopeCheckResult,
    pub usage: ExecutionUsage,
    pub unavailable_fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeCheckResult {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub renamed: Vec<(String, String)>,
    pub disposition: ScopeDisposition,
    #[serde(default)]
    pub findings: Vec<ScopeFinding>,
    #[serde(default)]
    pub policy_snapshot_hash: String,
    #[serde(default)]
    pub phase: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDisposition {
    Contained,
    Broadened,
    HumanReviewRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeFileClass {
    OrdinarySource,
    DependencyManifest,
    DependencyLockfile,
    Migration,
    Configuration,
    Test,
    GeneratedArtifact,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDecision {
    AllowedChange,
    JustifiedExpectedFileChange,
    ProhibitedChange,
    UndeclaredScopeExpansion,
    AmbiguousHumanReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeRuleSource {
    BuiltIn,
    Configuration,
    ExpectedFiles,
    EvidenceValidation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFileMatch {
    pub normalized: String,
    pub source_line: u64,
    pub match_kind: ExpectedMatchKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeFinding {
    pub finding_id: String,
    pub change_id: String,
    pub path: String,
    pub old_path: Option<String>,
    pub change_kind: GitChangeKind,
    pub file_class: ScopeFileClass,
    pub decision: ScopeDecision,
    pub rule_id: String,
    pub rule_source: ScopeRuleSource,
    pub rule_detail: String,
    pub expected_file_match: Option<ExpectedFileMatch>,
    pub allowed_path_match: Option<String>,
    pub prohibited_rule_match: Option<String>,
    pub policy_snapshot_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDeclarationMode {
    ExpectedOrConfigured,
    ExpectedRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeClassPolicy {
    Deny,
    HumanReview,
    AllowWhenExpected,
    AllowWhenConfigured,
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeFileClassPolicies {
    pub dependency_manifest: ScopeClassPolicy,
    pub dependency_lockfile: ScopeClassPolicy,
    pub migration: ScopeClassPolicy,
    pub configuration: ScopeClassPolicy,
    pub test: ScopeClassPolicy,
    pub generated_artifact: ScopeClassPolicy,
}

impl Default for ScopeFileClassPolicies {
    fn default() -> Self {
        Self {
            dependency_manifest: ScopeClassPolicy::HumanReview,
            dependency_lockfile: ScopeClassPolicy::HumanReview,
            migration: ScopeClassPolicy::HumanReview,
            configuration: ScopeClassPolicy::HumanReview,
            test: ScopeClassPolicy::AllowWhenExpected,
            generated_artifact: ScopeClassPolicy::HumanReview,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePathEntry {
    pub normalized: String,
    pub match_kind: ExpectedMatchKind,
}

impl ScopePathEntry {
    pub fn matches(&self, path: &str) -> bool {
        match self.match_kind {
            ExpectedMatchKind::ExactFile => path == self.normalized,
            ExpectedMatchKind::Directory => {
                path.starts_with(&self.normalized) && path.len() > self.normalized.len()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProhibitedRuleKind {
    Path { entry: ScopePathEntry },
    FileClass { class: ScopeFileClass },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProhibitedRule {
    pub rule_id: String,
    pub rule: ProhibitedRuleKind,
    /// Empty means the rule applies to every supported change kind.
    pub change_kinds: Vec<GitChangeKind>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeClassificationRule {
    pub rule_id: String,
    pub class: ScopeFileClass,
    pub entry: ScopePathEntry,
    pub source: ScopeRuleSource,
    pub precedence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopePolicySnapshot {
    pub schema_version: String,
    pub prd_path: String,
    pub prd_content_hash: String,
    pub contract: Vec<ExpectedFileEntry>,
    pub allowed_paths: Vec<ScopePathEntry>,
    pub allow_prd_expected_file_expansion: bool,
    pub declaration_mode: ScopeDeclarationMode,
    pub prohibited_rules: Vec<ProhibitedRule>,
    pub file_class_policies: ScopeFileClassPolicies,
    pub classification_rules: Vec<ScopeClassificationRule>,
    pub builtin_rules_version: String,
    pub baseline_revision: String,
    pub config_provenance: String,
    pub snapshot_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeEvaluation {
    pub findings: Vec<ScopeFinding>,
    pub disposition: ScopeDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeRuleSummary {
    pub policy_snapshot_hash: String,
    pub authorized_paths: Vec<ScopePathEntry>,
    pub expected_files: Vec<ExpectedFileEntry>,
    pub prohibited_rules: Vec<ProhibitedRule>,
    pub file_class_policies: ScopeFileClassPolicies,
    pub blocking_scope_findings: Vec<ScopeFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerIndependence {
    pub kind: IndependenceKind,
    pub evidence: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterIsolationEvidence {
    pub adapter_id: String,
    pub fresh_process_per_execution: bool,
    pub detail: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndependenceKind {
    IndependentProviderOrModel,
    IsolatedSameModelFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCycle {
    pub cycle_id: String,
    pub task_id: String,
    pub attempt: u32,
    pub state: ReviewCycleState,
    pub implementation: AgentObservation,
    pub implementation_execution: Option<StageExecution>,
    pub reviewer: Option<AgentObservation>,
    pub independence: Option<ReviewerIndependence>,
    pub review_request: Option<ArtifactRef>,
    pub review_result: Option<ReviewResult>,
    pub remediation_request: Option<ArtifactRef>,
    pub remediation_result: Option<RemediationResult>,
    pub verification_before_review: Vec<VerificationEvidence>,
    pub verification_after_remediation: Vec<VerificationEvidence>,
    pub verification_history: Vec<VerificationEvidence>,
    #[serde(default)]
    pub scope_policy_snapshot: Option<ArtifactRef>,
    #[serde(default)]
    pub scope_evaluations: Vec<ScopeCheckResult>,
    pub aggregate_usage: ExecutionUsage,
    pub aggregate_duration_ms: u64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub disposition: ReviewDisposition,
    pub stop_reasons: Vec<ReviewStopReason>,
    pub review_attempts: Vec<StageExecution>,
    pub remediation_attempts: Vec<StageExecution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageExecution {
    pub stage_id: String,
    pub kind: StageKind,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: u64,
    pub outcome: String,
    pub usage: ExecutionUsage,
    pub unavailable_fields: BTreeMap<String, String>,
    pub request_artifact: Option<ArtifactRef>,
    pub response_artifact: Option<ArtifactRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Implementation,
    Verification,
    Review,
    Remediation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewCycleState {
    CollectingEvidence,
    Verifying,
    AwaitingReview,
    Reviewed,
    Remediating,
    Reverifying,
    Completed,
    HumanReviewRequired,
    Interrupted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDisposition {
    Pending,
    RemediationRequired,
    ReadyForHumanApproval,
    HumanReviewRequired,
    Rejected,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStopReason {
    RetryLimitExhausted,
    TokenLimitExhausted,
    CostLimitExhausted,
    DurationLimitExhausted,
    ConflictingFindings,
    ScopeBroadened,
    ScopeAmbiguous,
    ArchitecturalApprovalRequired,
    VerificationUnsuccessful,
    NoIndependentReviewer,
    MalformedReview,
    AgentFailure,
    EvidenceFailure,
    Interrupted,
    CleanReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonClassification {
    ProjectInvariant,
    ProjectSpecificHeuristic,
    GeneralEngineeringHeuristic,
    OneOffFinding,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonStatus {
    Proposed,
    Approved,
    Rejected,
    Superseded,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonApplicability {
    pub project_id: String,
    pub paths: Vec<String>,
    pub categories: Vec<FindingCategory>,
    pub exclusions: Vec<String>,
    pub max_future_tokens: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanApproval {
    pub human_id: String,
    pub approved_at: String,
    pub source_revision: String,
    pub exact_statement: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonProvenance {
    pub finding_id: String,
    pub review_cycle_id: String,
    pub remediation_id: String,
    pub resolution_evidence: Vec<EvidenceRef>,
    pub source_revision: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonProposal {
    pub lesson_id: String,
    pub project_id: String,
    pub classification: LessonClassification,
    pub statement: String,
    pub rationale: String,
    pub applicability: LessonApplicability,
    pub provenance: LessonProvenance,
    pub status: LessonStatus,
    pub proposed_at: String,
    pub reviewed_by: Option<HumanApproval>,
}
