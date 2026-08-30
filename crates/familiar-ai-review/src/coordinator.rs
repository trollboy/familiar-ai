use std::io::Write;
use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use thiserror::Error;

use crate::*;

pub trait ReviewAgent {
    fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
        None
    }
    fn review(
        &self,
        request: &ReviewRequest,
        output: &mut dyn Write,
    ) -> Result<ReviewResult, ReviewExecutionError>;
}
pub trait RemediationAgent {
    fn remediate(
        &self,
        request: &RemediationRequest,
        output: &mut dyn Write,
    ) -> Result<RemediationResult, RemediationExecutionError>;
}
pub trait ReviewStore {
    fn save_cycle(&self, cycle: &ReviewCycle) -> Result<(), String>;
    fn save_artifact(&self, kind: &str, value: &[u8]) -> Result<ArtifactRef, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLimits {
    pub max_review_attempts: u32,
    pub max_remediation_attempts: u32,
    pub max_total_tokens: Option<u64>,
    pub max_total_cost_microusd: Option<u64>,
    pub max_total_duration_ms: Option<u64>,
    pub review_reservation_tokens: Option<u64>,
    pub remediation_reservation_tokens: Option<u64>,
    pub action_reservation_cost_microusd: Option<u64>,
    pub action_reservation_duration_ms: u64,
}
impl WorkflowLimits {
    pub fn validate(&self) -> Result<(), CoordinatorError> {
        if self.max_review_attempts == 0 || self.max_remediation_attempts == 0 {
            return Err(CoordinatorError::InvalidLimits);
        }
        if self.max_total_tokens.is_none()
            && self.max_total_cost_microusd.is_none()
            && self.max_total_duration_ms.is_none()
        {
            return Err(CoordinatorError::InvalidLimits);
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct CoordinationRequest {
    pub cycle_id: String,
    pub task: ReviewTask,
    pub implementation: AgentObservation,
    pub reviewer: AgentAssignment,
    pub standard_reviewer: Option<AgentAssignment>,
    pub tier_policy: ReviewTierPolicy,
    pub declared_risk_classes: Vec<String>,
    pub contracts: Vec<BoundedDocument>,
    pub invariants: Vec<BoundedInvariant>,
    pub verification_plan: VerificationPlan,
    pub package_budget: ReviewPackageBudget,
    pub limits: WorkflowLimits,
    pub allow_same_model_fallback: bool,
    pub implementation_usage: ExecutionUsage,
    pub implementation_duration_ms: u64,
    pub scope_policy: ScopePolicySnapshot,
}

pub struct ReviewCoordinator<'a> {
    pub collector: &'a dyn EvidenceCollector,
    pub verifier: &'a dyn VerificationRunner,
    pub reviewer: &'a dyn ReviewAgent,
    pub implementer: &'a dyn RemediationAgent,
    pub store: &'a dyn ReviewStore,
    pub policy: BlockingPolicy,
}

impl ReviewCoordinator<'_> {
    pub fn run(
        &self,
        repository: &Path,
        request: CoordinationRequest,
        output: &mut dyn Write,
    ) -> Result<ReviewCycle, CoordinatorError> {
        request.limits.validate()?;
        let started = Utc::now().to_rfc3339();
        let initial_usage = request.implementation_usage.clone();
        let mut cycle = ReviewCycle {
            cycle_id: request.cycle_id.clone(),
            task_id: request.task.task_id.clone(),
            attempt: 0,
            state: ReviewCycleState::CollectingEvidence,
            implementation: request.implementation.clone(),
            implementation_execution: Some(StageExecution {
                stage_id: format!("{}-implementation", request.cycle_id),
                kind: StageKind::Implementation,
                started_at: started.clone(),
                ended_at: started.clone(),
                duration_ms: request.implementation_duration_ms,
                outcome: "completed".into(),
                usage: request.implementation_usage.clone(),
                unavailable_fields: request.implementation_usage.unavailable_fields.clone(),
                request_artifact: None,
                response_artifact: None,
            }),
            reviewer: None,
            independence: None,
            review_request: None,
            review_result: None,
            remediation_request: None,
            remediation_result: None,
            verification_before_review: vec![],
            verification_after_remediation: vec![],
            verification_history: vec![],
            scope_policy_snapshot: None,
            scope_evaluations: vec![],
            tier_selection: None,
            aggregate_usage: initial_usage,
            aggregate_duration_ms: request.implementation_duration_ms,
            started_at: started,
            ended_at: None,
            disposition: ReviewDisposition::Pending,
            stop_reasons: vec![],
            review_attempts: vec![],
            remediation_attempts: vec![],
        };
        let snapshot_bytes = serde_json::to_vec(&request.scope_policy)
            .map_err(|e| CoordinatorError::Persistence(e.to_string()))?;
        cycle.scope_policy_snapshot = Some(
            self.store
                .save_artifact("scope_policy_snapshot", &snapshot_bytes)
                .map_err(CoordinatorError::Persistence)?,
        );
        self.store
            .save_cycle(&cycle)
            .map_err(CoordinatorError::Persistence)?;
        let mut captured = match self
            .collector
            .capture(repository, &request.task.base_revision)
        {
            Ok(value) => value,
            Err(error) => {
                eprintln!("review: diff capture failed: {error}");
                return self.stop(cycle, ReviewStopReason::EvidenceFailure);
            }
        };
        let scope_evidence = match collect_scope_evidence(repository, &captured.changed_files) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("review: scope evidence collection failed: {error}");
                return self.stop(cycle, ReviewStopReason::EvidenceFailure);
            }
        };
        let evaluation = evaluate_scope(
            &request.scope_policy,
            &captured.changed_files,
            &scope_evidence,
        );
        cycle.scope_evaluations.push(scope_check_result(
            &captured.changed_files,
            &evaluation,
            &request.scope_policy,
            "initial",
        ));
        cycle.tier_selection = Some(select_review_tier(
            &request.tier_policy,
            &request.declared_risk_classes,
            &captured.changed_files,
            captured.diff.byte_size,
            cycle
                .scope_evaluations
                .last()
                .expect("scope evaluation just recorded"),
        ));
        self.store
            .save_cycle(&cycle)
            .map_err(CoordinatorError::Persistence)?;
        match evaluation.disposition {
            ScopeDisposition::Broadened => {
                return self.stop(cycle, ReviewStopReason::ScopeBroadened)
            }
            ScopeDisposition::HumanReviewRequired => {
                return self.stop(cycle, ReviewStopReason::ScopeAmbiguous)
            }
            ScopeDisposition::Contained => {}
        }
        cycle.state = ReviewCycleState::Verifying;
        self.store
            .save_cycle(&cycle)
            .map_err(CoordinatorError::Persistence)?;
        for check in &request.verification_plan.checks {
            eprintln!("review: verification '{}' running...", check.check_id);
            if let Err(error) = self.reserve(&cycle, &request.limits, Action::Verification) {
                return self.stop(cycle, limit_reason(&error));
            }
            let ev = match self
                .verifier
                .run(repository, check, &captured.diff.content_hash)
            {
                Ok(value) => value,
                Err(_) => return self.stop(cycle, ReviewStopReason::VerificationUnsuccessful),
            };
            cycle.verification_before_review.push(ev);
            cycle.verification_history.push(
                cycle
                    .verification_before_review
                    .last()
                    .expect("just pushed verification")
                    .clone(),
            );
            let verification_duration = cycle
                .verification_before_review
                .last()
                .expect("just pushed verification")
                .duration_ms;
            if !add_duration(&mut cycle, verification_duration) {
                return self.stop(cycle, ReviewStopReason::DurationLimitExhausted);
            }
            if let Some(reason) = budget_violation(&cycle, &request.limits) {
                return self.stop(cycle, reason);
            }
        }
        if required_failed(&cycle.verification_before_review) {
            return self.stop(cycle, ReviewStopReason::VerificationUnsuccessful);
        }
        if cycle
            .tier_selection
            .as_ref()
            .is_some_and(|selection| selection.tier == ReviewTier::ChecksOnly)
        {
            cycle.disposition = ReviewDisposition::ReadyForHumanApproval;
            return self.stop(cycle, ReviewStopReason::CleanReview);
        }
        let full_reviewer = request.reviewer.clone();
        let standard_reviewer = request.standard_reviewer.clone();
        let selected_reviewer = if cycle
            .tier_selection
            .as_ref()
            .is_some_and(|selection| selection.tier == ReviewTier::Standard)
        {
            standard_reviewer
                .clone()
                .unwrap_or_else(|| full_reviewer.clone())
        } else {
            full_reviewer.clone()
        };
        let isolation = self.reviewer.isolation_evidence();
        let Some(independence) = check_independence(
            &request.implementation.assignment,
            &selected_reviewer,
            request.allow_same_model_fallback,
            isolation.as_ref(),
        ) else {
            return self.stop(cycle, ReviewStopReason::NoIndependentReviewer);
        };
        cycle.independence = Some(independence);
        let mut request = request;
        request.reviewer = selected_reviewer;
        let mut prior = Vec::new();
        let mut remediation_count = 0;
        loop {
            if cycle.attempt >= request.limits.max_review_attempts {
                return self.stop(cycle, ReviewStopReason::RetryLimitExhausted);
            }
            if let Err(error) = self.reserve(&cycle, &request.limits, Action::Review) {
                return self.stop(cycle, limit_reason(&error));
            }
            cycle.attempt += 1;
            cycle.state = ReviewCycleState::AwaitingReview;
            let review_id = format!("{}-review-{}", cycle.cycle_id, cycle.attempt);
            let package = match build_review_request(ReviewPackageInput {
                review_id,
                task: request.task.clone(),
                implementation: request.implementation.assignment.clone(),
                reviewer: request.reviewer.clone(),
                candidate_revision: Some(captured.resulting_tree.clone()),
                captured: captured.clone(),
                contracts: request.contracts.clone(),
                invariants: request.invariants.clone(),
                verification: if remediation_count == 0 {
                    cycle.verification_before_review.clone()
                } else {
                    cycle.verification_after_remediation.clone()
                },
                prior_findings: prior.clone(),
                budget: request.package_budget.clone(),
            }) {
                Ok(value) => value,
                Err(_) => return self.stop(cycle, ReviewStopReason::EvidenceFailure),
            };
            let package_bytes = serde_json::to_vec(&package)
                .map_err(|e| CoordinatorError::Persistence(e.to_string()))?;
            cycle.review_request = Some(
                self.store
                    .save_artifact("review_request", &package_bytes)
                    .map_err(CoordinatorError::Persistence)?,
            );
            self.store
                .save_cycle(&cycle)
                .map_err(CoordinatorError::Persistence)?;
            let attempt_started = Utc::now().to_rfc3339();
            let attempt_timer = Instant::now();
            eprintln!(
                "review: independent reviewer analyzing (attempt {}, typically 2-3 minutes)...",
                cycle.attempt
            );
            let result = match self.reviewer.review(&package, output) {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("review: reviewer agent failed: {error}");
                    let mut stage = failed_stage(
                        format!("{}-review-{}", cycle.cycle_id, cycle.attempt),
                        StageKind::Review,
                        attempt_started,
                        attempt_timer,
                        "agent_failure",
                    );
                    stage.request_artifact = cycle.review_request.clone();
                    if !add_usage(&mut cycle, &stage.usage) {
                        return self.stop(cycle, ReviewStopReason::TokenLimitExhausted);
                    }
                    if !add_duration(&mut cycle, stage.duration_ms) {
                        return self.stop(cycle, ReviewStopReason::DurationLimitExhausted);
                    }
                    cycle.review_attempts.push(stage);
                    self.store
                        .save_cycle(&cycle)
                        .map_err(CoordinatorError::Persistence)?;
                    if cycle.attempt < request.limits.max_review_attempts {
                        continue;
                    }
                    return self.stop(cycle, ReviewStopReason::AgentFailure);
                }
            };
            // The reviewer execution succeeded and reported usage even when
            // validation rejects its result; keep the ledger known so the
            // remaining review attempts stay reachable under a finite token
            // ceiling.
            let reviewed_usage = result.usage.clone();
            let result = match self.policy.apply_and_validate(&package, result) {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("review: result rejected by policy validation: {error}");
                    let mut stage = failed_stage(
                        format!("{}-review-{}", cycle.cycle_id, cycle.attempt),
                        StageKind::Review,
                        attempt_started,
                        attempt_timer,
                        "malformed_review",
                    );
                    stage.usage = reviewed_usage;
                    stage.request_artifact = cycle.review_request.clone();
                    if !add_usage(&mut cycle, &stage.usage) {
                        return self.stop(cycle, ReviewStopReason::TokenLimitExhausted);
                    }
                    if !add_duration(&mut cycle, stage.duration_ms) {
                        return self.stop(cycle, ReviewStopReason::DurationLimitExhausted);
                    }
                    cycle.review_attempts.push(stage);
                    self.store
                        .save_cycle(&cycle)
                        .map_err(CoordinatorError::Persistence)?;
                    if cycle.attempt < request.limits.max_review_attempts {
                        continue;
                    }
                    return self.stop(cycle, ReviewStopReason::MalformedReview);
                }
            };
            let result_bytes = serde_json::to_vec(&result)
                .map_err(|error| CoordinatorError::Persistence(error.to_string()))?;
            let result_artifact = self
                .store
                .save_artifact("review_result", &result_bytes)
                .map_err(CoordinatorError::Persistence)?;
            cycle.review_attempts.push(StageExecution {
                stage_id: format!("{}-review-{}", cycle.cycle_id, cycle.attempt),
                kind: StageKind::Review,
                started_at: result.started_at.clone(),
                ended_at: result.ended_at.clone(),
                duration_ms: result.duration_ms,
                outcome: "completed".into(),
                usage: result.usage.clone(),
                unavailable_fields: result.unavailable_fields.clone(),
                request_artifact: cycle.review_request.clone(),
                response_artifact: Some(result_artifact),
            });
            if !add_usage(&mut cycle, &result.usage) {
                return self.stop(cycle, ReviewStopReason::TokenLimitExhausted);
            }
            if !add_duration(&mut cycle, result.duration_ms) {
                return self.stop(cycle, ReviewStopReason::DurationLimitExhausted);
            }
            if let Some(reason) = budget_violation(&cycle, &request.limits) {
                return self.stop(cycle, reason);
            }
            cycle.reviewer = Some(result.reviewer.clone());
            cycle.state = ReviewCycleState::Reviewed;
            cycle.review_result = Some(result.clone());
            self.store
                .save_cycle(&cycle)
                .map_err(CoordinatorError::Persistence)?;
            if findings_conflict(&result.findings) {
                return self.stop(cycle, ReviewStopReason::ConflictingFindings);
            }
            let blocking: Vec<_> = deduplicate_findings(
                result
                    .findings
                    .iter()
                    .filter(|f| f.blocking && f.status == FindingStatus::Open)
                    .cloned()
                    .collect(),
            );
            if blocking.is_empty() {
                cycle.disposition = ReviewDisposition::ReadyForHumanApproval;
                return self.stop(cycle, ReviewStopReason::CleanReview);
            }
            if blocking.iter().any(|f| {
                f.category == FindingCategory::ArchitecturalDrift
                    || f.category == FindingCategory::InvariantViolation
            }) {
                return self.stop(cycle, ReviewStopReason::ArchitecturalApprovalRequired);
            }
            let remediation_result = loop {
                if remediation_count >= request.limits.max_remediation_attempts {
                    return self.stop(cycle, ReviewStopReason::RetryLimitExhausted);
                }
                if let Err(error) = self.reserve(&cycle, &request.limits, Action::Remediation) {
                    return self.stop(cycle, limit_reason(&error));
                }
                remediation_count += 1;
                cycle.state = ReviewCycleState::Remediating;
                let remediation = RemediationRequest {
                    remediation_id: format!("{}-remediation-{remediation_count}", cycle.cycle_id),
                    cycle_id: cycle.cycle_id.clone(),
                    task: request.task.clone(),
                    implementation: request.implementation.assignment.clone(),
                    base_revision: request.task.base_revision.clone(),
                    allowed_paths: request
                        .scope_policy
                        .allowed_paths
                        .iter()
                        .map(|entry| entry.normalized.clone())
                        .collect(),
                    prohibited_paths: request
                        .scope_policy
                        .prohibited_rules
                        .iter()
                        .map(|rule| rule.rule_id.clone())
                        .collect(),
                    blocking_findings: blocking.clone(),
                    relevant_diff: captured.diff.clone(),
                    relevant_contracts: request.contracts.clone(),
                    relevant_invariants: request.invariants.clone(),
                    verification_failures: vec![],
                    acceptance_checks: request
                        .verification_plan
                        .checks
                        .iter()
                        .map(|c| RemediationCheck {
                            check_id: c.check_id.clone(),
                            description: format!("run exact configured command: {:?}", c.argv),
                        })
                        .collect(),
                    budget: RemediationBudget {
                        max_tokens: request.limits.remediation_reservation_tokens,
                        max_cost_microusd: request.limits.action_reservation_cost_microusd,
                        max_duration_ms: request.limits.action_reservation_duration_ms,
                    },
                    scope_rules: Some(scope_rule_summary(
                        &request.scope_policy,
                        cycle
                            .scope_evaluations
                            .last()
                            .map(|evaluation| {
                                evaluation
                                    .findings
                                    .iter()
                                    .filter(|finding| {
                                        !matches!(
                                            finding.decision,
                                            ScopeDecision::AllowedChange
                                                | ScopeDecision::JustifiedExpectedFileChange
                                        )
                                    })
                                    .cloned()
                                    .collect()
                            })
                            .unwrap_or_default(),
                    )),
                };
                let remediation_bytes = serde_json::to_vec(&remediation)
                    .map_err(|e| CoordinatorError::Persistence(e.to_string()))?;
                cycle.remediation_request = Some(
                    self.store
                        .save_artifact("remediation_request", &remediation_bytes)
                        .map_err(CoordinatorError::Persistence)?,
                );
                self.store
                    .save_cycle(&cycle)
                    .map_err(CoordinatorError::Persistence)?;
                let started = Utc::now().to_rfc3339();
                let timer = Instant::now();
                match self.implementer.remediate(&remediation, output) {
                    Ok(result) => {
                        let result_bytes = serde_json::to_vec(&result)
                            .map_err(|error| RemediationExecutionError::Agent(error.to_string()))?;
                        let result_artifact = self
                            .store
                            .save_artifact("remediation_result", &result_bytes)
                            .map_err(RemediationExecutionError::Agent)?;
                        cycle.remediation_attempts.push(StageExecution {
                            stage_id: remediation.remediation_id,
                            kind: StageKind::Remediation,
                            started_at: result.started_at.clone(),
                            ended_at: result.ended_at.clone(),
                            duration_ms: result.duration_ms,
                            outcome: result.execution.outcome.clone(),
                            usage: result.usage.clone(),
                            unavailable_fields: result.unavailable_fields.clone(),
                            request_artifact: cycle.remediation_request.clone(),
                            response_artifact: Some(result_artifact),
                        });
                        break result;
                    }
                    Err(error) => {
                        eprintln!("review: remediation agent failed: {error}");
                        let mut stage = failed_stage(
                            remediation.remediation_id,
                            StageKind::Remediation,
                            started,
                            timer,
                            "agent_failure",
                        );
                        stage.request_artifact = cycle.remediation_request.clone();
                        if !add_usage(&mut cycle, &stage.usage) {
                            return self.stop(cycle, ReviewStopReason::TokenLimitExhausted);
                        }
                        if !add_duration(&mut cycle, stage.duration_ms) {
                            return self.stop(cycle, ReviewStopReason::DurationLimitExhausted);
                        }
                        cycle.remediation_attempts.push(stage);
                        self.store
                            .save_cycle(&cycle)
                            .map_err(CoordinatorError::Persistence)?;
                        if remediation_count >= request.limits.max_remediation_attempts {
                            return self.stop(cycle, ReviewStopReason::AgentFailure);
                        }
                    }
                }
            };
            if !add_usage(&mut cycle, &remediation_result.usage) {
                return self.stop(cycle, ReviewStopReason::TokenLimitExhausted);
            }
            if !add_duration(&mut cycle, remediation_result.duration_ms) {
                return self.stop(cycle, ReviewStopReason::DurationLimitExhausted);
            }
            if let Some(reason) = budget_violation(&cycle, &request.limits) {
                return self.stop(cycle, reason);
            }
            captured = match self
                .collector
                .capture(repository, &request.task.base_revision)
            {
                Ok(value) => value,
                Err(_) => return self.stop(cycle, ReviewStopReason::EvidenceFailure),
            };
            let scope_evidence = match collect_scope_evidence(repository, &captured.changed_files) {
                Ok(value) => value,
                Err(_) => return self.stop(cycle, ReviewStopReason::EvidenceFailure),
            };
            let evaluation = evaluate_scope(
                &request.scope_policy,
                &captured.changed_files,
                &scope_evidence,
            );
            let scope = scope_check_result(
                &captured.changed_files,
                &evaluation,
                &request.scope_policy,
                &format!("remediation-{remediation_count}"),
            );
            cycle.scope_evaluations.push(scope.clone());
            let selection = select_review_tier(
                &request.tier_policy,
                &request.declared_risk_classes,
                &captured.changed_files,
                captured.diff.byte_size,
                &scope,
            );
            request.reviewer = if selection.tier == ReviewTier::Standard {
                standard_reviewer
                    .clone()
                    .unwrap_or_else(|| full_reviewer.clone())
            } else {
                full_reviewer.clone()
            };
            cycle.tier_selection = Some(selection);
            cycle.independence = check_independence(
                &request.implementation.assignment,
                &request.reviewer,
                request.allow_same_model_fallback,
                self.reviewer.isolation_evidence().as_ref(),
            );
            if cycle.independence.is_none() {
                return self.stop(cycle, ReviewStopReason::NoIndependentReviewer);
            }
            cycle.remediation_result = Some(RemediationResult {
                changed_files: captured.changed_files.clone(),
                resulting_diff: captured.diff.clone(),
                scope_check: scope,
                ..remediation_result
            });
            self.store
                .save_cycle(&cycle)
                .map_err(CoordinatorError::Persistence)?;
            match evaluation.disposition {
                ScopeDisposition::Broadened => {
                    return self.stop(cycle, ReviewStopReason::ScopeBroadened)
                }
                ScopeDisposition::HumanReviewRequired => {
                    return self.stop(cycle, ReviewStopReason::ScopeAmbiguous)
                }
                ScopeDisposition::Contained => {}
            }
            cycle.state = ReviewCycleState::Reverifying;
            cycle.verification_after_remediation.clear();
            let cited_checks: Vec<String> = blocking
                .iter()
                .flat_map(|finding| finding.evidence.iter())
                .filter_map(|evidence| match evidence {
                    FindingEvidence::Verification { check_id, .. } => Some(check_id.clone()),
                    _ => None,
                })
                .collect();
            let unsuccessful_checks: Vec<String> = cycle
                .verification_before_review
                .iter()
                .filter(|evidence| evidence.status != VerificationStatus::Passed)
                .map(|evidence| evidence.check_id.clone())
                .collect();
            for check in relevant_checks(
                &request.verification_plan,
                &captured
                    .changed_files
                    .iter()
                    .map(|f| f.path.clone())
                    .collect::<Vec<_>>(),
                &cited_checks,
                &unsuccessful_checks,
            ) {
                if let Err(error) = self.reserve(&cycle, &request.limits, Action::Verification) {
                    return self.stop(cycle, limit_reason(&error));
                }
                let ev = match self
                    .verifier
                    .run(repository, check, &captured.diff.content_hash)
                {
                    Ok(value) => value,
                    Err(_) => return self.stop(cycle, ReviewStopReason::VerificationUnsuccessful),
                };
                cycle.verification_after_remediation.push(ev);
                cycle.verification_history.push(
                    cycle
                        .verification_after_remediation
                        .last()
                        .expect("just pushed verification")
                        .clone(),
                );
                let verification_duration = cycle
                    .verification_after_remediation
                    .last()
                    .expect("just pushed verification")
                    .duration_ms;
                if !add_duration(&mut cycle, verification_duration) {
                    return self.stop(cycle, ReviewStopReason::DurationLimitExhausted);
                }
                if let Some(reason) = budget_violation(&cycle, &request.limits) {
                    return self.stop(cycle, reason);
                }
            }
            if required_failed(&cycle.verification_after_remediation) {
                return self.stop(cycle, ReviewStopReason::VerificationUnsuccessful);
            }
            prior = blocking
                .into_iter()
                .map(|f| FindingReference {
                    finding_id: f.finding_id,
                    status: f.status,
                    claim: f.claim,
                    category: f.category,
                    evidence: f.evidence,
                })
                .collect();
        }
    }
    fn reserve(
        &self,
        cycle: &ReviewCycle,
        limits: &WorkflowLimits,
        action: Action,
    ) -> Result<(), CoordinatorError> {
        let token = match action {
            Action::Review => limits.review_reservation_tokens,
            Action::Remediation => limits.remediation_reservation_tokens,
            Action::Verification => Some(0),
        };
        if let Some(max) = limits.max_total_tokens {
            let used = cycle
                .aggregate_usage
                .total_tokens
                .ok_or(CoordinatorError::UnknownUsage)?;
            if used
                .checked_add(token.ok_or(CoordinatorError::UnknownUsage)?)
                .ok_or(CoordinatorError::AccountingOverflow)?
                > max
            {
                return Err(CoordinatorError::Limit(
                    ReviewStopReason::TokenLimitExhausted,
                ));
            }
        }
        if let Some(max) = limits.max_total_cost_microusd {
            let used = cycle
                .aggregate_usage
                .estimated_cost_microusd
                .ok_or(CoordinatorError::UnknownCost)?;
            if used
                .checked_add(
                    limits
                        .action_reservation_cost_microusd
                        .ok_or(CoordinatorError::UnknownCost)?,
                )
                .ok_or(CoordinatorError::AccountingOverflow)?
                > max
            {
                return Err(CoordinatorError::Limit(
                    ReviewStopReason::CostLimitExhausted,
                ));
            }
        }
        if let Some(max) = limits.max_total_duration_ms {
            if cycle
                .aggregate_duration_ms
                .checked_add(limits.action_reservation_duration_ms)
                .ok_or(CoordinatorError::AccountingOverflow)?
                > max
            {
                return Err(CoordinatorError::Limit(
                    ReviewStopReason::DurationLimitExhausted,
                ));
            }
        }
        Ok(())
    }
    fn stop(
        &self,
        mut cycle: ReviewCycle,
        reason: ReviewStopReason,
    ) -> Result<ReviewCycle, CoordinatorError> {
        cycle.state = if reason == ReviewStopReason::CleanReview {
            ReviewCycleState::Completed
        } else {
            ReviewCycleState::HumanReviewRequired
        };
        if reason != ReviewStopReason::CleanReview {
            cycle.disposition = ReviewDisposition::HumanReviewRequired
        }
        cycle.stop_reasons.push(reason);
        cycle.ended_at = Some(Utc::now().to_rfc3339());
        self.store
            .save_cycle(&cycle)
            .map_err(CoordinatorError::Persistence)?;
        Ok(cycle)
    }
}
#[derive(Clone, Copy)]
enum Action {
    Review,
    Remediation,
    Verification,
}
fn add_usage(cycle: &mut ReviewCycle, usage: &ExecutionUsage) -> bool {
    if let Some(total) = cycle.aggregate_usage.checked_add(usage) {
        cycle.aggregate_usage = total;
        true
    } else {
        false
    }
}
fn add_duration(cycle: &mut ReviewCycle, duration: u64) -> bool {
    if let Some(total) = cycle.aggregate_duration_ms.checked_add(duration) {
        cycle.aggregate_duration_ms = total;
        true
    } else {
        false
    }
}
fn budget_violation(cycle: &ReviewCycle, limits: &WorkflowLimits) -> Option<ReviewStopReason> {
    if let Some(max) = limits.max_total_tokens {
        match cycle.aggregate_usage.total_tokens {
            Some(value) if value <= max => {}
            _ => return Some(ReviewStopReason::TokenLimitExhausted),
        }
    }
    if let Some(max) = limits.max_total_cost_microusd {
        match cycle.aggregate_usage.estimated_cost_microusd {
            Some(value) if value <= max => {}
            _ => return Some(ReviewStopReason::CostLimitExhausted),
        }
    }
    if limits
        .max_total_duration_ms
        .is_some_and(|max| cycle.aggregate_duration_ms > max)
    {
        return Some(ReviewStopReason::DurationLimitExhausted);
    }
    None
}
fn required_failed(v: &[VerificationEvidence]) -> bool {
    v.iter()
        .any(|e| e.required && e.status != VerificationStatus::Passed)
}

fn evidence_identity(finding: &ReviewFinding) -> String {
    serde_json::to_string(&(finding.category, &finding.evidence)).unwrap_or_default()
}

fn findings_conflict(findings: &[ReviewFinding]) -> bool {
    findings.iter().enumerate().any(|(index, left)| {
        findings[index + 1..].iter().any(|right| {
            evidence_identity(left) == evidence_identity(right)
                && left.remediation != right.remediation
                && left.status == FindingStatus::Open
                && right.status == FindingStatus::Open
        })
    })
}

fn deduplicate_findings(findings: Vec<ReviewFinding>) -> Vec<ReviewFinding> {
    let mut seen = std::collections::BTreeSet::new();
    findings
        .into_iter()
        .filter(|finding| {
            seen.insert((
                finding.category,
                evidence_identity(finding),
                finding.claim.clone(),
            ))
        })
        .collect()
}

fn limit_reason(error: &CoordinatorError) -> ReviewStopReason {
    match error {
        CoordinatorError::Limit(reason) => *reason,
        CoordinatorError::UnknownUsage => ReviewStopReason::TokenLimitExhausted,
        CoordinatorError::UnknownCost => ReviewStopReason::CostLimitExhausted,
        CoordinatorError::AccountingOverflow => ReviewStopReason::DurationLimitExhausted,
        _ => ReviewStopReason::EvidenceFailure,
    }
}

fn failed_stage(
    id: String,
    kind: StageKind,
    started_at: String,
    timer: Instant,
    outcome: &str,
) -> StageExecution {
    let mut unavailable = std::collections::BTreeMap::new();
    for field in [
        "input_tokens",
        "output_tokens",
        "cached_tokens",
        "total_tokens",
        "estimated_cost_microusd",
    ] {
        unavailable.insert(field.into(), outcome.into());
    }
    StageExecution {
        stage_id: id,
        kind,
        started_at,
        ended_at: Utc::now().to_rfc3339(),
        duration_ms: u64::try_from(timer.elapsed().as_millis()).unwrap_or(u64::MAX),
        outcome: outcome.into(),
        usage: ExecutionUsage {
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            total_tokens: None,
            estimated_cost_microusd: None,
            pricing_provenance: None,
            unavailable_fields: unavailable.clone(),
        },
        unavailable_fields: unavailable,
        request_artifact: None,
        response_artifact: None,
    }
}

#[derive(Debug, Error)]
pub enum ReviewExecutionError {
    #[error("review agent failed: {0}")]
    Agent(String),
}
#[derive(Debug, Error)]
pub enum RemediationExecutionError {
    #[error("remediation agent failed: {0}")]
    Agent(String),
}
#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("invalid workflow limits")]
    InvalidLimits,
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error(transparent)]
    Verification(#[from] VerificationError),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error("review failed: {0}")]
    Review(#[from] ReviewExecutionError),
    #[error("remediation failed: {0}")]
    Remediation(#[from] RemediationExecutionError),
    #[error("malformed review: {0}")]
    MalformedReview(#[from] ReviewValidationError),
    #[error("persistence failed: {0}")]
    Persistence(String),
    #[error("accounting overflow")]
    AccountingOverflow,
    #[error("token usage is unavailable")]
    UnknownUsage,
    #[error("cost is unavailable")]
    UnknownCost,
    #[error("workflow limit reached: {0:?}")]
    Limit(ReviewStopReason),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    fn assignment(role: AgentRole, session: &str) -> AgentAssignment {
        AgentAssignment {
            adapter_id: "fake".into(),
            agent_id: "fake".into(),
            provider: Some("p".into()),
            requested_model: Some("m".into()),
            role,
            session_id: Some(session.into()),
        }
    }
    fn reference() -> EvidenceRef {
        EvidenceRef {
            content_hash: "fnv1a64:1".into(),
            media_type: "text/x-diff".into(),
            byte_size: 1,
            repository: "repo".into(),
            revision: "base".into(),
            storage_ref: "mem".into(),
            truncated: false,
            omitted_bytes: 0,
        }
    }
    struct Collector;
    impl EvidenceCollector for Collector {
        fn capture(&self, _: &Path, _: &str) -> Result<CapturedDiff, EvidenceError> {
            Ok(CapturedDiff {
                base_revision: "base".into(),
                resulting_tree: "result".into(),
                changed_files: vec![ChangedFile {
                    path: "src/lib.rs".into(),
                    kind: GitChangeKind::Modified,
                    old_path: None,
                    line_summary: vec![],
                }],
                diff: reference(),
                bytes: vec![1],
            })
        }
    }
    struct Verifier;
    impl VerificationRunner for Verifier {
        fn run(
            &self,
            _: &Path,
            c: &VerificationCheck,
            id: &str,
        ) -> Result<VerificationEvidence, VerificationError> {
            Ok(VerificationEvidence {
                check_id: c.check_id.clone(),
                argv: c.argv.clone(),
                working_directory: ".".into(),
                environment_identity: BTreeMap::new(),
                tool_identity: None,
                tested_identity: id.into(),
                started_at: "2026-01-01T00:00:00Z".into(),
                ended_at: "2026-01-01T00:00:00Z".into(),
                duration_ms: 1,
                exit_code: Some(0),
                signal: None,
                status: VerificationStatus::Passed,
                required: true,
                summary: "passed".into(),
                stdout: Some(reference()),
                stderr: Some(reference()),
                truncated: false,
            })
        }
    }
    struct Reviewer;
    impl ReviewAgent for Reviewer {
        fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
            Some(AdapterIsolationEvidence {
                adapter_id: "fake".into(),
                fresh_process_per_execution: true,
                detail: "fake isolated review".into(),
            })
        }
        fn review(
            &self,
            r: &ReviewRequest,
            _: &mut dyn Write,
        ) -> Result<ReviewResult, ReviewExecutionError> {
            Ok(ReviewResult {
                review_id: r.review_id.clone(),
                reviewer: AgentObservation {
                    assignment: r.reviewer.clone(),
                    agent_version: None,
                    reported_model: None,
                    unavailable_fields: BTreeMap::new(),
                },
                started_at: "2026-01-01T00:00:00Z".into(),
                ended_at: "2026-01-01T00:00:00Z".into(),
                duration_ms: 1,
                findings: vec![],
                reviewed_manifest_hash: r.manifest.manifest_hash.clone(),
                usage: ExecutionUsage {
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    cached_tokens: Some(0),
                    total_tokens: Some(2),
                    estimated_cost_microusd: None,
                    pricing_provenance: None,
                    unavailable_fields: BTreeMap::new(),
                },
                disposition: ReviewDisposition::Pending,
                unavailable_fields: BTreeMap::new(),
            })
        }
    }
    struct Implementer;
    impl RemediationAgent for Implementer {
        fn remediate(
            &self,
            _: &RemediationRequest,
            _: &mut dyn Write,
        ) -> Result<RemediationResult, RemediationExecutionError> {
            panic!("clean review must not remediate")
        }
    }
    struct RemediatingReviewer {
        calls: RefCell<u32>,
    }
    impl ReviewAgent for RemediatingReviewer {
        fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
            Some(AdapterIsolationEvidence {
                adapter_id: "fake".into(),
                fresh_process_per_execution: true,
                detail: "fake isolated review".into(),
            })
        }
        fn review(
            &self,
            request: &ReviewRequest,
            _: &mut dyn Write,
        ) -> Result<ReviewResult, ReviewExecutionError> {
            let mut calls = self.calls.borrow_mut();
            *calls += 1;
            let status = if *calls == 1 {
                FindingStatus::Open
            } else {
                FindingStatus::Resolved
            };
            Ok(ReviewResult {
                review_id: request.review_id.clone(),
                reviewer: AgentObservation {
                    assignment: request.reviewer.clone(),
                    agent_version: None,
                    reported_model: None,
                    unavailable_fields: BTreeMap::new(),
                },
                started_at: "2026-01-01T00:00:00Z".into(),
                ended_at: "2026-01-01T00:00:00Z".into(),
                duration_ms: 1,
                findings: vec![ReviewFinding {
                    finding_id: "finding".into(),
                    category: FindingCategory::CorrectnessDefect,
                    severity: FindingSeverity::High,
                    blocking: false,
                    title: "defect".into(),
                    claim: "wrong result".into(),
                    evidence: vec![
                        FindingEvidence::FileRange {
                            path: "src/lib.rs".into(),
                            range: LineRange { start: 1, end: 1 },
                        },
                        FindingEvidence::Verification {
                            check_id: "test".into(),
                            output: reference(),
                        },
                    ],
                    remediation: "correct the result".into(),
                    status,
                    supersedes: None,
                }],
                reviewed_manifest_hash: request.manifest.manifest_hash.clone(),
                usage: known_usage(),
                disposition: ReviewDisposition::Pending,
                unavailable_fields: BTreeMap::new(),
            })
        }
    }
    struct SuccessfulImplementer {
        calls: RefCell<u32>,
    }
    impl RemediationAgent for SuccessfulImplementer {
        fn remediate(
            &self,
            request: &RemediationRequest,
            _: &mut dyn Write,
        ) -> Result<RemediationResult, RemediationExecutionError> {
            *self.calls.borrow_mut() += 1;
            Ok(RemediationResult {
                remediation_id: request.remediation_id.clone(),
                implementation: AgentObservation {
                    assignment: request.implementation.clone(),
                    agent_version: None,
                    reported_model: None,
                    unavailable_fields: BTreeMap::new(),
                },
                started_at: "2026-01-01T00:00:00Z".into(),
                ended_at: "2026-01-01T00:00:00Z".into(),
                duration_ms: 1,
                execution: ExecutionObservation {
                    exit_code: Some(0),
                    signal: None,
                    outcome: "completed".into(),
                },
                addressed_findings: vec![],
                changed_files: vec![],
                resulting_diff: reference(),
                scope_check: ScopeCheckResult {
                    added: vec![],
                    modified: vec![],
                    deleted: vec![],
                    renamed: vec![],
                    disposition: ScopeDisposition::Contained,
                    findings: vec![],
                    policy_snapshot_hash: String::new(),
                    phase: String::new(),
                },
                usage: known_usage(),
                unavailable_fields: BTreeMap::new(),
            })
        }
    }
    fn known_usage() -> ExecutionUsage {
        ExecutionUsage {
            input_tokens: Some(1),
            output_tokens: Some(1),
            cached_tokens: Some(0),
            total_tokens: Some(2),
            estimated_cost_microusd: None,
            pricing_provenance: None,
            unavailable_fields: BTreeMap::new(),
        }
    }
    fn test_scope_policy(allowed: &[&str]) -> ScopePolicySnapshot {
        compile_scope_policy(ScopePolicyInput {
            prd_path: "docs/prds/PRD-000.md".into(),
            prd_content_hash: "sha256:test".into(),
            contract: vec![],
            allowed_paths: allowed.iter().map(|value| (*value).to_owned()).collect(),
            allow_prd_expected_file_expansion: false,
            declaration_mode: ScopeDeclarationMode::ExpectedOrConfigured,
            prohibited_rules: vec![
                ProhibitedRule {
                    rule_id: "no_dependency_manifest_changes".into(),
                    rule: ProhibitedRuleKind::FileClass {
                        class: ScopeFileClass::DependencyManifest,
                    },
                    change_kinds: vec![],
                    description: Some("dependency changes are prohibited".into()),
                },
                ProhibitedRule {
                    rule_id: "no_dependency_lockfile_changes".into(),
                    rule: ProhibitedRuleKind::FileClass {
                        class: ScopeFileClass::DependencyLockfile,
                    },
                    change_kinds: vec![],
                    description: Some("dependency changes are prohibited".into()),
                },
            ],
            file_class_policies: ScopeFileClassPolicies::default(),
            classification_rules: vec![],
            baseline_revision: "base".into(),
            config_provenance: "test".into(),
        })
        .expect("test scope policy compiles")
    }
    fn base_request() -> CoordinationRequest {
        CoordinationRequest {
            cycle_id: "scenario".into(),
            task: ReviewTask {
                task_id: "task".into(),
                objective: "objective".into(),
                acceptance_criteria: vec!["criterion".into()],
                base_revision: "base".into(),
                allowed_paths: vec!["src/".into()],
                prohibited_changes: vec!["dependency changes".into()],
                verification_plan_id: "plan".into(),
            },
            implementation: AgentObservation {
                assignment: assignment(AgentRole::Implementation, "implementation"),
                agent_version: None,
                reported_model: None,
                unavailable_fields: BTreeMap::new(),
            },
            reviewer: assignment(AgentRole::Review, "review"),
            standard_reviewer: None,
            tier_policy: ReviewTierPolicy::default(),
            declared_risk_classes: vec![],
            contracts: vec![],
            invariants: vec![],
            verification_plan: VerificationPlan {
                plan_id: "plan".into(),
                checks: vec![VerificationCheck {
                    check_id: "test".into(),
                    argv: vec!["true".into()],
                    working_directory: ".".into(),
                    environment: BTreeMap::new(),
                    timeout_ms: 100,
                    required: true,
                    path_prefixes: vec!["src/".into()],
                }],
                full_after_remediation: false,
            },
            package_budget: ReviewPackageBudget {
                max_bytes: 10_000,
                max_estimated_tokens: 10_000,
            },
            limits: WorkflowLimits {
                max_review_attempts: 2,
                max_remediation_attempts: 2,
                max_total_tokens: None,
                max_total_cost_microusd: None,
                max_total_duration_ms: Some(10_000),
                review_reservation_tokens: None,
                remediation_reservation_tokens: None,
                action_reservation_cost_microusd: None,
                action_reservation_duration_ms: 10,
            },
            allow_same_model_fallback: true,
            implementation_usage: known_usage(),
            implementation_duration_ms: 1,
            scope_policy: test_scope_policy(&["src/"]),
        }
    }
    #[test]
    fn checks_only_runs_applicable_verification_without_review_attempt() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &Reviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let mut request = base_request();
        request.tier_policy.rules.push(ReviewTierRule {
            id: "small-source".into(),
            tier: ReviewTier::ChecksOnly,
            path_prefixes: vec!["src/".into()],
            max_changed_files: Some(1),
            max_changed_bytes: Some(10),
            change_kinds: vec![GitChangeKind::Modified],
            scope_classes: vec![ScopeFileClass::OrdinarySource],
        });
        let cycle = coordinator
            .run(Path::new("."), request, &mut Vec::new())
            .unwrap();
        assert_eq!(cycle.state, ReviewCycleState::Completed);
        assert_eq!(cycle.verification_before_review.len(), 1);
        assert!(cycle.review_attempts.is_empty());
        assert!(cycle.reviewer.is_none());
        assert_eq!(cycle.tier_selection.unwrap().tier, ReviewTier::ChecksOnly);
    }
    #[test]
    fn standard_uses_configured_independent_reviewer_identity() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &Reviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let mut request = base_request();
        request.tier_policy.rules.push(ReviewTierRule {
            id: "standard-source".into(),
            tier: ReviewTier::Standard,
            path_prefixes: vec!["src/".into()],
            max_changed_files: Some(1),
            max_changed_bytes: Some(10),
            change_kinds: vec![GitChangeKind::Modified],
            scope_classes: vec![ScopeFileClass::OrdinarySource],
        });
        let mut cheaper = assignment(AgentRole::Review, "standard");
        cheaper.agent_id = "economy-reviewer".into();
        cheaper.provider = Some("independent".into());
        cheaper.requested_model = Some("economy".into());
        request.standard_reviewer = Some(cheaper.clone());
        let cycle = coordinator
            .run(Path::new("."), request, &mut Vec::new())
            .unwrap();
        assert_eq!(cycle.tier_selection.unwrap().tier, ReviewTier::Standard);
        assert_eq!(cycle.reviewer.unwrap().assignment, cheaper);
        assert_eq!(cycle.review_attempts.len(), 1);
    }
    struct FailingVerifier;
    impl VerificationRunner for FailingVerifier {
        fn run(
            &self,
            repository: &Path,
            check: &VerificationCheck,
            id: &str,
        ) -> Result<VerificationEvidence, VerificationError> {
            let mut evidence = Verifier.run(repository, check, id)?;
            evidence.status = VerificationStatus::Failed;
            evidence.exit_code = Some(1);
            Ok(evidence)
        }
    }
    struct NoIsolationReviewer;
    impl ReviewAgent for NoIsolationReviewer {
        fn review(
            &self,
            r: &ReviewRequest,
            o: &mut dyn Write,
        ) -> Result<ReviewResult, ReviewExecutionError> {
            Reviewer.review(r, o)
        }
    }
    struct OutsideCollector;
    impl EvidenceCollector for OutsideCollector {
        fn capture(&self, _: &Path, _: &str) -> Result<CapturedDiff, EvidenceError> {
            Ok(CapturedDiff {
                base_revision: "base".into(),
                resulting_tree: "result".into(),
                changed_files: vec![ChangedFile {
                    path: "outside.txt".into(),
                    kind: GitChangeKind::Added,
                    old_path: None,
                    line_summary: vec![LineRange { start: 1, end: 1 }],
                }],
                diff: reference(),
                bytes: b"diff --git a/outside.txt b/outside.txt\n@@ -0,0 +1 @@\n+x\n".to_vec(),
            })
        }
    }
    struct ConflictingReviewer;
    impl ReviewAgent for ConflictingReviewer {
        fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
            Reviewer.isolation_evidence()
        }
        fn review(
            &self,
            r: &ReviewRequest,
            o: &mut dyn Write,
        ) -> Result<ReviewResult, ReviewExecutionError> {
            let mut result = Reviewer.review(r, o)?;
            let evidence = FindingEvidence::FileRange {
                path: "src/lib.rs".into(),
                range: LineRange { start: 1, end: 1 },
            };
            result.findings = vec![
                ReviewFinding {
                    finding_id: "a".into(),
                    category: FindingCategory::ScopeViolation,
                    severity: FindingSeverity::High,
                    blocking: false,
                    title: "a".into(),
                    claim: "scope".into(),
                    evidence: vec![evidence.clone()],
                    remediation: "remove".into(),
                    status: FindingStatus::Open,
                    supersedes: None,
                },
                ReviewFinding {
                    finding_id: "b".into(),
                    category: FindingCategory::ScopeViolation,
                    severity: FindingSeverity::High,
                    blocking: false,
                    title: "b".into(),
                    claim: "scope".into(),
                    evidence: vec![evidence],
                    remediation: "keep".into(),
                    status: FindingStatus::Open,
                    supersedes: None,
                },
            ];
            Ok(result)
        }
    }
    struct MalformedThenClean {
        calls: RefCell<u32>,
    }
    impl ReviewAgent for MalformedThenClean {
        fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
            Reviewer.isolation_evidence()
        }
        fn review(
            &self,
            r: &ReviewRequest,
            o: &mut dyn Write,
        ) -> Result<ReviewResult, ReviewExecutionError> {
            *self.calls.borrow_mut() += 1;
            let mut result = Reviewer.review(r, o)?;
            if *self.calls.borrow() == 1 {
                result.reviewed_manifest_hash = "wrong".into()
            }
            Ok(result)
        }
    }
    struct FailingImplementer {
        calls: RefCell<u32>,
    }
    impl RemediationAgent for FailingImplementer {
        fn remediate(
            &self,
            _: &RemediationRequest,
            _: &mut dyn Write,
        ) -> Result<RemediationResult, RemediationExecutionError> {
            *self.calls.borrow_mut() += 1;
            Err(RemediationExecutionError::Agent("failed".into()))
        }
    }
    struct PersistentBlockingReviewer;
    impl ReviewAgent for PersistentBlockingReviewer {
        fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
            Reviewer.isolation_evidence()
        }
        fn review(
            &self,
            request: &ReviewRequest,
            output: &mut dyn Write,
        ) -> Result<ReviewResult, ReviewExecutionError> {
            RemediatingReviewer {
                calls: RefCell::new(0),
            }
            .review(request, output)
        }
    }
    struct ExpandingCollector {
        calls: RefCell<u32>,
    }
    impl EvidenceCollector for ExpandingCollector {
        fn capture(&self, repository: &Path, base: &str) -> Result<CapturedDiff, EvidenceError> {
            *self.calls.borrow_mut() += 1;
            if *self.calls.borrow() == 1 {
                Collector.capture(repository, base)
            } else {
                OutsideCollector.capture(repository, base)
            }
        }
    }
    #[derive(Default)]
    struct Store {
        cycles: RefCell<Vec<ReviewCycle>>,
    }
    impl ReviewStore for Store {
        fn save_cycle(&self, c: &ReviewCycle) -> Result<(), String> {
            self.cycles.borrow_mut().push(c.clone());
            Ok(())
        }
        fn save_artifact(&self, _: &str, v: &[u8]) -> Result<ArtifactRef, String> {
            let mut e = reference();
            e.byte_size = v.len() as u64;
            Ok(e)
        }
    }
    #[test]
    fn clean_fake_integration_stops_at_human_approval() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &Reviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let req = CoordinationRequest {
            cycle_id: "c".into(),
            task: ReviewTask {
                task_id: "t".into(),
                objective: "o".into(),
                acceptance_criteria: vec!["a".into()],
                base_revision: "base".into(),
                allowed_paths: vec!["src/".into()],
                prohibited_changes: vec![],
                verification_plan_id: "v".into(),
            },
            implementation: AgentObservation {
                assignment: assignment(AgentRole::Implementation, "impl"),
                agent_version: None,
                reported_model: None,
                unavailable_fields: BTreeMap::new(),
            },
            reviewer: assignment(AgentRole::Review, "review"),
            standard_reviewer: None,
            tier_policy: ReviewTierPolicy::default(),
            declared_risk_classes: vec![],
            contracts: vec![],
            invariants: vec![],
            verification_plan: VerificationPlan {
                plan_id: "v".into(),
                checks: vec![VerificationCheck {
                    check_id: "test".into(),
                    argv: vec!["true".into()],
                    working_directory: ".".into(),
                    environment: BTreeMap::new(),
                    timeout_ms: 100,
                    required: true,
                    path_prefixes: vec!["src/".into()],
                }],
                full_after_remediation: false,
            },
            package_budget: ReviewPackageBudget {
                max_bytes: 10000,
                max_estimated_tokens: 10000,
            },
            limits: WorkflowLimits {
                max_review_attempts: 1,
                max_remediation_attempts: 1,
                max_total_tokens: Some(100),
                max_total_cost_microusd: None,
                max_total_duration_ms: None,
                review_reservation_tokens: Some(10),
                remediation_reservation_tokens: Some(10),
                action_reservation_cost_microusd: None,
                action_reservation_duration_ms: 10,
            },
            allow_same_model_fallback: true,
            implementation_usage: known_usage(),
            implementation_duration_ms: 1,
            scope_policy: test_scope_policy(&["src/"]),
        };
        let result = coordinator
            .run(Path::new("."), req, &mut Vec::new())
            .unwrap();
        assert_eq!(result.disposition, ReviewDisposition::ReadyForHumanApproval);
        assert_eq!(result.stop_reasons, vec![ReviewStopReason::CleanReview]);
        assert!(store.cycles.borrow().len() >= 4)
    }

    #[test]
    fn blocking_finding_runs_one_fake_remediation_and_requires_new_review() {
        let store = Store::default();
        let reviewer = RemediatingReviewer {
            calls: RefCell::new(0),
        };
        let implementer = SuccessfulImplementer {
            calls: RefCell::new(0),
        };
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &reviewer,
            implementer: &implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let request = CoordinationRequest {
            cycle_id: "remediation-cycle".into(),
            task: ReviewTask {
                task_id: "task".into(),
                objective: "objective".into(),
                acceptance_criteria: vec!["criterion".into()],
                base_revision: "base".into(),
                allowed_paths: vec!["src/".into()],
                prohibited_changes: vec![],
                verification_plan_id: "plan".into(),
            },
            implementation: AgentObservation {
                assignment: assignment(AgentRole::Implementation, "implementation"),
                agent_version: None,
                reported_model: None,
                unavailable_fields: BTreeMap::new(),
            },
            reviewer: assignment(AgentRole::Review, "review"),
            standard_reviewer: None,
            tier_policy: ReviewTierPolicy::default(),
            declared_risk_classes: vec![],
            contracts: vec![],
            invariants: vec![],
            verification_plan: VerificationPlan {
                plan_id: "plan".into(),
                checks: vec![VerificationCheck {
                    check_id: "test".into(),
                    argv: vec!["true".into()],
                    working_directory: ".".into(),
                    environment: BTreeMap::new(),
                    timeout_ms: 100,
                    required: true,
                    path_prefixes: vec!["src/".into()],
                }],
                full_after_remediation: false,
            },
            package_budget: ReviewPackageBudget {
                max_bytes: 10_000,
                max_estimated_tokens: 10_000,
            },
            limits: WorkflowLimits {
                max_review_attempts: 2,
                max_remediation_attempts: 1,
                max_total_tokens: Some(100),
                max_total_cost_microusd: None,
                max_total_duration_ms: None,
                review_reservation_tokens: Some(10),
                remediation_reservation_tokens: Some(10),
                action_reservation_cost_microusd: None,
                action_reservation_duration_ms: 10,
            },
            allow_same_model_fallback: true,
            implementation_usage: known_usage(),
            implementation_duration_ms: 1,
            scope_policy: test_scope_policy(&["src/"]),
        };
        let cycle = coordinator
            .run(Path::new("."), request, &mut Vec::new())
            .unwrap();
        assert_eq!(*reviewer.calls.borrow(), 2);
        assert_eq!(*implementer.calls.borrow(), 1);
        assert_eq!(cycle.disposition, ReviewDisposition::ReadyForHumanApproval);
        assert_eq!(cycle.attempt, 2);
    }
    #[test]
    fn absent_independent_reviewer_stops_before_review() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &NoIsolationReviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let cycle = coordinator
            .run(Path::new("."), base_request(), &mut Vec::new())
            .unwrap();
        assert_eq!(
            cycle.stop_reasons,
            vec![ReviewStopReason::NoIndependentReviewer]
        );
        assert_eq!(cycle.attempt, 0)
    }
    #[test]
    fn failed_required_verification_stops_before_review() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &FailingVerifier,
            reviewer: &Reviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let cycle = coordinator
            .run(Path::new("."), base_request(), &mut Vec::new())
            .unwrap();
        assert_eq!(
            cycle.stop_reasons,
            vec![ReviewStopReason::VerificationUnsuccessful]
        );
        assert_eq!(cycle.attempt, 0)
    }
    #[test]
    fn scope_expansion_stops_without_remediation() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &OutsideCollector,
            verifier: &Verifier,
            reviewer: &Reviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let request = base_request();
        let policy_hash = request.scope_policy.snapshot_hash.clone();
        let cycle = coordinator
            .run(Path::new("."), request, &mut Vec::new())
            .unwrap();
        assert_eq!(cycle.stop_reasons, vec![ReviewStopReason::ScopeBroadened]);
        assert_eq!(cycle.attempt, 0);
        assert!(cycle.verification_before_review.is_empty());
        assert!(cycle.scope_policy_snapshot.is_some());
        let evaluation = cycle.scope_evaluations.last().unwrap();
        assert_eq!(evaluation.phase, "initial");
        assert_eq!(evaluation.policy_snapshot_hash, policy_hash);
        let finding = &evaluation.findings[0];
        assert_eq!(finding.path, "outside.txt");
        assert_eq!(finding.change_kind, GitChangeKind::Added);
        assert_eq!(finding.decision, ScopeDecision::UndeclaredScopeExpansion);
        assert_eq!(finding.rule_id, "undeclared_change");
        let persisted = store.cycles.borrow();
        let stored = persisted.last().unwrap();
        assert_eq!(stored.scope_evaluations, cycle.scope_evaluations);
    }

    struct UnmergedCollector;
    impl EvidenceCollector for UnmergedCollector {
        fn capture(&self, _: &Path, _: &str) -> Result<CapturedDiff, EvidenceError> {
            Ok(CapturedDiff {
                base_revision: "base".into(),
                resulting_tree: "result".into(),
                changed_files: vec![ChangedFile {
                    path: "src/conflict.rs".into(),
                    kind: GitChangeKind::Unmerged,
                    old_path: None,
                    line_summary: vec![LineRange { start: 1, end: 1 }],
                }],
                diff: reference(),
                bytes: b"diff".to_vec(),
            })
        }
    }
    #[test]
    fn ambiguous_scope_stops_with_distinct_reason_before_verification_and_review() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &UnmergedCollector,
            verifier: &FailingVerifier,
            reviewer: &Reviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let cycle = coordinator
            .run(Path::new("."), base_request(), &mut Vec::new())
            .unwrap();
        assert_eq!(cycle.stop_reasons, vec![ReviewStopReason::ScopeAmbiguous]);
        assert_eq!(cycle.state, ReviewCycleState::HumanReviewRequired);
        assert_eq!(cycle.disposition, ReviewDisposition::HumanReviewRequired);
        assert_eq!(cycle.attempt, 0);
        assert!(cycle.verification_before_review.is_empty());
        let finding = &cycle.scope_evaluations[0].findings[0];
        assert_eq!(finding.decision, ScopeDecision::AmbiguousHumanReview);
        assert_eq!(finding.rule_id, "evidence:unmerged");
    }
    #[test]
    fn conflicting_findings_stop_without_choosing() {
        let store = Store::default();
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &ConflictingReviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let cycle = coordinator
            .run(Path::new("."), base_request(), &mut Vec::new())
            .unwrap();
        assert_eq!(
            cycle.stop_reasons,
            vec![ReviewStopReason::ConflictingFindings]
        )
    }
    #[test]
    fn malformed_review_consumes_attempt_and_retries() {
        let store = Store::default();
        let reviewer = MalformedThenClean {
            calls: RefCell::new(0),
        };
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &reviewer,
            implementer: &Implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let cycle = coordinator
            .run(Path::new("."), base_request(), &mut Vec::new())
            .unwrap();
        assert_eq!(*reviewer.calls.borrow(), 2);
        assert_eq!(cycle.disposition, ReviewDisposition::ReadyForHumanApproval);
        assert_eq!(cycle.review_attempts.len(), 2)
    }
    #[test]
    fn remediation_launch_failures_exhaust_bound() {
        let store = Store::default();
        let reviewer = RemediatingReviewer {
            calls: RefCell::new(0),
        };
        let implementer = FailingImplementer {
            calls: RefCell::new(0),
        };
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &reviewer,
            implementer: &implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let cycle = coordinator
            .run(Path::new("."), base_request(), &mut Vec::new())
            .unwrap();
        assert_eq!(*implementer.calls.borrow(), 2);
        assert_eq!(cycle.stop_reasons, vec![ReviewStopReason::AgentFailure]);
        assert_eq!(cycle.remediation_attempts.len(), 2)
    }
    #[test]
    fn persistent_blocking_finding_exhausts_remediation() {
        let store = Store::default();
        let implementer = SuccessfulImplementer {
            calls: RefCell::new(0),
        };
        let coordinator = ReviewCoordinator {
            collector: &Collector,
            verifier: &Verifier,
            reviewer: &PersistentBlockingReviewer,
            implementer: &implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let mut request = base_request();
        request.limits.max_review_attempts = 3;
        request.limits.max_remediation_attempts = 1;
        let cycle = coordinator
            .run(Path::new("."), request, &mut Vec::new())
            .unwrap();
        assert_eq!(*implementer.calls.borrow(), 1);
        assert_eq!(
            cycle.stop_reasons,
            vec![ReviewStopReason::RetryLimitExhausted]
        );
    }
    #[test]
    fn post_remediation_scope_expansion_stops_without_revert() {
        let store = Store::default();
        let collector = ExpandingCollector {
            calls: RefCell::new(0),
        };
        let implementer = SuccessfulImplementer {
            calls: RefCell::new(0),
        };
        let coordinator = ReviewCoordinator {
            collector: &collector,
            verifier: &Verifier,
            reviewer: &PersistentBlockingReviewer,
            implementer: &implementer,
            store: &store,
            policy: BlockingPolicy::default(),
        };
        let request = base_request();
        let policy_hash = request.scope_policy.snapshot_hash.clone();
        let cycle = coordinator
            .run(Path::new("."), request, &mut Vec::new())
            .unwrap();
        assert_eq!(cycle.stop_reasons, vec![ReviewStopReason::ScopeBroadened]);
        assert_eq!(*implementer.calls.borrow(), 1);
        // Both evaluations reuse the identical compiled policy snapshot.
        assert_eq!(cycle.scope_evaluations.len(), 2);
        assert_eq!(cycle.scope_evaluations[0].phase, "initial");
        assert_eq!(cycle.scope_evaluations[1].phase, "remediation-1");
        assert!(cycle
            .scope_evaluations
            .iter()
            .all(|evaluation| evaluation.policy_snapshot_hash == policy_hash));
        let remediation = cycle.remediation_result.as_ref().unwrap();
        assert_eq!(remediation.scope_check.policy_snapshot_hash, policy_hash);
        assert_eq!(
            remediation.scope_check.disposition,
            ScopeDisposition::Broadened
        );
        assert!(!remediation.scope_check.findings.is_empty());
    }

    struct FailingStore {
        after: u32,
        saves: RefCell<u32>,
    }
    impl ReviewStore for FailingStore {
        fn save_cycle(&self, _: &ReviewCycle) -> Result<(), String> {
            let mut saves = self.saves.borrow_mut();
            *saves += 1;
            if *saves > self.after {
                Err("injected storage failure".into())
            } else {
                Ok(())
            }
        }
        fn save_artifact(&self, kind: &str, value: &[u8]) -> Result<ArtifactRef, String> {
            Store::default().save_artifact(kind, value)
        }
    }
    #[test]
    fn storage_failure_around_scope_persistence_prevents_review_invocation() {
        // Fail on the very first save (before scope findings persist) and on
        // the save that persists the initial scope evaluation.
        for after in [0, 1] {
            let store = FailingStore {
                after,
                saves: RefCell::new(0),
            };
            let coordinator = ReviewCoordinator {
                collector: &Collector,
                verifier: &Verifier,
                reviewer: &Reviewer,
                implementer: &Implementer,
                store: &store,
                policy: BlockingPolicy::default(),
            };
            let error = coordinator
                .run(Path::new("."), base_request(), &mut Vec::new())
                .unwrap_err();
            assert!(matches!(error, CoordinatorError::Persistence(_)));
        }
    }
}
