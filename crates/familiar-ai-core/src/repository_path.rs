use std::path::{Component, Path, PathBuf};

/// The single minting site for repository origin identity (PRD-087).
///
/// Before this, three independent call sites shelled out to
/// `git rev-parse --git-common-dir` and canonicalized the result: backlog
/// discovery, per-repository config matching, and accounting evidence. They
/// agreed only by the accident of being copy-pasted identically — nothing
/// stopped one of them drifting (a missing `--path-format=absolute`, a
/// different canonicalization order) and minting a different key for the
/// same repository. Git's common directory is shared by every linked
/// worktree of one repository, so this is what makes identity derived while
/// executing inside a worktree resolve to that worktree's origin repository:
/// every caller now goes through this one function instead of deriving it
/// independently.
pub const INVARIANT_REPOSITORY_IDENTITY: &str = "repository-identity-single-mint";

#[derive(Debug, thiserror::Error)]
pub enum RepositoryOriginError {
    #[error("cannot run git: {0}")]
    Exec(#[source] std::io::Error),
    #[error("git rev-parse --git-common-dir failed: {0}")]
    GitFailed(String),
    #[error("git returned a non-UTF-8 path")]
    NonUtf8,
    #[error("cannot canonicalize git common directory: {0}")]
    Canonicalize(#[source] std::io::Error),
}

/// The canonical Git common-directory identity of the repository containing
/// `path`. Every worktree of one repository (the primary checkout and every
/// `git worktree add` lease) shares the same common directory, so this is
/// the sole definition of repository identity: two paths produce the same
/// key if and only if they belong to the same repository.
pub fn repository_origin_key(path: &Path) -> Result<String, RepositoryOriginError> {
    let output = crate::git_env::git_command(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .output()
    .map_err(RepositoryOriginError::Exec)?;
    if !output.status.success() {
        return Err(RepositoryOriginError::GitFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let value = String::from_utf8(output.stdout).map_err(|_| RepositoryOriginError::NonUtf8)?;
    let canonical = Path::new(value.trim())
        .canonicalize()
        .map_err(RepositoryOriginError::Canonicalize)?;
    canonical
        .to_str()
        .map(|s| s.replace('\\', "/"))
        .ok_or(RepositoryOriginError::NonUtf8)
}

/// Advisory form of [`repository_origin_key`] for callers that treat "not a
/// Git repository" (or git being unavailable) as `None` rather than a hard
/// error — per-repository config matching and accounting evidence, neither
/// of which own the repository's authoritative identity.
pub fn git_common_directory(path: &Path) -> Option<String> {
    repository_origin_key(path).ok()
}

/// The project-scoped, repository-relative identity of one file entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalFileIdentity {
    project_id: i64,
    path: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PathIdentityError {
    #[error("absolute repository-relative path is invalid")]
    AbsoluteInput,
    #[error("parent traversal is invalid")]
    ParentTraversal,
    #[error("file identity is empty")]
    EmptyFilePath,
    #[error("observed path is not within the declared project root")]
    ProjectRootMismatch,
    #[error("path is not lexically contained by the project root")]
    LexicalEscape,
    #[error("path resolves through a symbolic link outside the project root")]
    SymlinkEscape,
    #[error("path cannot be represented losslessly")]
    NonUtf8,
    #[error("filesystem error while validating path: {0}")]
    Filesystem(#[source] std::io::Error),
}

impl CanonicalFileIdentity {
    pub fn from_relative(
        project_id: i64,
        project_root: &Path,
        path: &Path,
    ) -> Result<Self, PathIdentityError> {
        let normalized = normalize_relative(path, false)?;
        verify_physical_containment(project_root, &normalized)?;
        Ok(Self {
            project_id,
            path: normalized,
        })
    }

    pub fn from_observed(
        project_id: i64,
        project_root: &Path,
        observed: &Path,
    ) -> Result<Self, PathIdentityError> {
        if !observed.is_absolute() {
            return Err(PathIdentityError::ProjectRootMismatch);
        }
        let physical_root = project_root
            .canonicalize()
            .map_err(PathIdentityError::Filesystem)?;
        let logical_root = normalize_host_path(project_root)?;
        let observed = normalize_host_path(observed)?;
        let relative = observed
            .strip_prefix(&logical_root)
            .map_err(|_| PathIdentityError::ProjectRootMismatch)?;
        let normalized = normalize_relative(relative, false)?;
        verify_physical_containment(&physical_root, &normalized)?;
        Ok(Self {
            project_id,
            path: normalized,
        })
    }

    /// Validate a value at the persistence boundary. No filesystem access is
    /// needed: writers must already supply the canonical logical identity.
    pub fn validate_stored(project_id: i64, path: &str) -> Result<Self, PathIdentityError> {
        let normalized = normalize_relative(Path::new(path), false)?;
        if normalized != path {
            return Err(PathIdentityError::LexicalEscape);
        }
        Ok(Self {
            project_id,
            path: normalized,
        })
    }

    pub fn module_prefix(path: &Path) -> Result<String, PathIdentityError> {
        let normalized = normalize_relative(path, true)?;
        if normalized.is_empty() {
            Ok(normalized)
        } else {
            Ok(format!("{normalized}/"))
        }
    }

    pub fn project_id(&self) -> i64 {
        self.project_id
    }
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn host_path(&self, project_root: &Path) -> PathBuf {
        project_root.join(Path::new(&self.path))
    }
}

fn normalize_relative(path: &Path, allow_empty: bool) -> Result<String, PathIdentityError> {
    if path.is_absolute() {
        return Err(PathIdentityError::AbsoluteInput);
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => return Err(PathIdentityError::ParentTraversal),
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathIdentityError::AbsoluteInput)
            }
            Component::Normal(value) => {
                parts.push(value.to_str().ok_or(PathIdentityError::NonUtf8)?)
            }
        }
    }
    if parts.is_empty() && !allow_empty {
        return Err(PathIdentityError::EmptyFilePath);
    }
    Ok(parts.join("/"))
}

fn normalize_host_path(path: &Path) -> Result<PathBuf, PathIdentityError> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => return Err(PathIdentityError::ParentTraversal),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => result.push(component.as_os_str()),
            Component::Normal(value) => {
                value.to_str().ok_or(PathIdentityError::NonUtf8)?;
                result.push(value);
            }
        }
    }
    Ok(result)
}

fn verify_physical_containment(root: &Path, relative: &str) -> Result<(), PathIdentityError> {
    let root = root.canonicalize().map_err(PathIdentityError::Filesystem)?;
    let candidate = root.join(relative);
    match candidate.canonicalize() {
        Ok(physical) if physical.starts_with(&root) => Ok(()),
        Ok(_) => Err(PathIdentityError::SymlinkEscape),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PathIdentityError::Filesystem(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn git(dir: &Path, args: &[&str]) {
        assert!(crate::git_env::git_command(dir, args)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn two_worktrees_of_one_repository_mint_the_same_origin_key() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("file"), "base").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);

        let worktree_a = temp.path().join("wt-a");
        let worktree_b = temp.path().join("wt-b");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "wt-a",
                worktree_a.to_str().unwrap(),
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "wt-b",
                worktree_b.to_str().unwrap(),
            ],
        );

        let main_key = repository_origin_key(&repo).unwrap();
        let key_a = repository_origin_key(&worktree_a).unwrap();
        let key_b = repository_origin_key(&worktree_b).unwrap();
        // Naming the invariant in the failure is what makes this a mutation
        // regression rather than an equality check: remove the single minting
        // site and each worktree mints its own path instead, and the failure
        // says which guarantee went with it.
        assert_eq!(
            main_key, key_a,
            "{INVARIANT_REPOSITORY_IDENTITY} violated: worktree a minted its own \
             identity instead of the origin repository's"
        );
        assert_eq!(
            main_key, key_b,
            "{INVARIANT_REPOSITORY_IDENTITY} violated: worktree b minted its own \
             identity instead of the origin repository's"
        );
        assert_ne!(
            key_a,
            worktree_a.to_string_lossy(),
            "{INVARIANT_REPOSITORY_IDENTITY} violated: identity was taken from the \
             worktree path, the shape this invariant exists to prevent"
        );

        // The advisory wrapper is the same single minting site, not a
        // second independent computation.
        assert_eq!(git_common_directory(&worktree_a), Some(key_a));
        assert_eq!(git_common_directory(&worktree_b), Some(key_b));
    }

    #[test]
    fn non_repository_path_is_advisory_none_not_a_panic() {
        let temp = tempdir().unwrap();
        assert!(git_common_directory(temp.path()).is_none());
        assert!(repository_origin_key(temp.path()).is_err());
    }

    #[test]
    fn relative_normalization_is_idempotent() {
        let root = tempdir().unwrap();
        let identity =
            CanonicalFileIdentity::from_relative(7, root.path(), Path::new("./src//main.rs"))
                .unwrap();
        assert_eq!(identity.path(), "src/main.rs");
        assert_eq!(
            CanonicalFileIdentity::from_relative(7, root.path(), Path::new(identity.path()))
                .unwrap(),
            identity
        );
    }

    #[test]
    fn rejects_absolute_parent_and_empty_file_paths() {
        let root = tempdir().unwrap();
        assert!(matches!(
            CanonicalFileIdentity::from_relative(1, root.path(), Path::new("/tmp/a")),
            Err(PathIdentityError::AbsoluteInput)
        ));
        assert!(matches!(
            CanonicalFileIdentity::from_relative(1, root.path(), Path::new("src/../a")),
            Err(PathIdentityError::ParentTraversal)
        ));
        assert!(matches!(
            CanonicalFileIdentity::from_relative(1, root.path(), Path::new(".")),
            Err(PathIdentityError::EmptyFilePath)
        ));
    }

    #[test]
    fn observed_path_uses_logical_relative_identity() {
        let root = tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/main.rs"), "fn main() {}").unwrap();
        let identity = CanonicalFileIdentity::from_observed(
            3,
            root.path(),
            &root.path().join("src/./main.rs"),
        )
        .unwrap();
        assert_eq!(identity.path(), "src/main.rs");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_preserve_logical_identity_and_cannot_escape() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(root.path().join("target"), "ok").unwrap();
        std::fs::write(outside.path().join("secret"), "no").unwrap();
        symlink(root.path().join("target"), root.path().join("inside-link")).unwrap();
        symlink(
            outside.path().join("secret"),
            root.path().join("outside-link"),
        )
        .unwrap();
        assert_eq!(
            CanonicalFileIdentity::from_relative(1, root.path(), Path::new("inside-link"))
                .unwrap()
                .path(),
            "inside-link"
        );
        assert!(matches!(
            CanonicalFileIdentity::from_relative(1, root.path(), Path::new("outside-link")),
            Err(PathIdentityError::SymlinkEscape)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_without_lossy_conversion() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempdir().unwrap();
        let path = Path::new(std::ffi::OsStr::from_bytes(b"bad-\xff"));
        assert!(matches!(
            CanonicalFileIdentity::from_relative(1, root.path(), path),
            Err(PathIdentityError::NonUtf8)
        ));
    }

    #[test]
    fn module_prefix_is_component_bounded() {
        assert_eq!(
            CanonicalFileIdentity::module_prefix(Path::new("./src/")).unwrap(),
            "src/"
        );
        assert_eq!(
            CanonicalFileIdentity::module_prefix(Path::new("")).unwrap(),
            ""
        );
    }
}
