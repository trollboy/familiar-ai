//! Relevance scoring for file summaries and decisions against extracted keywords.

use familiar_core::models::{Decision, FileSummary};

#[derive(Debug, Clone)]
pub struct ScoredItem<T> {
    pub item: T,
    pub score: u32,
    pub matched_terms: Vec<String>,
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

pub fn score_file_summary<'a>(
    summary: &'a FileSummary,
    terms: &[String],
    original_query: &str,
) -> ScoredItem<&'a FileSummary> {
    let mut score: u32 = 0;
    let mut matched = Vec::new();
    let query_lower = original_query.to_lowercase();

    // +5 exact phrase match on path or summary
    if !query_lower.is_empty()
        && (contains_ci(&summary.path, &query_lower) || contains_ci(&summary.summary, &query_lower))
    {
        score += 5;
    }

    for term in terms {
        let mut hit = false;
        // +3 path match
        if contains_ci(&summary.path, term) {
            score += 3;
            hit = true;
        }
        // +2 summary text match
        if contains_ci(&summary.summary, term) {
            score += 2;
            hit = true;
        }
        // +1 symbol match
        if summary
            .extracted_symbols
            .iter()
            .any(|s| contains_ci(s, term))
        {
            score += 1;
            hit = true;
        }
        if hit {
            matched.push(term.clone());
        }
    }

    matched.sort();
    matched.dedup();

    ScoredItem {
        item: summary,
        score,
        matched_terms: matched,
    }
}

pub fn score_decision<'a>(
    decision: &'a Decision,
    terms: &[String],
    original_query: &str,
) -> ScoredItem<&'a Decision> {
    let mut score: u32 = 0;
    let mut matched = Vec::new();
    let query_lower = original_query.to_lowercase();

    // +5 exact phrase match on title or summary
    if !query_lower.is_empty()
        && (contains_ci(&decision.title, &query_lower)
            || contains_ci(&decision.summary, &query_lower))
    {
        score += 5;
    }

    for term in terms {
        let mut hit = false;
        // +3 title match
        if contains_ci(&decision.title, term) {
            score += 3;
            hit = true;
        }
        // +2 summary match
        if contains_ci(&decision.summary, term) {
            score += 2;
            hit = true;
        }
        // +1 related_files match
        if decision.related_files.iter().any(|f| contains_ci(f, term)) {
            score += 1;
            hit = true;
        }
        if hit {
            matched.push(term.clone());
        }
    }

    matched.sort();
    matched.dedup();

    ScoredItem {
        item: decision,
        score,
        matched_terms: matched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_file_summary(path: &str, summary: &str, symbols: &[&str]) -> FileSummary {
        let now = Utc::now();
        FileSummary {
            id: 1,
            project_id: 1,
            path: path.into(),
            summary: summary.into(),
            tags: vec![],
            extracted_symbols: symbols.iter().map(|s| s.to_string()).collect(),
            last_known_mtime: None,
            last_known_size: None,
            last_updated_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    fn make_decision(title: &str, summary: &str, related: &[&str]) -> Decision {
        let now = Utc::now();
        Decision {
            id: 1,
            project_id: 1,
            title: title.into(),
            summary: summary.into(),
            related_files: related.iter().map(|s| s.to_string()).collect(),
            source_session: None,
            confidence: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn file_summary_all_matches() {
        let fs = make_file_summary(
            "src/auth/token.rs",
            "Handles JWT token validation",
            &["validate_token", "TokenStore"],
        );
        let terms = vec!["auth".into(), "token".into()];
        let scored = score_file_summary(&fs, &terms, "auth token");

        // exact phrase match (+5) + auth in path (+3) + token in path (+3) +
        // auth NOT in summary + token in summary (+2) + token in symbol (+1)
        assert!(scored.score > 0);
        assert!(scored.matched_terms.contains(&"auth".to_string()));
        assert!(scored.matched_terms.contains(&"token".to_string()));
    }

    #[test]
    fn file_summary_no_match() {
        let fs = make_file_summary("src/main.rs", "Entry point", &["main"]);
        let terms = vec!["database".into()];
        let scored = score_file_summary(&fs, &terms, "database");
        assert_eq!(scored.score, 0);
        assert!(scored.matched_terms.is_empty());
    }

    #[test]
    fn exact_phrase_bonus() {
        let fs = make_file_summary("src/lib.rs", "auth token refresh logic", &[]);
        let terms = vec!["auth".into(), "token".into(), "refresh".into()];
        let scored = score_file_summary(&fs, &terms, "auth token refresh");
        // Should have the +5 exact phrase bonus
        assert!(scored.score >= 5);
    }

    #[test]
    fn decision_scoring() {
        let d = make_decision(
            "Keep auth stateless",
            "JWT remains the auth mechanism",
            &["src/auth/token.rs"],
        );
        let terms = vec!["auth".into()];
        let scored = score_decision(&d, &terms, "auth");
        // title +3, summary +2, related +1, exact +5 = 11
        assert!(scored.score > 0);
        assert!(scored.matched_terms.contains(&"auth".to_string()));
    }

    #[test]
    fn decision_no_match() {
        let d = make_decision("Use PostgreSQL", "Primary data store", &[]);
        let terms = vec!["redis".into()];
        let scored = score_decision(&d, &terms, "redis");
        assert_eq!(scored.score, 0);
    }

    #[test]
    fn matched_terms_deduped() {
        let fs = make_file_summary("src/auth.rs", "auth module for authentication", &["auth"]);
        let terms = vec!["auth".into()];
        let scored = score_file_summary(&fs, &terms, "auth");
        // "auth" appears in path, summary, and symbols — but matched_terms should only list it once
        assert_eq!(scored.matched_terms.len(), 1);
    }
}
