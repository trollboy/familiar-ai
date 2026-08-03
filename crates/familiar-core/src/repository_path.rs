use std::path::{Component, Path, PathBuf};

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
