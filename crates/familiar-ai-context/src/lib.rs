//! Deterministic compilation of repository document context for execution.

mod budget;

pub use budget::{
    BudgetedExecutionContext, ContextBudget, ContextBudgetDecision, ContextBudgetError,
    ContextBudgetOutcome, ContextBudgetReport, ContextBudgeter, ContextExclusionReason,
    ContextInclusionReason, ContextPriority,
};

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use thiserror::Error;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextReferenceKind {
    Prd,
    Adr,
    Contract,
    Supporting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReferenceRoot {
    pub prefix: String,
    pub kind: ContextReferenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextProfile {
    pub active_dir: String,
    pub reference_roots: Vec<ContextReferenceRoot>,
}

impl Default for ContextProfile {
    fn default() -> Self {
        Self {
            active_dir: "docs/prds".into(),
            reference_roots: [
                ("docs/adr/", ContextReferenceKind::Adr),
                ("docs/contracts/", ContextReferenceKind::Contract),
                ("docs/supporting/", ContextReferenceKind::Supporting),
            ]
            .into_iter()
            .map(|(prefix, kind)| ContextReferenceRoot {
                prefix: prefix.into(),
                kind,
            })
            .collect(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ContextCompiler;

#[derive(Debug, Clone, Copy)]
pub struct ContextRequest<'a> {
    pub repository: &'a Path,
    pub prd: &'a Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionContext {
    pub repository: RepositoryContext,
    pub prd: ContextDocument,
    pub documents: Vec<ContextDocument>,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryContext {
    pub repository: PathBuf,
    pub worktree: PathBuf,
    pub git_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDocument {
    pub path: String,
    pub kind: DocumentKind,
    pub content: String,
    pub inclusion: InclusionReason,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Prd,
    Adr,
    Contract,
    Supporting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InclusionReason {
    RequestedPrd,
    DirectReference { referenced_by: String },
}

#[derive(Debug, Error)]
pub enum ContextCompilationError {
    #[error("repository/worktree input is unavailable at {path}: {source}")]
    RepositoryInput { path: PathBuf, source: io::Error },
    #[error("Git operation `{operation}` failed in {path}: {detail}")]
    Git {
        operation: &'static str,
        path: PathBuf,
        detail: String,
    },
    #[error("invalid PRD path {path}: {detail}")]
    InvalidPrd { path: PathBuf, detail: String },
    #[error("cannot read PRD {path}: {source}")]
    ReadPrd { path: PathBuf, source: io::Error },
    #[error("invalid directly referenced document {path}: {detail}")]
    InvalidReference { path: String, detail: String },
    #[error("cannot read directly referenced document {path}: {source}")]
    ReadReference { path: PathBuf, source: io::Error },
    #[error("token estimate overflow for {path}")]
    TokenEstimateOverflow { path: String },
    #[error("aggregate token estimate overflow")]
    AggregateTokenOverflow,
}

impl ContextCompiler {
    pub fn new() -> Self {
        Self
    }

    pub fn compile(
        &self,
        request: ContextRequest<'_>,
    ) -> Result<ExecutionContext, ContextCompilationError> {
        self.compile_profiled(request, &ContextProfile::default())
    }

    pub fn compile_profiled(
        &self,
        request: ContextRequest<'_>,
        profile: &ContextProfile,
    ) -> Result<ExecutionContext, ContextCompilationError> {
        let input = request.repository.canonicalize().map_err(|source| {
            ContextCompilationError::RepositoryInput {
                path: request.repository.to_owned(),
                source,
            }
        })?;
        let worktree = required_git(
            &input,
            &["rev-parse", "--show-toplevel"],
            "rev-parse --show-toplevel",
        )?;
        let worktree = PathBuf::from(worktree).canonicalize().map_err(|source| {
            ContextCompilationError::RepositoryInput {
                path: input.clone(),
                source,
            }
        })?;
        let common = required_git(
            &worktree,
            &["rev-parse", "--git-common-dir"],
            "rev-parse --git-common-dir",
        )?;
        let common = PathBuf::from(common);
        let repository = if common.is_absolute() {
            common
        } else {
            worktree.join(common)
        }
        .canonicalize()
        .map_err(|source| ContextCompilationError::RepositoryInput {
            path: worktree.clone(),
            source,
        })?;
        let git_commit = optional_git(&worktree, &["rev-parse", "--verify", "HEAD"]);

        let prd_path = validate_prd(&worktree, &input, request.prd, &profile.active_dir)?;
        let prd_identity = identity(&worktree, &prd_path).map_err(|detail| {
            ContextCompilationError::InvalidPrd {
                path: prd_path.clone(),
                detail,
            }
        })?;
        let prd_content =
            fs::read_to_string(&prd_path).map_err(|source| ContextCompilationError::ReadPrd {
                path: prd_path.clone(),
                source,
            })?;
        let prd = document(
            prd_identity.clone(),
            DocumentKind::Prd,
            prd_content,
            InclusionReason::RequestedPrd,
        )?;

        let candidates = discover_profiled(&prd.content, &profile.reference_roots);
        let mut validated = BTreeMap::new();
        for (relative, (root, kind)) in candidates {
            let path = validate_reference(&worktree, &relative, &root)?;
            let canonical_identity = identity(&worktree, &path).map_err(|detail| {
                ContextCompilationError::InvalidReference {
                    path: relative.clone(),
                    detail,
                }
            })?;
            validated.entry(canonical_identity).or_insert((path, kind));
        }
        let mut documents = Vec::with_capacity(validated.len());
        for (canonical_identity, (path, kind)) in validated {
            let content = fs::read_to_string(&path).map_err(|source| {
                ContextCompilationError::ReadReference {
                    path: path.clone(),
                    source,
                }
            })?;
            documents.push(document(
                canonical_identity,
                kind,
                content,
                InclusionReason::DirectReference {
                    referenced_by: prd_identity.clone(),
                },
            )?);
        }
        let estimated_tokens = documents
            .iter()
            .try_fold(prd.estimated_tokens, |total, item| {
                total
                    .checked_add(item.estimated_tokens)
                    .ok_or(ContextCompilationError::AggregateTokenOverflow)
            })?;

        Ok(ExecutionContext {
            repository: RepositoryContext {
                repository,
                worktree,
                git_commit,
            },
            prd,
            documents,
            estimated_tokens,
        })
    }
}

fn document(
    path: String,
    kind: DocumentKind,
    content: String,
    inclusion: InclusionReason,
) -> Result<ContextDocument, ContextCompilationError> {
    let estimated_tokens = u64::try_from(familiar_ai_tokens::estimate_tokens(&content))
        .map_err(|_| ContextCompilationError::TokenEstimateOverflow { path: path.clone() })?;
    Ok(ContextDocument {
        path,
        kind,
        content,
        inclusion,
        estimated_tokens,
    })
}

fn required_git(
    cwd: &Path,
    args: &[&str],
    operation: &'static str,
) -> Result<String, ContextCompilationError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| ContextCompilationError::Git {
            operation,
            path: cwd.to_owned(),
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(ContextCompilationError::Git {
            operation,
            path: cwd.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let value = String::from_utf8(output.stdout).map_err(|error| ContextCompilationError::Git {
        operation,
        path: cwd.to_owned(),
        detail: error.to_string(),
    })?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Err(ContextCompilationError::Git {
            operation,
            path: cwd.to_owned(),
            detail: "empty output".into(),
        });
    }
    Ok(value)
}

fn optional_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn validate_prd(
    worktree: &Path,
    input: &Path,
    supplied: &Path,
    active_dir: &str,
) -> Result<PathBuf, ContextCompilationError> {
    if supplied.as_os_str().is_empty() {
        return Err(ContextCompilationError::InvalidPrd {
            path: supplied.into(),
            detail: "PRD path cannot be empty".into(),
        });
    }
    let candidate = if supplied.is_absolute() {
        supplied.to_owned()
    } else {
        input.join(supplied)
    };
    let path = candidate
        .canonicalize()
        .map_err(|error| ContextCompilationError::InvalidPrd {
            path: candidate.clone(),
            detail: format!("cannot resolve path: {error}"),
        })?;
    let prd_root = worktree.join(active_dir).canonicalize().map_err(|error| {
        ContextCompilationError::InvalidPrd {
            path: worktree.join(active_dir),
            detail: format!("cannot resolve repository PRD directory: {error}"),
        }
    })?;
    if !prd_root.starts_with(worktree) {
        return Err(ContextCompilationError::InvalidPrd {
            path: prd_root,
            detail: "repository PRD directory escapes the worktree".into(),
        });
    }
    if !path.starts_with(&prd_root) {
        return Err(ContextCompilationError::InvalidPrd {
            path,
            detail: format!("path must be contained in {}", prd_root.display()),
        });
    }
    if !path.is_file() {
        return Err(ContextCompilationError::InvalidPrd {
            path,
            detail: "path is not a regular file".into(),
        });
    }
    if path.extension().and_then(|value| value.to_str()) != Some("md") {
        return Err(ContextCompilationError::InvalidPrd {
            path,
            detail: "path must have a .md extension".into(),
        });
    }
    Ok(path)
}

fn discover_profiled(
    content: &str,
    roots: &[ContextReferenceRoot],
) -> BTreeMap<String, (String, DocumentKind)> {
    let mapped: Vec<_> = roots
        .iter()
        .map(|root| {
            (
                root.prefix.as_str(),
                root.prefix.trim_end_matches('/').to_owned(),
                match root.kind {
                    ContextReferenceKind::Prd => DocumentKind::Prd,
                    ContextReferenceKind::Adr => DocumentKind::Adr,
                    ContextReferenceKind::Contract => DocumentKind::Contract,
                    ContextReferenceKind::Supporting => DocumentKind::Supporting,
                },
            )
        })
        .collect();
    discover_roots(content, &mapped)
}

fn discover_roots(
    content: &str,
    roots: &[(&str, String, DocumentKind)],
) -> BTreeMap<String, (String, DocumentKind)> {
    let mut paths = BTreeMap::new();
    for (prefix, root, kind) in roots {
        let mut rest = content;
        while let Some(index) = rest.find(prefix) {
            let preceding = rest[..index].chars().next_back();
            rest = &rest[index..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')))
                .unwrap_or(rest.len());
            let reference = rest[..end].trim_end_matches('.');
            if preceding.map_or(true, |c| {
                !(c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
            }) && reference.ends_with(".md")
            {
                paths.insert(reference.to_owned(), (root.clone(), *kind));
            }
            rest = &rest[end..];
        }
    }
    paths
}

fn validate_reference(
    worktree: &Path,
    relative: &str,
    root: &str,
) -> Result<PathBuf, ContextCompilationError> {
    let candidate = worktree.join(relative);
    let path =
        candidate
            .canonicalize()
            .map_err(|error| ContextCompilationError::InvalidReference {
                path: relative.into(),
                detail: format!("cannot be resolved: {error}"),
            })?;
    let allowed = worktree.join(root).canonicalize().map_err(|error| {
        ContextCompilationError::InvalidReference {
            path: relative.into(),
            detail: format!("cannot resolve allowed documentation directory: {error}"),
        }
    })?;
    if !allowed.starts_with(worktree) {
        return Err(ContextCompilationError::InvalidReference {
            path: relative.into(),
            detail: "allowed documentation directory escapes the worktree".into(),
        });
    }
    if !path.starts_with(&allowed) || !path.is_file() {
        return Err(ContextCompilationError::InvalidReference {
            path: relative.into(),
            detail: "path escapes its documentation directory or is not a regular file".into(),
        });
    }
    if path.extension().and_then(|value| value.to_str()) != Some("md") {
        return Err(ContextCompilationError::InvalidReference {
            path: relative.into(),
            detail: "path must have a .md extension".into(),
        });
    }
    Ok(path)
}

fn identity(worktree: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(worktree)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repository() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("docs/prds")).unwrap();
        fs::create_dir_all(temp.path().join("docs/adr")).unwrap();
        fs::create_dir_all(temp.path().join("docs/contracts")).unwrap();
        fs::create_dir_all(temp.path().join("docs/supporting")).unwrap();
        assert!(Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(temp.path())
            .status()
            .unwrap()
            .success());
        temp
    }

    #[test]
    fn compiles_equal_structured_context_with_provenance_ordering_and_estimates() {
        let temp = repository();
        fs::write(temp.path().join("docs/adr/z.md"), "adr").unwrap();
        fs::write(temp.path().join("docs/contracts/a.md"), "héllo").unwrap();
        fs::write(temp.path().join("docs/supporting/m.md"), "").unwrap();
        let source = "docs/adr/z.md docs/contracts/a.md docs/adr/z.md docs/supporting/m.md";
        fs::write(temp.path().join("docs/prds/work.md"), source).unwrap();
        let request = ContextRequest {
            repository: temp.path(),
            prd: Path::new("docs/prds/work.md"),
        };
        let first = ContextCompiler.compile(request).unwrap();
        let second = ContextCompiler.compile(request).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.prd.path, "docs/prds/work.md");
        assert_eq!(first.prd.kind, DocumentKind::Prd);
        assert_eq!(first.prd.inclusion, InclusionReason::RequestedPrd);
        assert_eq!(
            first
                .documents
                .iter()
                .map(|item| item.path.as_str())
                .collect::<Vec<_>>(),
            [
                "docs/adr/z.md",
                "docs/contracts/a.md",
                "docs/supporting/m.md"
            ]
        );
        assert_eq!(
            first.documents[1].estimated_tokens,
            familiar_ai_tokens::estimate_tokens("héllo") as u64
        );
        assert_eq!(
            first.estimated_tokens,
            first.prd.estimated_tokens
                + first
                    .documents
                    .iter()
                    .map(|item| item.estimated_tokens)
                    .sum::<u64>()
        );
        assert!(first.documents.iter().all(|item| item.inclusion
            == InclusionReason::DirectReference {
                referenced_by: "docs/prds/work.md".into()
            }));
        assert!(first.repository.repository.ends_with(".git"));
        assert_eq!(
            first.repository.worktree,
            temp.path().canonicalize().unwrap()
        );
        assert_eq!(first.repository.git_commit, None);
    }

    #[test]
    fn profiled_compilation_uses_active_and_declared_reference_roots() {
        let temp = repository();
        fs::create_dir_all(temp.path().join("docs/prd/todo")).unwrap();
        fs::create_dir_all(temp.path().join("docs/runbooks")).unwrap();
        fs::write(temp.path().join("docs/runbooks/ops.md"), "operations").unwrap();
        fs::write(
            temp.path().join("docs/prd/todo/0139a-work.md"),
            "docs/runbooks/ops.md",
        )
        .unwrap();
        let profile = ContextProfile {
            active_dir: "docs/prd/todo".into(),
            reference_roots: vec![ContextReferenceRoot {
                prefix: "docs/runbooks/".into(),
                kind: ContextReferenceKind::Supporting,
            }],
        };
        let compiled = ContextCompiler
            .compile_profiled(
                ContextRequest {
                    repository: temp.path(),
                    prd: Path::new("docs/prd/todo/0139a-work.md"),
                },
                &profile,
            )
            .unwrap();
        assert_eq!(compiled.prd.path, "docs/prd/todo/0139a-work.md");
        assert_eq!(compiled.documents[0].path, "docs/runbooks/ops.md");
    }

    #[test]
    fn ignores_ineligible_and_transitive_references() {
        let temp = repository();
        fs::write(
            temp.path().join("docs/adr/one.md"),
            "docs/contracts/transitive.md",
        )
        .unwrap();
        fs::write(temp.path().join("docs/contracts/transitive.md"), "no").unwrap();
        fs::write(
            temp.path().join("docs/prds/work.md"),
            "docs/adr/one.md docs/other.md file.md /docs/contracts/no.md docs/contracts/no.txt",
        )
        .unwrap();
        let context = ContextCompiler
            .compile(ContextRequest {
                repository: temp.path(),
                prd: Path::new("docs/prds/work.md"),
            })
            .unwrap();
        assert_eq!(context.documents.len(), 1);
        assert_eq!(context.documents[0].path, "docs/adr/one.md");
    }

    #[test]
    fn eligible_missing_reference_is_an_error_with_the_path() {
        let temp = repository();
        fs::write(temp.path().join("docs/prds/work.md"), "docs/adr/missing.md").unwrap();
        let error = ContextCompiler
            .compile(ContextRequest {
                repository: temp.path(),
                prd: Path::new("docs/prds/work.md"),
            })
            .unwrap_err();
        assert!(
            matches!(error, ContextCompilationError::InvalidReference { path, .. } if path == "docs/adr/missing.md")
        );
    }

    #[test]
    fn rejects_out_of_scope_and_non_utf8_prds() {
        let temp = repository();
        fs::write(temp.path().join("outside.md"), "outside").unwrap();
        let outside = ContextCompiler
            .compile(ContextRequest {
                repository: temp.path(),
                prd: Path::new("outside.md"),
            })
            .unwrap_err();
        assert!(matches!(
            outside,
            ContextCompilationError::InvalidPrd { .. }
        ));
        fs::write(temp.path().join("docs/prds/bad.md"), [0xff]).unwrap();
        let bad = ContextCompiler
            .compile(ContextRequest {
                repository: temp.path(),
                prd: Path::new("docs/prds/bad.md"),
            })
            .unwrap_err();
        assert!(matches!(bad, ContextCompilationError::ReadPrd { .. }));
    }

    #[test]
    fn linked_worktree_records_common_repository_and_head() {
        let temp = repository();
        fs::write(temp.path().join("docs/prds/work.md"), "work").unwrap();
        assert!(Command::new("git")
            .args(["-C", temp.path().to_str().unwrap(), "add", "."])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args([
                "-C",
                temp.path().to_str().unwrap(),
                "-c",
                "user.name=Familiar Test",
                "-c",
                "user.email=familiar@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ])
            .status()
            .unwrap()
            .success());
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("linked");
        assert!(Command::new("git")
            .args([
                "-C",
                temp.path().to_str().unwrap(),
                "worktree",
                "add",
                "--quiet",
                linked.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success());

        let context = ContextCompiler
            .compile(ContextRequest {
                repository: &linked,
                prd: Path::new("docs/prds/work.md"),
            })
            .unwrap();
        let head = required_git(&linked, &["rev-parse", "--verify", "HEAD"], "head").unwrap();
        assert_eq!(context.repository.worktree, linked.canonicalize().unwrap());
        assert_eq!(
            context.repository.repository,
            temp.path().join(".git").canonicalize().unwrap()
        );
        assert_eq!(
            context.repository.git_commit.as_deref(),
            Some(head.as_str())
        );
    }
}
