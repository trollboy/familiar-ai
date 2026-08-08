//! Simple heuristics for the inference router.
//! Determines whether a model call is needed, and provides fallback
//! scoring/profile selection when no LLM is available.

use familiar_ai_core::config::BudgetProfile;
use familiar_ai_tokens::estimate_tokens;

/// Returns false when the input is trivial enough that no model call is needed.
pub fn needs_model(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Only punctuation
    if trimmed
        .chars()
        .all(|c| c.is_ascii_punctuation() || c.is_whitespace())
    {
        return false;
    }
    // Only numbers
    if trimmed
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '.' || c == ',')
    {
        return false;
    }
    // Very short (< 10 estimated tokens)
    if estimate_tokens(trimmed) < 10 {
        return false;
    }
    true
}

/// Heuristic budget profile selection based on task text length.
pub fn heuristic_packer_profile(task: &str) -> BudgetProfile {
    let tokens = estimate_tokens(task);
    if tokens < 20 {
        BudgetProfile::Minimal
    } else if tokens < 100 {
        BudgetProfile::Balanced
    } else if tokens < 500 {
        BudgetProfile::Aggressive
    } else {
        BudgetProfile::MaxAccuracy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportanceScore {
    Low,
    Medium,
    High,
}

/// High-importance keywords (auth, security, payment, etc.)
const HIGH_KEYWORDS: &[&str] = &[
    "auth",
    "security",
    "password",
    "credential",
    "token",
    "secret",
    "payment",
    "billing",
    "privacy",
    "encrypt",
    "decrypt",
    "certificate",
    "vulnerability",
    "exploit",
    "injection",
    "permission",
    "rbac",
    "oauth",
];

/// Low-importance indicators
const LOW_KEYWORDS: &[&str] = &[
    "readme",
    "changelog",
    "license",
    "comment",
    "todo",
    "fixme",
    "test_helper",
    "fixture",
    "mock",
    "snapshot",
    "docs/",
];

/// Heuristic importance scoring based on keyword presence.
pub fn heuristic_importance(input: &str) -> ImportanceScore {
    let lower = input.to_lowercase();
    if HIGH_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return ImportanceScore::High;
    }
    if LOW_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return ImportanceScore::Low;
    }
    ImportanceScore::Medium
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_does_not_need_model() {
        assert!(!needs_model(""));
        assert!(!needs_model("   "));
    }

    #[test]
    fn punctuation_only_does_not_need_model() {
        assert!(!needs_model("...!!!???"));
    }

    #[test]
    fn numbers_only_does_not_need_model() {
        assert!(!needs_model("12345 67.89"));
    }

    #[test]
    fn very_short_does_not_need_model() {
        assert!(!needs_model("hi"));
    }

    #[test]
    fn real_content_needs_model() {
        assert!(needs_model(
            "Implement authentication token refresh with Redis backing store"
        ));
    }

    #[test]
    fn heuristic_profile_tiny() {
        assert_eq!(heuristic_packer_profile("fix bug"), BudgetProfile::Minimal);
    }

    #[test]
    fn heuristic_profile_medium() {
        let task = "Implement the authentication flow using JWT tokens with refresh capability and Redis session storage";
        assert_eq!(heuristic_packer_profile(task), BudgetProfile::Balanced);
    }

    #[test]
    fn heuristic_profile_large() {
        let task = "word ".repeat(600); // 3000 chars = 750 estimated tokens → MaxAccuracy
        assert_eq!(heuristic_packer_profile(&task), BudgetProfile::MaxAccuracy);
    }

    #[test]
    fn importance_auth_is_high() {
        assert_eq!(
            heuristic_importance("implement auth token rotation"),
            ImportanceScore::High
        );
    }

    #[test]
    fn importance_security_is_high() {
        assert_eq!(
            heuristic_importance("fix security vulnerability in payment flow"),
            ImportanceScore::High
        );
    }

    #[test]
    fn importance_readme_is_low() {
        assert_eq!(
            heuristic_importance("update readme with new instructions"),
            ImportanceScore::Low
        );
    }

    #[test]
    fn importance_test_fixture_is_low() {
        assert_eq!(
            heuristic_importance("add test_helper for mock database"),
            ImportanceScore::Low
        );
    }

    #[test]
    fn importance_generic_is_medium() {
        assert_eq!(
            heuristic_importance("refactor the database connection pool"),
            ImportanceScore::Medium
        );
    }
}
