use once_cell::sync::Lazy;
use regex::Regex;

use crate::language::Language;

#[derive(Debug, Clone)]
pub struct ExtractedFile {
    pub line_count: usize,
    pub symbols: Vec<String>,
    pub first_docblock: Option<String>,
    pub is_binary: bool,
    pub byte_size: usize,
}

const BINARY_SAMPLE_SIZE: usize = 8192;
const NON_PRINTABLE_BINARY_RATIO: f32 = 0.30;

/// Detect whether content is binary.
/// Primary signal: any NUL byte in the first 8 KB.
/// Fallback: non-printable byte ratio > 30% in the sample.
pub fn looks_binary(content: &[u8]) -> bool {
    let sample_end = content.len().min(BINARY_SAMPLE_SIZE);
    let sample = &content[..sample_end];
    if sample.contains(&0u8) {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let non_printable = sample
        .iter()
        .filter(|&&b| {
            // Treat tab/lf/cr/space as printable; everything else outside ASCII printable
            // and outside high UTF-8 (>=0x80) as suspicious.
            !(b == b'\t' || b == b'\n' || b == b'\r' || (0x20..=0x7E).contains(&b) || b >= 0x80)
        })
        .count();
    let ratio = non_printable as f32 / sample.len() as f32;
    ratio > NON_PRINTABLE_BINARY_RATIO
}

// --- Per-language regexes ---

static RUST_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(?:pub(?:\([^)]+\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:fn|struct|enum|trait|impl|type|mod|const|static)\s+(\w+)").unwrap()
});

static PYTHON_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*(?:async\s+)?(?:def|class)\s+(\w+)").unwrap());

static JS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*(?:export\s+(?:default\s+)?)?(?:async\s+)?(?:function|class|const|let|var)\s+(\w+)",
    )
    .unwrap()
});

static GO_FUNC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*func\s+(?:\([^)]*\)\s+)?(\w+)").unwrap());
static GO_TYPE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*type\s+(\w+)").unwrap());

static JAVA_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*(?:public|private|protected)?\s*(?:static\s+)?(?:final\s+)?(?:class|interface|enum)\s+(\w+)",
    )
    .unwrap()
});

static C_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(?:static\s+|extern\s+|inline\s+)*(?:[\w*]+\s+)+([a-zA-Z_]\w*)\s*\(").unwrap()
});

static RUBY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*(?:def|class|module)\s+(\w+)").unwrap());

static MARKDOWN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s{0,3}#{1,6}\s+(.+?)\s*$").unwrap());

const MAX_SYMBOLS: usize = 64;

pub fn extract(content: &str, language: &Language) -> ExtractedFile {
    let bytes = content.as_bytes();
    let byte_size = bytes.len();
    let is_binary = looks_binary(bytes);

    if is_binary {
        return ExtractedFile {
            line_count: 0,
            symbols: Vec::new(),
            first_docblock: None,
            is_binary: true,
            byte_size,
        };
    }

    let line_count = if content.is_empty() {
        0
    } else {
        content.lines().count()
    };

    let symbols = match language {
        Language::Rust => extract_with_regex(content, &RUST_RE),
        Language::Python => extract_with_regex(content, &PYTHON_RE),
        Language::JavaScript | Language::TypeScript => extract_with_regex(content, &JS_RE),
        Language::Go => {
            let mut s = extract_with_regex(content, &GO_FUNC_RE);
            s.extend(extract_with_regex(content, &GO_TYPE_RE));
            dedup_truncate(s)
        }
        Language::Java => extract_with_regex(content, &JAVA_RE),
        Language::Cpp | Language::C => extract_with_regex(content, &C_RE),
        Language::Ruby => extract_with_regex(content, &RUBY_RE),
        Language::Markdown => extract_with_regex(content, &MARKDOWN_RE),
        _ => Vec::new(),
    };

    let first_docblock = extract_first_docblock(content, language);

    ExtractedFile {
        line_count,
        symbols,
        first_docblock,
        is_binary: false,
        byte_size,
    }
}

fn extract_with_regex(content: &str, re: &Regex) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        if let Some(caps) = re.captures(line) {
            if let Some(m) = caps.get(1) {
                out.push(m.as_str().to_string());
                if out.len() >= MAX_SYMBOLS {
                    break;
                }
            }
        }
    }
    dedup_truncate(out)
}

fn dedup_truncate(mut v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
    if v.len() > MAX_SYMBOLS {
        v.truncate(MAX_SYMBOLS);
    }
    v
}

fn extract_first_docblock(content: &str, language: &Language) -> Option<String> {
    let comment_prefixes: &[&str] = match language {
        Language::Rust => &["///", "//!", "//"],
        Language::JavaScript
        | Language::TypeScript
        | Language::Java
        | Language::Cpp
        | Language::C
        | Language::Go => &["///", "//"],
        Language::Python | Language::Ruby | Language::Shell | Language::Yaml | Language::Toml => {
            &["#"]
        }
        _ => &[],
    };

    if comment_prefixes.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    let mut started = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            if started {
                break;
            }
            continue;
        }
        let mut matched = false;
        for prefix in comment_prefixes {
            if trimmed.starts_with(prefix) {
                let stripped = trimmed
                    .strip_prefix(prefix)
                    .unwrap_or(trimmed)
                    .trim()
                    .to_string();
                if !stripped.is_empty() {
                    lines.push(stripped);
                }
                matched = true;
                started = true;
                break;
            }
        }
        if !matched {
            if started {
                break;
            }
            // Skip non-comment shebangs and other preamble
            if trimmed.starts_with("#!") {
                continue;
            }
            return None;
        }
    }

    if lines.is_empty() {
        None
    } else {
        let joined = lines.join(" ");
        let trimmed = joined.trim();
        // Cap docblock length so it doesn't dominate the summary
        const MAX_DOC_LEN: usize = 280;
        if trimmed.len() > MAX_DOC_LEN {
            Some(format!("{}...", &trimmed[..MAX_DOC_LEN]))
        } else {
            Some(trimmed.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detects_nul_byte() {
        assert!(looks_binary(b"hello\x00world"));
    }

    #[test]
    fn binary_does_not_flag_text() {
        assert!(!looks_binary(b"hello world\nthis is text"));
    }

    #[test]
    fn binary_handles_empty() {
        assert!(!looks_binary(b""));
    }

    #[test]
    fn extract_rust_symbols() {
        let src = r#"
//! Module docstring.

pub struct Config {
    pub name: String,
}

pub fn load_config() -> Config {
    Config { name: "x".into() }
}

fn helper() {}

pub trait Loader {}

impl Loader for Config {}
"#;
        let result = extract(src, &Language::Rust);
        assert!(result.symbols.contains(&"Config".to_string()));
        assert!(result.symbols.contains(&"load_config".to_string()));
        assert!(result.symbols.contains(&"helper".to_string()));
        assert!(result.symbols.contains(&"Loader".to_string()));
        assert!(!result.is_binary);
        assert!(result.line_count > 0);
    }

    #[test]
    fn extract_python_symbols() {
        let src = r#"
"""Module docstring."""

class MyClass:
    pass

def my_function():
    pass

async def async_fn():
    pass
"#;
        let result = extract(src, &Language::Python);
        assert!(result.symbols.contains(&"MyClass".to_string()));
        assert!(result.symbols.contains(&"my_function".to_string()));
        assert!(result.symbols.contains(&"async_fn".to_string()));
    }

    #[test]
    fn extract_javascript_symbols() {
        let src = r#"
// Top of file
export function foo() {}
class Bar {}
const baz = 42;
let qux = "hi";
export default function defaultFn() {}
"#;
        let result = extract(src, &Language::JavaScript);
        assert!(result.symbols.contains(&"foo".to_string()));
        assert!(result.symbols.contains(&"Bar".to_string()));
        assert!(result.symbols.contains(&"baz".to_string()));
        assert!(result.symbols.contains(&"qux".to_string()));
    }

    #[test]
    fn extract_go_symbols() {
        let src = r#"
package main

type Config struct {
    Name string
}

func (c *Config) Method() {}
func TopLevel() {}
"#;
        let result = extract(src, &Language::Go);
        assert!(result.symbols.contains(&"Config".to_string()));
        assert!(result.symbols.contains(&"TopLevel".to_string()));
    }

    #[test]
    fn extract_markdown_headings() {
        let src = "# Title\n\n## Section A\n\nText\n\n### Subsection\n";
        let result = extract(src, &Language::Markdown);
        assert!(result.symbols.iter().any(|s| s == "Title"));
        assert!(result.symbols.iter().any(|s| s == "Section A"));
    }

    #[test]
    fn first_docblock_rust_inner() {
        let src = "//! This is the module doc.\n//! Continues here.\n\npub fn foo() {}\n";
        let result = extract(src, &Language::Rust);
        let doc = result.first_docblock.unwrap();
        assert!(doc.contains("module doc"));
        assert!(doc.contains("Continues here"));
    }

    #[test]
    fn first_docblock_python() {
        let src = "# This is a top comment\n# Continued\n\ndef foo(): pass\n";
        let result = extract(src, &Language::Python);
        let doc = result.first_docblock.unwrap();
        assert!(doc.contains("top comment"));
    }

    #[test]
    fn first_docblock_skips_shebang() {
        let src = "#!/bin/bash\n# Real comment\n\necho hi\n";
        let result = extract(src, &Language::Shell);
        // Shebang skipped, comment captured
        assert!(result
            .first_docblock
            .as_ref()
            .is_some_and(|d| d.contains("Real comment")));
    }

    #[test]
    fn binary_content_returns_minimal() {
        let content_str = unsafe { std::str::from_utf8_unchecked(b"\x00\x01\x02\x03") };
        let result = extract(content_str, &Language::Other("bin".into()));
        assert!(result.is_binary);
        assert!(result.symbols.is_empty());
        assert_eq!(result.line_count, 0);
    }

    #[test]
    fn empty_file() {
        let result = extract("", &Language::Rust);
        assert_eq!(result.line_count, 0);
        assert!(result.symbols.is_empty());
        assert!(result.first_docblock.is_none());
    }

    #[test]
    fn line_count_counts_lines() {
        let src = "line1\nline2\nline3\n";
        let result = extract(src, &Language::Other("txt".into()));
        assert_eq!(result.line_count, 3);
    }

    #[test]
    fn symbol_dedup() {
        let src = r#"
fn foo() {}
fn foo() {}
fn bar() {}
"#;
        let result = extract(src, &Language::Rust);
        let foo_count = result.symbols.iter().filter(|s| *s == "foo").count();
        assert_eq!(foo_count, 1);
    }

    #[test]
    fn symbol_cap_enforced() {
        let mut src = String::new();
        for i in 0..200 {
            src.push_str(&format!("fn fn_{i}() {{}}\n"));
        }
        let result = extract(&src, &Language::Rust);
        assert!(result.symbols.len() <= MAX_SYMBOLS);
    }
}
