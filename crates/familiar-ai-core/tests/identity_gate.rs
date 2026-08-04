//! Rename-completeness gate: no pre-rename machine identifier may remain in
//! the implementation surface. Documented exceptions:
//! - `familiar.db` (agreed PRD-015 adjustment: the database file name is
//!   unchanged and migrates inside its directory, byte-identical);
//! - lines carrying an `identity-gate: allow` marker (the legacy layout used
//!   by the migration itself, the legacy env prefix used by stale-variable
//!   detection, and their tests);
//! - `crates/familiar-ai-review/tests/prd_fixtures.rs` (pins of historical
//!   PRD documents, which are byte-frozen and legitimately name old paths).
//!
//! Documentation under `docs/` is out of scope by the PRD itself.

use std::path::{Path, PathBuf};

const ALLOWED_FILES: [&str; 1] = ["crates/familiar-ai-review/tests/prd_fixtures.rs"];
const ALLOW_MARKER: &str = "identity-gate: allow";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn scan_targets(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join("Dockerfile"),
        root.join("docker-compose.yml"),
    ];
    collect(&root.join("config"), &mut files);
    collect(&root.join("crates"), &mut files);
    files
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            collect(&path, files);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "toml" || ext == "sql" || ext == "yml")
        {
            files.push(path);
        }
    }
}

/// Occurrences of `needle` in `line` that are not followed by `allowed_next`.
fn violates(line: &str, needle: &str, allowed_next: &str) -> bool {
    let mut from = 0;
    while let Some(found) = line[from..].find(needle) {
        let after = from + found + needle.len();
        if !line[after..].starts_with(allowed_next) {
            return true;
        }
        from = after;
    }
    false
}

#[test]
fn no_pre_rename_machine_identifier_remains() {
    // Patterns assembled from parts so this file never matches itself.
    let underscore = ["famil", "iar_"].concat();
    let dash = ["famil", "iar-"].concat();
    let env = ["FAMIL", "IAR_"].concat();
    let quoted_bare = ["\"famil", "iar\""].concat();
    let pid = ["famil", "iar.pid"].concat();
    let sock = ["famil", "iar.sock"].concat();
    let db_exception = ["famil", "iar.db"].concat();
    let macos_support = ["Support/Famil", "iar\""].concat();
    let macos_logs = ["Logs/Famil", "iar\""].concat();

    let root = repo_root();
    let mut violations = Vec::new();
    for file in scan_targets(&root) {
        let relative = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if ALLOWED_FILES.contains(&relative.as_str()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (index, raw_line) in content.lines().enumerate() {
            if raw_line.contains(ALLOW_MARKER) {
                continue;
            }
            let line = raw_line.replace(&db_exception, "");
            let bad = violates(&line, &underscore, "ai_")
                || violates(&line, &dash, "ai")
                || violates(&line, &env, "AI_")
                || line.contains(&quoted_bare)
                || line.contains(&pid)
                || line.contains(&sock)
                || line.contains(&macos_support)
                || line.contains(&macos_logs);
            if bad {
                violations.push(format!("{relative}:{}: {raw_line}", index + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "pre-rename identifiers remain:\n{}",
        violations.join("\n")
    );
}
