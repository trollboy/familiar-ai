use std::collections::BTreeSet;
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

const REFERENCE_PREFIXES: [&str; 2] = ["docs/adr/", "docs/contracts/"];

const EXECUTION_CONSTRAINTS: &str = r#"- Implement the supplied PRD exactly as written and do not broaden its scope.
- Treat repository source and Git state as authoritative.
- Inspect the existing implementation and identify blocking conflicts before editing.
- Do not modify architecture documents, ADRs, contracts, or existing PRDs.
- Do not implement later PRDs or perform unrelated cleanup.
- Preserve existing user changes in the worktree.
- When implementation is complete, audit every acceptance criterion, run focused tests, formatting, static analysis, and attempt the workspace test suite.
- Distinguish implementation-caused failures from pre-existing failures and summarize changed files and deviations.
- Stop after completing and reporting the supplied PRD."#;

#[derive(Debug)]
pub enum RunError {
    CurrentDirectory(io::Error),
    RepositoryRootNotFound(PathBuf),
    InvalidPrdPath(String),
    ReadDocument {
        path: PathBuf,
        source: io::Error,
    },
    Spawn {
        executable: String,
        source: io::Error,
    },
    Feed(io::Error),
    Wait(io::Error),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(error) => write!(f, "cannot resolve current directory: {error}"),
            Self::RepositoryRootNotFound(path) => write!(
                f,
                "no Git repository root found at or above {}",
                path.display()
            ),
            Self::InvalidPrdPath(message) => f.write_str(message),
            Self::ReadDocument { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Self::Spawn { executable, source } => {
                write!(f, "cannot launch Codex executable {executable:?}: {source}")
            }
            Self::Feed(error) => write!(f, "cannot feed execution prompt to Codex: {error}"),
            Self::Wait(error) => write!(f, "cannot wait for Codex: {error}"),
        }
    }
}

impl std::error::Error for RunError {}

pub fn execute(prd_path: &Path, codex_executable: &str) -> Result<ExitStatus, RunError> {
    let current_dir = env::current_dir().map_err(RunError::CurrentDirectory)?;
    let repository_root = resolve_repository_root(&current_dir)?;
    let prompt = build_prompt(&repository_root, prd_path)?;

    let mut child = Command::new(codex_executable)
        .arg("exec")
        .arg("-")
        .current_dir(&repository_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|source| RunError::Spawn {
            executable: codex_executable.to_owned(),
            source,
        })?;

    let feed_result = child
        .stdin
        .take()
        .expect("piped child stdin must be available")
        .write_all(prompt.as_bytes());

    let status = child.wait().map_err(RunError::Wait)?;
    match feed_result {
        Ok(()) => Ok(status),
        Err(_) if !status.success() => Ok(status),
        Err(error) => Err(RunError::Feed(error)),
    }
}

pub fn resolve_repository_root(start: &Path) -> Result<PathBuf, RunError> {
    let start = start.canonicalize().map_err(RunError::CurrentDirectory)?;

    start
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
        .ok_or(RunError::RepositoryRootNotFound(start))
}

pub fn build_prompt(repository_root: &Path, supplied_path: &Path) -> Result<String, RunError> {
    let repository_root = repository_root
        .canonicalize()
        .map_err(RunError::CurrentDirectory)?;
    let prd_path = validate_prd_path(&repository_root, supplied_path)?;
    let prd = read_utf8(&prd_path)?;
    let references = discover_references(&repository_root, &prd)?;
    let relative_prd = prd_path
        .strip_prefix(&repository_root)
        .expect("validated path");

    let mut prompt = String::new();
    prompt.push_str("# Familiar execution request\n\n");
    prompt.push_str("Implement the PRD below in this repository.\n\n");
    prompt.push_str("## Fixed execution constraints\n\n");
    prompt.push_str(EXECUTION_CONSTRAINTS);
    prompt.push_str("\n\n## PRD: ");
    prompt.push_str(&relative_prd.to_string_lossy());
    prompt.push_str("\n\n");
    prompt.push_str(&prd);
    prompt.push('\n');

    for reference in references {
        let relative = reference
            .strip_prefix(&repository_root)
            .expect("contained reference");
        prompt.push_str("\n## Directly referenced document: ");
        prompt.push_str(&relative.to_string_lossy());
        prompt.push_str("\n\n");
        prompt.push_str(&read_utf8(&reference)?);
        prompt.push('\n');
    }

    Ok(prompt)
}

fn validate_prd_path(repository_root: &Path, supplied_path: &Path) -> Result<PathBuf, RunError> {
    if supplied_path.as_os_str().is_empty() {
        return Err(RunError::InvalidPrdPath("PRD path cannot be empty".into()));
    }

    let candidate = if supplied_path.is_absolute() {
        supplied_path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(RunError::CurrentDirectory)?
            .join(supplied_path)
    };
    let path = candidate.canonicalize().map_err(|error| {
        RunError::InvalidPrdPath(format!(
            "cannot resolve PRD path {}: {error}",
            candidate.display()
        ))
    })?;
    let prd_dir = repository_root
        .join("docs/prds")
        .canonicalize()
        .map_err(|error| {
            RunError::InvalidPrdPath(format!("cannot resolve repository PRD directory: {error}"))
        })?;

    if !path.starts_with(&prd_dir) {
        return Err(RunError::InvalidPrdPath(format!(
            "PRD path must be contained in {}",
            prd_dir.display()
        )));
    }
    if !path.is_file() {
        return Err(RunError::InvalidPrdPath(format!(
            "PRD path is not a regular file: {}",
            path.display()
        )));
    }
    if path.extension().and_then(|value| value.to_str()) != Some("md") {
        return Err(RunError::InvalidPrdPath(format!(
            "PRD path must have a .md extension: {}",
            path.display()
        )));
    }

    Ok(path)
}

fn discover_references(repository_root: &Path, prd: &str) -> Result<Vec<PathBuf>, RunError> {
    let mut relative_paths = BTreeSet::new();

    for prefix in REFERENCE_PREFIXES {
        let mut remainder = prd;
        while let Some(index) = remainder.find(prefix) {
            remainder = &remainder[index..];
            let end = remainder
                .find(|character: char| {
                    !(character.is_ascii_alphanumeric()
                        || matches!(character, '/' | '-' | '_' | '.'))
                })
                .unwrap_or(remainder.len());
            let reference = remainder[..end].trim_end_matches('.');
            if reference.ends_with(".md") {
                relative_paths.insert(reference.to_owned());
            }
            remainder = &remainder[end..];
        }
    }

    relative_paths
        .into_iter()
        .map(|relative| {
            let path = repository_root
                .join(&relative)
                .canonicalize()
                .map_err(|error| {
                    RunError::InvalidPrdPath(format!(
                        "directly referenced document {relative} cannot be resolved: {error}"
                    ))
                })?;
            let allowed_directory = if relative.starts_with("docs/adr/") {
                repository_root.join("docs/adr")
            } else {
                repository_root.join("docs/contracts")
            };
            if !path.starts_with(&allowed_directory) || !path.is_file() {
                return Err(RunError::InvalidPrdPath(format!(
                    "directly referenced document escapes its documentation directory: {relative}"
                )));
            }
            Ok(path)
        })
        .collect()
}

fn read_utf8(path: &Path) -> Result<String, RunError> {
    fs::read_to_string(path).map_err(|source| RunError::ReadDocument {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repository() -> TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::create_dir_all(temp.path().join("docs/prds")).unwrap();
        fs::create_dir_all(temp.path().join("docs/contracts")).unwrap();
        fs::create_dir_all(temp.path().join("docs/adr")).unwrap();
        temp
    }

    #[test]
    fn resolves_repository_root_from_descendant() {
        let repo = repository();
        let nested = repo.path().join("crates/example");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            resolve_repository_root(&nested).unwrap(),
            repo.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn rejects_path_outside_prd_directory() {
        let repo = repository();
        let outside = repo.path().join("README.md");
        fs::write(&outside, "not a PRD").unwrap();

        let error = build_prompt(repo.path(), &outside).unwrap_err();
        assert!(error.to_string().contains("must be contained"));
    }

    #[test]
    fn rejects_missing_prd() {
        let repo = repository();
        let error = build_prompt(repo.path(), Path::new("docs/prds/missing.md")).unwrap_err();
        assert!(error.to_string().contains("cannot resolve PRD path"));
    }

    #[test]
    fn builds_prompt_with_sorted_direct_references_and_constraints() {
        let repo = repository();
        fs::write(
            repo.path().join("docs/contracts/event-model.md"),
            "EVENT CONTRACT",
        )
        .unwrap();
        fs::write(
            repo.path().join("docs/contracts/command-model.md"),
            "COMMAND CONTRACT",
        )
        .unwrap();
        fs::write(repo.path().join("docs/adr/ADR-001-final.md"), "ADR CONTENT").unwrap();
        let prd = repo.path().join("docs/prds/PRD-003.md");
        fs::write(
            &prd,
            "# PRD\nSee `docs/contracts/event-model.md`, docs/contracts/command-model.md, and `docs/adr/ADR-001-final.md`.\n",
        )
        .unwrap();

        let prompt = build_prompt(repo.path(), &prd).unwrap();

        assert!(prompt.contains("Implement the supplied PRD exactly as written"));
        assert!(prompt.contains("# PRD\nSee"));
        assert!(prompt.contains("ADR CONTENT"));
        let command = prompt.find("COMMAND CONTRACT").unwrap();
        let event = prompt.find("EVENT CONTRACT").unwrap();
        assert!(
            command < event,
            "references should have deterministic order"
        );
    }
}
