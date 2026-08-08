use std::path::Path;

use crate::extractor::{extract, ExtractedFile};
use crate::language::{detect_language, Language};

#[derive(Debug, Clone)]
pub struct GeneratedSummary {
    pub summary_text: String,
    pub tags: Vec<String>,
    pub extracted_symbols: Vec<String>,
    pub line_count: usize,
}

/// Stateless summary generator. Future LLM-backed implementations can swap
/// in here without changing the call sites.
pub struct SummaryGenerator;

impl SummaryGenerator {
    pub fn new() -> Self {
        Self
    }

    pub fn generate(&self, path: &Path, content: &str) -> GeneratedSummary {
        let language = detect_language(path);
        let extracted = extract(content, &language);

        let tags = build_tags(path, &language, &extracted);
        let summary_text = synthesize_text(path, &language, &extracted);

        GeneratedSummary {
            summary_text,
            tags,
            extracted_symbols: extracted.symbols,
            line_count: extracted.line_count,
        }
    }
}

impl Default for SummaryGenerator {
    fn default() -> Self {
        Self::new()
    }
}

fn build_tags(path: &Path, language: &Language, extracted: &ExtractedFile) -> Vec<String> {
    let mut tags = Vec::new();
    tags.push(language.name().to_string());

    if extracted.is_binary {
        tags.push("binary".into());
        return tags;
    }

    tags.push(language.broad_category().to_string());

    let path_str = path.to_string_lossy().to_lowercase();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    // test detection
    if path_str.contains("/test")
        || path_str.contains("/tests/")
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.rs")
        || file_name.ends_with("_test.go")
        || file_name.ends_with(".test.js")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".spec.js")
        || file_name.ends_with(".spec.ts")
    {
        tags.push("test".into());
    }

    // migration detection
    if path_str.contains("/migrations/") || file_name.contains("migration") {
        tags.push("migration".into());
    }

    // schema detection
    if file_name.starts_with("schema.")
        || file_name.ends_with(".proto")
        || file_name.ends_with(".graphql")
        || file_name.ends_with(".sql")
    {
        tags.push("schema".into());
    }

    // build tooling detection
    if matches!(language, Language::Other(ref s) if s == "dockerfile" || s == "makefile") {
        tags.push("build".into());
    }

    // api detection (heuristic: paths with /api/ or /routes/ or /handlers/)
    if path_str.contains("/api/")
        || path_str.contains("/routes/")
        || path_str.contains("/handlers/")
    {
        tags.push("api".into());
    }

    tags
}

fn synthesize_text(path: &Path, language: &Language, extracted: &ExtractedFile) -> String {
    if extracted.is_binary {
        return format!("Binary file ({} bytes).", extracted.byte_size);
    }

    let lang_display = language.name();
    let path_display = path.display();

    let symbol_summary = if extracted.symbols.is_empty() {
        String::new()
    } else {
        let n = extracted.symbols.len();
        let preview: Vec<&str> = extracted
            .symbols
            .iter()
            .take(8)
            .map(|s| s.as_str())
            .collect();
        format!(
            " Defines {n} top-level symbol{}: {}{}.",
            if n == 1 { "" } else { "s" },
            preview.join(", "),
            if n > preview.len() { ", ..." } else { "" }
        )
    };

    let doc_part = match &extracted.first_docblock {
        Some(d) => format!(" Doc: \"{d}\"."),
        None => String::new(),
    };

    format!(
        "{lang_display} file at {path_display} ({} lines).{symbol_summary}{doc_part}",
        extracted.line_count
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rust_summary_includes_symbols() {
        let gen = SummaryGenerator::new();
        let src = "pub struct Foo {}\npub fn bar() {}\n";
        let result = gen.generate(&PathBuf::from("src/lib.rs"), src);
        assert!(result.summary_text.contains("rust"));
        assert!(result.summary_text.contains("Foo"));
        assert!(result.summary_text.contains("bar"));
        assert!(result.tags.contains(&"rust".to_string()));
        assert!(result.tags.contains(&"code".to_string()));
        assert_eq!(result.line_count, 2);
    }

    #[test]
    fn binary_summary() {
        let gen = SummaryGenerator::new();
        let content = unsafe { std::str::from_utf8_unchecked(b"\x00\x01\x02") };
        let result = gen.generate(&PathBuf::from("foo.bin"), content);
        assert!(result.summary_text.contains("Binary file"));
        assert!(result.tags.contains(&"binary".to_string()));
        assert!(result.extracted_symbols.is_empty());
    }

    #[test]
    fn test_tag_inferred_from_path() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(&PathBuf::from("tests/foo_test.rs"), "pub fn x() {}\n");
        assert!(result.tags.contains(&"test".to_string()));
    }

    #[test]
    fn migration_tag_inferred() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(
            &PathBuf::from("crates/familiar-ai-storage/migrations/001_init.sql"),
            "CREATE TABLE foo (id INTEGER);\n",
        );
        assert!(result.tags.contains(&"migration".to_string()));
        assert!(result.tags.contains(&"schema".to_string()));
    }

    #[test]
    fn dockerfile_gets_build_tag() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(&PathBuf::from("Dockerfile"), "FROM rust:1.88\n");
        assert!(result.tags.contains(&"build".to_string()));
    }

    #[test]
    fn api_tag_inferred() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(&PathBuf::from("src/api/users.rs"), "pub fn list() {}\n");
        assert!(result.tags.contains(&"api".to_string()));
    }

    #[test]
    fn empty_file_summary() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(&PathBuf::from("empty.rs"), "");
        assert!(result.summary_text.contains("0 lines"));
        assert!(result.extracted_symbols.is_empty());
    }

    #[test]
    fn doc_block_appears_in_summary() {
        let gen = SummaryGenerator::new();
        let src = "//! This is the module docstring.\n\npub fn x() {}\n";
        let result = gen.generate(&PathBuf::from("foo.rs"), src);
        assert!(result.summary_text.contains("module docstring"));
    }

    #[test]
    fn markdown_file_gets_docs_category() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(&PathBuf::from("README.md"), "# Title\n\nContent.\n");
        assert!(result.tags.contains(&"docs".to_string()));
    }

    #[test]
    fn json_file_gets_data_category() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(&PathBuf::from("config.json"), "{}\n");
        assert!(result.tags.contains(&"data".to_string()));
    }

    #[test]
    fn line_count_exposed() {
        let gen = SummaryGenerator::new();
        let result = gen.generate(&PathBuf::from("foo.rs"), "a\nb\nc\nd\n");
        assert_eq!(result.line_count, 4);
    }
}
