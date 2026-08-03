use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Java,
    Cpp,
    C,
    Ruby,
    Shell,
    Markdown,
    Toml,
    Json,
    Yaml,
    Other(String),
}

impl Language {
    /// Short stable name used as a tag.
    pub fn name(&self) -> &str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Go => "go",
            Language::Java => "java",
            Language::Cpp => "cpp",
            Language::C => "c",
            Language::Ruby => "ruby",
            Language::Shell => "shell",
            Language::Markdown => "markdown",
            Language::Toml => "toml",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Other(s) => s.as_str(),
        }
    }

    /// Broad category used as a tag: code | config | docs | data
    pub fn broad_category(&self) -> &'static str {
        match self {
            Language::Rust
            | Language::Python
            | Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::Java
            | Language::Cpp
            | Language::C
            | Language::Ruby
            | Language::Shell => "code",
            Language::Toml | Language::Yaml => "config",
            Language::Json => "data",
            Language::Markdown => "docs",
            Language::Other(_) => "data",
        }
    }
}

pub fn detect_language(path: &Path) -> Language {
    // Special filenames
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name == "Dockerfile" || name.starts_with("Dockerfile.") {
            return Language::Other("dockerfile".into());
        }
        if name == "Makefile" || name.ends_with(".mk") {
            return Language::Other("makefile".into());
        }
    }

    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_lowercase(),
        None => return Language::Other("unknown".into()),
    };

    match ext.as_str() {
        "rs" => Language::Rust,
        "py" => Language::Python,
        "js" | "mjs" | "cjs" => Language::JavaScript,
        "ts" | "tsx" => Language::TypeScript,
        "jsx" => Language::JavaScript,
        "go" => Language::Go,
        "java" => Language::Java,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Language::Cpp,
        "c" | "h" => Language::C,
        "rb" => Language::Ruby,
        "sh" | "bash" | "zsh" => Language::Shell,
        "md" | "markdown" => Language::Markdown,
        "toml" => Language::Toml,
        "json" => Language::Json,
        "yaml" | "yml" => Language::Yaml,
        other => Language::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_rust() {
        assert_eq!(detect_language(&PathBuf::from("foo.rs")), Language::Rust);
    }

    #[test]
    fn detects_python() {
        assert_eq!(detect_language(&PathBuf::from("foo.py")), Language::Python);
    }

    #[test]
    fn detects_typescript() {
        assert_eq!(
            detect_language(&PathBuf::from("foo.ts")),
            Language::TypeScript
        );
        assert_eq!(
            detect_language(&PathBuf::from("foo.tsx")),
            Language::TypeScript
        );
    }

    #[test]
    fn detects_javascript_variants() {
        assert_eq!(
            detect_language(&PathBuf::from("foo.js")),
            Language::JavaScript
        );
        assert_eq!(
            detect_language(&PathBuf::from("foo.mjs")),
            Language::JavaScript
        );
    }

    #[test]
    fn detects_dockerfile() {
        assert_eq!(
            detect_language(&PathBuf::from("Dockerfile")).name(),
            "dockerfile"
        );
        assert_eq!(
            detect_language(&PathBuf::from("Dockerfile.test")).name(),
            "dockerfile"
        );
    }

    #[test]
    fn detects_makefile() {
        assert_eq!(
            detect_language(&PathBuf::from("Makefile")).name(),
            "makefile"
        );
        assert_eq!(detect_language(&PathBuf::from("foo.mk")).name(), "makefile");
    }

    #[test]
    fn unknown_extension_is_other() {
        assert_eq!(
            detect_language(&PathBuf::from("foo.xyz")),
            Language::Other("xyz".into())
        );
    }

    #[test]
    fn no_extension_is_unknown() {
        assert_eq!(
            detect_language(&PathBuf::from("README")),
            Language::Other("unknown".into())
        );
    }

    #[test]
    fn case_insensitive_extension() {
        assert_eq!(detect_language(&PathBuf::from("Foo.RS")), Language::Rust);
    }

    #[test]
    fn broad_categories() {
        assert_eq!(Language::Rust.broad_category(), "code");
        assert_eq!(Language::Toml.broad_category(), "config");
        assert_eq!(Language::Markdown.broad_category(), "docs");
        assert_eq!(Language::Json.broad_category(), "data");
    }
}
