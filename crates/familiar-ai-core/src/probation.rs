use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerStanding {
    Probation,
    Promoted,
    Quarantined,
    Retired,
}

impl WorkerStanding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Probation => "probation",
            Self::Promoted => "promoted",
            Self::Quarantined => "quarantined",
            Self::Retired => "retired",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbationPolicy {
    pub policy_id: String,
    pub version: String,
    pub minimum_accepted_prds: u64,
    pub minimum_review_pass_basis_points: u32,
    pub maximum_remediation_basis_points: u32,
    pub maximum_failure_basis_points: u32,
    pub probation_max_expected_files: u64,
    pub require_independent_review: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpiricalMetrics {
    pub completed_prds: u64,
    pub accepted_prds: u64,
    pub review_passes: u64,
    pub review_attempts: u64,
    pub remediated_prds: u64,
    pub failed_prds: u64,
    pub latency_ms: u64,
    pub cache_read_tokens: u64,
    pub cache_eligible_tokens: u64,
    pub cost_amount: Option<u64>,
    pub cost_unit: Option<String>,
    pub cost_authority: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmpiricalScore {
    pub review_pass_basis_points: Option<u32>,
    pub remediation_basis_points: Option<u32>,
    pub failure_basis_points: Option<u32>,
    pub cache_effectiveness_basis_points: Option<u32>,
    pub latency_per_accepted_prd_ms: Option<u64>,
    pub cost_per_accepted_prd: Option<u64>,
    pub cost_unit: Option<String>,
    pub trusted_identity: bool,
    pub promotion_eligible: bool,
}

fn rate(numerator: u64, denominator: u64) -> Option<u32> {
    (denominator != 0).then(|| numerator.saturating_mul(10_000).saturating_div(denominator) as u32)
}

pub fn score(
    policy: &ProbationPolicy,
    metrics: &EmpiricalMetrics,
    trusted_identity: bool,
) -> EmpiricalScore {
    let review = rate(metrics.review_passes, metrics.review_attempts);
    let remediation = rate(metrics.remediated_prds, metrics.completed_prds);
    let failure = rate(metrics.failed_prds, metrics.completed_prds);
    let cache = rate(metrics.cache_read_tokens, metrics.cache_eligible_tokens);
    let known_cost = metrics
        .cost_amount
        .zip(metrics.cost_unit.clone())
        .and_then(|(amount, unit)| metrics.cost_authority.as_ref().map(|_| (amount, unit)));
    let (cost_per_accepted_prd, cost_unit) = known_cost
        .filter(|_| metrics.accepted_prds != 0)
        .map(|(amount, unit)| (Some(amount / metrics.accepted_prds), Some(unit)))
        .unwrap_or((None, None));
    let promotion_eligible = trusted_identity
        && metrics.accepted_prds >= policy.minimum_accepted_prds
        && review.is_some_and(|v| v >= policy.minimum_review_pass_basis_points)
        && remediation.is_some_and(|v| v <= policy.maximum_remediation_basis_points)
        && failure.is_some_and(|v| v <= policy.maximum_failure_basis_points);
    EmpiricalScore {
        review_pass_basis_points: review,
        remediation_basis_points: remediation,
        failure_basis_points: failure,
        cache_effectiveness_basis_points: cache,
        latency_per_accepted_prd_ms: (metrics.accepted_prds != 0)
            .then(|| metrics.latency_ms / metrics.accepted_prds),
        cost_per_accepted_prd,
        cost_unit,
        trusted_identity,
        promotion_eligible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_identity_and_cost_never_become_trusted_or_cheap() {
        let policy = ProbationPolicy {
            policy_id: "p".into(),
            version: "1".into(),
            minimum_accepted_prds: 1,
            minimum_review_pass_basis_points: 10_000,
            maximum_remediation_basis_points: 0,
            maximum_failure_basis_points: 0,
            probation_max_expected_files: 1,
            require_independent_review: true,
        };
        let result = score(
            &policy,
            &EmpiricalMetrics {
                completed_prds: 1,
                accepted_prds: 1,
                review_passes: 1,
                review_attempts: 1,
                ..Default::default()
            },
            false,
        );
        assert!(!result.promotion_eligible);
        assert_eq!(result.cost_per_accepted_prd, None);
    }
}
