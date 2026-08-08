//! Tiny heuristic token estimator. Char-based approximation: tokens ≈ chars / 4.
//!
//! Replace with a real tokenizer in a future PRD if accuracy becomes important.
//! Used by PRD-007 (rollup truncation) and PRD-009 (task packer).

const CHARS_PER_TOKEN_APPROX: usize = 4;

/// Estimate the token count of a UTF-8 string using a char-based approximation.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN_APPROX)
}

/// Truncate text so its estimated token count fits within `max_tokens`.
/// Walks back to the nearest whitespace boundary so the cut isn't mid-word.
/// Returns `(possibly_truncated_text, was_truncated)`.
pub fn truncate_to_tokens(text: &str, max_tokens: usize) -> (String, bool) {
    if estimate_tokens(text) <= max_tokens {
        return (text.to_string(), false);
    }
    let max_chars = max_tokens.saturating_mul(CHARS_PER_TOKEN_APPROX);
    let marker = " ... [truncated]";
    let marker_len = marker.chars().count();

    // If the budget is smaller than the marker itself, return just the marker
    // to avoid weird underflow behavior at extreme tiny budgets.
    if max_chars <= marker_len {
        return (marker.trim().to_string(), true);
    }

    let cap = max_chars - marker_len;
    let raw: String = text.chars().take(cap).collect();

    // Walk back to the last whitespace (graceful word boundary)
    let trimmed = match raw.rfind(char::is_whitespace) {
        Some(idx) if idx > 0 => raw[..idx].trim_end().to_string(),
        _ => raw,
    };

    (format!("{trimmed}{marker}"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_short_string() {
        // 4 chars → 1 token
        assert_eq!(estimate_tokens("abcd"), 1);
        // 8 chars → 2 tokens
        assert_eq!(estimate_tokens("abcdefgh"), 2);
        // 5 chars → ceil(5/4) = 2 tokens
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn estimate_unicode_counts_chars_not_bytes() {
        // "héllo" is 5 chars but multiple bytes
        assert_eq!(estimate_tokens("héllo"), 2);
        // 4 emoji chars
        assert_eq!(estimate_tokens("🦀🦀🦀🦀"), 1);
    }

    #[test]
    fn truncate_within_budget_unchanged() {
        let (out, was) = truncate_to_tokens("hello", 1000);
        assert_eq!(out, "hello");
        assert!(!was);
    }

    #[test]
    fn truncate_over_budget_appends_marker() {
        let text = "the quick brown fox jumps over the lazy dog";
        // budget of 3 tokens = 12 chars max, minus marker length
        let (out, was) = truncate_to_tokens(text, 3);
        assert!(was);
        assert!(out.ends_with("... [truncated]"));
        // Should be much shorter than original
        assert!(out.len() < text.len());
    }

    #[test]
    fn truncate_walks_to_word_boundary() {
        // 30 chars, all words separated
        let text = "alpha beta gamma delta epsilon";
        let (out, was) = truncate_to_tokens(text, 4); // ~16 chars
        assert!(was);
        // Should not end mid-word — last char before marker should be whitespace-free
        // and the word should be intact (not "epsil" for example)
        let before_marker = out.trim_end_matches(" ... [truncated]");
        // None of the original words should be partially present
        for word in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            // Either the word is fully there or not at all
            let occurrences = before_marker.matches(word).count();
            let fragments = before_marker.match_indices(word).count();
            assert_eq!(occurrences, fragments);
        }
    }

    #[test]
    fn truncate_extreme_tiny_budget_returns_marker() {
        let (out, was) = truncate_to_tokens("hello world", 0);
        assert!(was);
        // Should be just the marker, no underflow
        assert_eq!(out, "... [truncated]");
    }

    #[test]
    fn truncate_marker_only_for_tiny_budgets() {
        // Budget of 1 token = 4 chars, less than marker (16 chars)
        let (out, was) = truncate_to_tokens("hello world", 1);
        assert!(was);
        assert_eq!(out, "... [truncated]");
    }

    #[test]
    fn truncate_no_whitespace_falls_back_to_raw() {
        // Long string with no spaces
        let text = "a".repeat(100);
        let (out, was) = truncate_to_tokens(&text, 5); // ~20 chars
        assert!(was);
        assert!(out.ends_with("... [truncated]"));
    }

    #[test]
    fn truncate_empty_string_unchanged() {
        let (out, was) = truncate_to_tokens("", 10);
        assert_eq!(out, "");
        assert!(!was);
    }

    #[test]
    fn truncate_exact_budget() {
        // exactly 4 chars = 1 token, budget = 1
        let (out, was) = truncate_to_tokens("abcd", 1);
        assert_eq!(out, "abcd");
        assert!(!was);
    }
}
