//! Keyword extraction from natural-language text.
//! Used by the context packer and search tools to match task descriptions
//! against file summaries and decisions.

use std::collections::HashSet;

const MAX_TERMS: usize = 10;
const MIN_TERM_LEN: usize = 3;

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "in", "on", "at", "to", "for", "of", "and", "or", "is", "it", "this", "that",
    "with", "from", "by", "as", "be", "was", "are", "not", "but", "if", "so", "we", "my", "do",
    "can", "has", "have", "had", "will", "would", "could", "should", "may", "about", "into", "all",
    "been", "its", "more", "some", "what", "when", "how", "who", "which", "their", "them", "then",
    "than", "only", "also", "just", "now", "new", "one", "two", "get", "set", "use", "very",
    "most", "each", "any", "our", "you", "your", "other",
];

/// Extract up to MAX_TERMS keyword tokens from text.
///
/// Lowercase, strip punctuation, split on whitespace, filter stopwords
/// and short words, deduplicate, cap at MAX_TERMS.
pub fn extract_keywords(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let cleaned: String = lower
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();

    let stopset: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut terms = Vec::new();

    for word in cleaned.split_whitespace() {
        if word.len() < MIN_TERM_LEN {
            continue;
        }
        if stopset.contains(word) {
            continue;
        }
        if seen.insert(word.to_string()) {
            terms.push(word.to_string());
            if terms.len() >= MAX_TERMS {
                break;
            }
        }
    }

    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_extraction() {
        let terms = extract_keywords("implement auth token refresh logic");
        assert!(terms.contains(&"implement".to_string()));
        assert!(terms.contains(&"auth".to_string()));
        assert!(terms.contains(&"token".to_string()));
        assert!(terms.contains(&"refresh".to_string()));
        assert!(terms.contains(&"logic".to_string()));
    }

    #[test]
    fn removes_stopwords() {
        let terms = extract_keywords("the quick brown fox and the lazy dog");
        assert!(!terms.contains(&"the".to_string()));
        assert!(!terms.contains(&"and".to_string()));
        assert!(terms.contains(&"quick".to_string()));
        assert!(terms.contains(&"brown".to_string()));
        assert!(terms.contains(&"fox".to_string()));
        assert!(terms.contains(&"lazy".to_string()));
        assert!(terms.contains(&"dog".to_string()));
    }

    #[test]
    fn filters_short_words() {
        let terms = extract_keywords("go to do it");
        // All are ≤2 chars or stopwords
        assert!(terms.is_empty());
    }

    #[test]
    fn deduplicates() {
        let terms = extract_keywords("auth auth auth token token");
        assert_eq!(terms.len(), 2);
        assert_eq!(terms[0], "auth");
        assert_eq!(terms[1], "token");
    }

    #[test]
    fn caps_at_max_terms() {
        let text =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron";
        let terms = extract_keywords(text);
        assert!(terms.len() <= MAX_TERMS);
    }

    #[test]
    fn handles_punctuation() {
        let terms = extract_keywords("auth-token, refresh_logic! (important)");
        // "auth-token" becomes "auth token", "refresh_logic" becomes "refresh logic"
        assert!(terms.contains(&"auth".to_string()));
        assert!(terms.contains(&"token".to_string()));
        assert!(terms.contains(&"refresh".to_string()));
        assert!(terms.contains(&"logic".to_string()));
        assert!(terms.contains(&"important".to_string()));
    }

    #[test]
    fn empty_input() {
        let terms = extract_keywords("");
        assert!(terms.is_empty());
    }

    #[test]
    fn case_insensitive() {
        let terms = extract_keywords("Auth TOKEN Refresh");
        assert!(terms.contains(&"auth".to_string()));
        assert!(terms.contains(&"token".to_string()));
        assert!(terms.contains(&"refresh".to_string()));
    }
}
