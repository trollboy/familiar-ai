use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const EXPECTED_FILES_HEADING: &str = "Expected Files";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedMatchKind {
    ExactFile,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFileEntry {
    pub source_line: u64,
    pub bullet_text: String,
    pub normalized: String,
    pub match_kind: ExpectedMatchKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFilesContract {
    pub prd_path: String,
    pub prd_content_hash: String,
    pub entries: Vec<ExpectedFileEntry>,
}

impl ExpectedFileEntry {
    pub fn matches(&self, path: &str) -> bool {
        match self.match_kind {
            ExpectedMatchKind::ExactFile => path == self.normalized,
            ExpectedMatchKind::Directory => {
                path.starts_with(&self.normalized) && path.len() > self.normalized.len()
            }
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ExpectedFilesError {
    #[error("no authoritative `## Expected Files` heading found")]
    MissingHeading,
    #[error("duplicate `## Expected Files` heading at line {line}")]
    DuplicateHeading { line: u64 },
    #[error("Expected Files section contains no top-level bullet list")]
    MissingBulletList,
    #[error("bullet at line {line} has no inline-code path expression")]
    NoPathExpression { line: u64 },
    #[error("bullet at line {line} contains an unclosed inline-code span")]
    UnclosedCodeSpan { line: u64 },
    #[error("bullet at line {line} has an empty path expression")]
    EmptyExpression { line: u64 },
    #[error("bullet at line {line} has unsupported path expression '{expression}': {rule}")]
    UnsupportedExpression {
        line: u64,
        expression: String,
        rule: ScopePathRule,
    },
    #[error("bullet at line {line} duplicates normalized expression '{normalized}'")]
    DuplicateExpression { line: u64, normalized: String },
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopePathRule {
    #[error("empty expression")]
    Empty,
    #[error("absolute paths are not supported")]
    AbsolutePath,
    #[error("backslashes are not supported")]
    Backslash,
    #[error("whitespace-joined expressions are not supported")]
    Whitespace,
    #[error("home expansion is not supported")]
    HomeExpansion,
    #[error("variable expansion is not supported")]
    VariableExpansion,
    #[error("URI forms are not supported")]
    UriForm,
    #[error("glob syntax other than a terminal '/**' is not supported")]
    UnsupportedGlob,
    #[error("empty, '.' or '..' path components are not supported")]
    InvalidComponent,
}

/// Normalize one path expression under the closed Expected Files grammar.
/// `dir/**` is normalized to `dir/`; a trailing `/` means every descendant.
pub fn normalize_scope_path(
    expression: &str,
) -> Result<(String, ExpectedMatchKind), ScopePathRule> {
    if expression.is_empty() {
        return Err(ScopePathRule::Empty);
    }
    if expression.starts_with('/') {
        return Err(ScopePathRule::AbsolutePath);
    }
    if expression.contains('\\') {
        return Err(ScopePathRule::Backslash);
    }
    if expression.chars().any(char::is_whitespace) {
        return Err(ScopePathRule::Whitespace);
    }
    if expression.starts_with('~') {
        return Err(ScopePathRule::HomeExpansion);
    }
    if expression.contains('$') {
        return Err(ScopePathRule::VariableExpansion);
    }
    if expression.contains(':') {
        return Err(ScopePathRule::UriForm);
    }
    let (body, kind) = if let Some(prefix) = expression.strip_suffix("/**") {
        (prefix, ExpectedMatchKind::Directory)
    } else if let Some(prefix) = expression.strip_suffix('/') {
        (prefix, ExpectedMatchKind::Directory)
    } else {
        (expression, ExpectedMatchKind::ExactFile)
    };
    if body.is_empty() {
        return Err(ScopePathRule::InvalidComponent);
    }
    if body.contains(['*', '?', '{', '}', '[', ']']) {
        return Err(ScopePathRule::UnsupportedGlob);
    }
    if body
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ScopePathRule::InvalidComponent);
    }
    let normalized = match kind {
        ExpectedMatchKind::ExactFile => body.to_owned(),
        ExpectedMatchKind::Directory => format!("{body}/"),
    };
    Ok((normalized, kind))
}

/// Parse the single authoritative `## Expected Files` section of a PRD.
/// Deterministic; no heuristics. Entries are returned in document order.
pub fn parse_expected_files(content: &str) -> Result<Vec<ExpectedFileEntry>, ExpectedFilesError> {
    let lines: Vec<&str> = content.lines().collect();
    let mut heading = None;
    for (index, line) in lines.iter().enumerate() {
        if let Some(text) = line.strip_prefix("## ") {
            if text.trim() == EXPECTED_FILES_HEADING {
                if heading.is_some() {
                    return Err(ExpectedFilesError::DuplicateHeading {
                        line: (index + 1) as u64,
                    });
                }
                heading = Some(index);
            }
        }
    }
    let start = heading.ok_or(ExpectedFilesError::MissingHeading)?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| {
            (line.starts_with("# ") || line.starts_with("## ")) && !line.starts_with("###")
        })
        .map(|(index, _)| index)
        .unwrap_or(lines.len());

    let mut entries: Vec<ExpectedFileEntry> = Vec::new();
    let mut index = start + 1;
    while index < end {
        let line = lines[index];
        if !line.starts_with("- ") {
            index += 1;
            continue;
        }
        let bullet_line = (index + 1) as u64;
        let mut block = String::from(line);
        index += 1;
        while index < end && !lines[index].starts_with("- ") && !lines[index].trim().is_empty() {
            block.push('\n');
            block.push_str(lines[index]);
            index += 1;
        }
        let expression = first_code_span(&block).map_err(|error| match error {
            SpanError::None => ExpectedFilesError::NoPathExpression { line: bullet_line },
            SpanError::Unclosed => ExpectedFilesError::UnclosedCodeSpan { line: bullet_line },
        })?;
        if expression.is_empty() {
            return Err(ExpectedFilesError::EmptyExpression { line: bullet_line });
        }
        let (normalized, match_kind) =
            normalize_scope_path(&expression).map_err(|rule| match rule {
                ScopePathRule::Empty => ExpectedFilesError::EmptyExpression { line: bullet_line },
                rule => ExpectedFilesError::UnsupportedExpression {
                    line: bullet_line,
                    expression: expression.clone(),
                    rule,
                },
            })?;
        if entries.iter().any(|entry| entry.normalized == normalized) {
            return Err(ExpectedFilesError::DuplicateExpression {
                line: bullet_line,
                normalized,
            });
        }
        entries.push(ExpectedFileEntry {
            source_line: bullet_line,
            bullet_text: line.to_owned(),
            normalized,
            match_kind,
        });
    }
    if entries.is_empty() {
        return Err(ExpectedFilesError::MissingBulletList);
    }
    Ok(entries)
}

enum SpanError {
    None,
    Unclosed,
}

fn first_code_span(block: &str) -> Result<String, SpanError> {
    let Some(open) = block.find('`') else {
        return Err(SpanError::None);
    };
    let rest = &block[open + 1..];
    let Some(close) = rest.find('`') else {
        return Err(SpanError::Unclosed);
    };
    Ok(rest[..close].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(section: &str) -> String {
        format!("# PRD\n\n## Objective\n\ntext\n\n## Expected Files\n\n{section}\n\n## Definition of Done\n\ndone\n")
    }

    #[test]
    fn parses_exact_directory_and_glob_bullets_with_provenance() {
        let content = document(
            "The implementation is expected to modify or add only:\n\n\
             - `src/lib.rs` for exports\n\
             - `crates/review/` — every descendant\n\
             - `crates/storage/**` only if required\n\
             - `docs/spec.md`\n   continuation prose with `other/path.rs` granting nothing",
        );
        let entries = parse_expected_files(&content).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.normalized.as_str(), e.match_kind))
                .collect::<Vec<_>>(),
            vec![
                ("src/lib.rs", ExpectedMatchKind::ExactFile),
                ("crates/review/", ExpectedMatchKind::Directory),
                ("crates/storage/", ExpectedMatchKind::Directory),
                ("docs/spec.md", ExpectedMatchKind::ExactFile),
            ]
        );
        assert_eq!(entries[0].source_line, 11);
        assert!(entries[1].matches("crates/review/src/policy.rs"));
        assert!(!entries[1].matches("crates/review"));
        assert!(entries[3].matches("docs/spec.md"));
        assert!(!entries[3].matches("docs/spec.md.bak"));
    }

    #[test]
    fn nested_bullets_and_prose_grant_no_authority() {
        let content = document(
            "- `src/lib.rs` top level\n\
             \u{20}\u{20}- `src/nested.rs` nested bullet\n\
             prose mentioning `src/prose.rs` outside any bullet",
        );
        let entries = parse_expected_files(&content).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].normalized, "src/lib.rs");
    }

    #[test]
    fn missing_and_duplicate_headings_fail() {
        assert_eq!(
            parse_expected_files("# PRD\n\n## Scope\n\n- `a`\n"),
            Err(ExpectedFilesError::MissingHeading)
        );
        let duplicated =
            "## Expected Files\n\n- `a.rs`\n\n## Other\n\n## Expected Files\n\n- `b.rs`\n";
        assert_eq!(
            parse_expected_files(duplicated),
            Err(ExpectedFilesError::DuplicateHeading { line: 7 })
        );
    }

    #[test]
    fn section_boundaries_stop_at_level_one_or_two_headings_only() {
        let content =
            "## Expected Files\n\n- `a.rs`\n\n### Sub\n\n- `b.rs`\n\n# Next\n\n- `c.rs`\n";
        let entries = parse_expected_files(content).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| e.normalized.as_str())
                .collect::<Vec<_>>(),
            vec!["a.rs", "b.rs"]
        );
    }

    #[test]
    fn nested_only_or_empty_sections_fail() {
        assert_eq!(
            parse_expected_files(&document("prose only, no bullets")),
            Err(ExpectedFilesError::MissingBulletList)
        );
        assert_eq!(
            parse_expected_files(&document("\u{20}\u{20}- `src/nested.rs` nested only")),
            Err(ExpectedFilesError::MissingBulletList)
        );
    }

    #[test]
    fn bullets_without_valid_first_spans_fail_with_line() {
        assert_eq!(
            parse_expected_files(&document("- no code span at all")),
            Err(ExpectedFilesError::NoPathExpression { line: 9 })
        );
        assert_eq!(
            parse_expected_files(&document("- unclosed `src/lib.rs span")),
            Err(ExpectedFilesError::UnclosedCodeSpan { line: 9 })
        );
        assert_eq!(
            parse_expected_files(&document("- empty `` span")),
            Err(ExpectedFilesError::EmptyExpression { line: 9 })
        );
        assert_eq!(
            parse_expected_files(&document("- joined `a.rs b.rs` span")),
            Err(ExpectedFilesError::UnsupportedExpression {
                line: 9,
                expression: "a.rs b.rs".into(),
                rule: ScopePathRule::Whitespace,
            })
        );
    }

    #[test]
    fn unsupported_expressions_are_rejected() {
        for (expression, rule) in [
            ("/abs/path.rs", ScopePathRule::AbsolutePath),
            ("a\\b.rs", ScopePathRule::Backslash),
            ("~/home.rs", ScopePathRule::HomeExpansion),
            ("$VAR/x.rs", ScopePathRule::VariableExpansion),
            ("file://x.rs", ScopePathRule::UriForm),
            ("src/*.rs", ScopePathRule::UnsupportedGlob),
            ("src/**/x.rs", ScopePathRule::UnsupportedGlob),
            ("src/{a,b}.rs", ScopePathRule::UnsupportedGlob),
            ("src/[ab].rs", ScopePathRule::UnsupportedGlob),
            ("src/a?.rs", ScopePathRule::UnsupportedGlob),
            ("./x.rs", ScopePathRule::InvalidComponent),
            ("a/../b.rs", ScopePathRule::InvalidComponent),
            ("a//b.rs", ScopePathRule::InvalidComponent),
        ] {
            assert_eq!(
                parse_expected_files(&document(&format!("- `{expression}` bullet"))),
                Err(ExpectedFilesError::UnsupportedExpression {
                    line: 9,
                    expression: expression.into(),
                    rule,
                }),
                "expression {expression} should fail with {rule:?}"
            );
        }
    }

    #[test]
    fn duplicate_normalized_expressions_fail() {
        assert_eq!(
            parse_expected_files(&document("- `crates/x/**` first\n- `crates/x/` second")),
            Err(ExpectedFilesError::DuplicateExpression {
                line: 10,
                normalized: "crates/x/".into(),
            })
        );
    }
}
