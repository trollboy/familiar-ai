use crate::{BacklogStatus, DiscoveredPrd, PrdId, RepositoryIdentity, RepositoryPath};
use ring::digest::{digest, SHA256};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

pub const BOOTSTRAP_MANIFEST_PATH: &str = ".familiar/backlog-bootstrap.toml";
pub const BOOTSTRAP_ACTOR: &str = "system:historical-backlog-bootstrap";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapItem {
    pub path: RepositoryPath,
    pub prd_number: u64,
    pub declared_content_hash: String,
    pub observed_content_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapManifest {
    pub version: u32,
    pub canonical_hash: String,
    pub raw_hash: String,
    pub items: Vec<BootstrapItem>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapApplied {
    pub run_id: String,
    pub item_count: usize,
    pub canonical_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapApplyResult {
    Absent,
    AlreadyApplied,
    Applied(BootstrapApplied),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapRollbackResult {
    pub rollback_run_id: String,
    pub item_count: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapStatusReport {
    pub state: String,
    pub repository_key: String,
    pub run_id: Option<String>,
    pub canonical_hash: Option<String>,
    pub item_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("manifest_invalid: {0}")]
    ManifestInvalid(String),
    #[error("manifest_stale: {0}")]
    ManifestStale(String),
    #[error("bootstrap_ineligible: {0}")]
    Ineligible(String),
    #[error("bootstrap_conflict: {0}")]
    Conflict(String),
    #[error("audit_corrupt: {0}")]
    AuditCorrupt(String),
    #[error("rollback_ineligible: {0}")]
    RollbackIneligible(String),
    #[error("bootstrap storage failed: {0}")]
    Storage(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    version: u32,
    completed: Vec<RawItem>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    path: String,
    sha256: String,
}

pub fn load_manifest(
    repository: &RepositoryIdentity,
    discovered: &[DiscoveredPrd],
) -> Result<Option<BootstrapManifest>, BootstrapError> {
    let path = repository.worktree.join(BOOTSTRAP_MANIFEST_PATH);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(BootstrapError::ManifestInvalid(format!(
                "cannot inspect manifest: {e}"
            )))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BootstrapError::ManifestInvalid(
            "manifest must be a regular non-symlink file".into(),
        ));
    }
    let bytes = fs::read(&path)
        .map_err(|e| BootstrapError::ManifestInvalid(format!("cannot read manifest: {e}")))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| BootstrapError::ManifestInvalid("manifest is not valid UTF-8".into()))?;
    let raw: RawManifest =
        toml::from_str(text).map_err(|e| BootstrapError::ManifestInvalid(e.to_string()))?;
    if raw.version != 1 {
        return Err(BootstrapError::ManifestInvalid(format!(
            "unsupported version {}",
            raw.version
        )));
    }
    if raw.completed.is_empty() {
        return Err(BootstrapError::ManifestInvalid(
            "completed must not be empty".into(),
        ));
    }
    let by_path: BTreeMap<&str, &DiscoveredPrd> =
        discovered.iter().map(|p| (p.path.as_str(), p)).collect();
    let mut seen_paths = BTreeSet::new();
    let mut seen_ids = BTreeSet::new();
    let mut items = Vec::new();
    let mut errors = Vec::new();
    for item in raw.completed {
        let digits = item
            .path
            .strip_prefix("docs/prds/PRD-")
            .and_then(|p| p.strip_suffix(".md"));
        let valid_path = digits
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            && !item.path.contains('\\');
        if !valid_path {
            errors.push(format!(
                "{}: invalid or noncanonical active PRD path",
                item.path
            ));
            continue;
        }
        if !seen_paths.insert(item.path.clone()) {
            errors.push(format!("{}: duplicate path", item.path));
            continue;
        }
        if item.sha256.len() != 64
            || !item
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            errors.push(format!(
                "{}: sha256 must be 64 lowercase hexadecimal characters",
                item.path
            ));
            continue;
        }
        let Some(prd) = by_path.get(item.path.as_str()) else {
            errors.push(format!(
                "{}: path is not an active discovered PRD",
                item.path
            ));
            continue;
        };
        if !seen_ids.insert(prd.id.clone()) {
            errors.push(format!(
                "{}: duplicate normalized PRD identity {}",
                item.path, prd.id
            ));
            continue;
        }
        if prd.content_hash != item.sha256 {
            errors.push(format!("{}: content hash mismatch", item.path));
            continue;
        }
        items.push(BootstrapItem {
            path: prd.path.clone(),
            prd_number: prd.number,
            declared_content_hash: item.sha256,
            observed_content_hash: prd.content_hash.clone(),
        });
    }
    if !errors.is_empty() {
        errors.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        return Err(
            if errors.iter().any(|e| e.ends_with("content hash mismatch")) {
                BootstrapError::ManifestStale(errors.join("; "))
            } else {
                BootstrapError::ManifestInvalid(errors.join("; "))
            },
        );
    }
    items.sort_by(|a, b| {
        (a.prd_number, a.path.as_str().as_bytes()).cmp(&(b.prd_number, b.path.as_str().as_bytes()))
    });
    let mut canonical = Vec::new();
    canonical.extend_from_slice(&raw.version.to_be_bytes());
    for item in &items {
        let path = item.path.as_str().as_bytes();
        canonical.extend_from_slice(&(path.len() as u32).to_be_bytes());
        canonical.extend_from_slice(path);
        canonical.extend_from_slice(&decode_hash(&item.declared_content_hash));
    }
    Ok(Some(BootstrapManifest {
        version: raw.version,
        canonical_hash: sha256(&canonical),
        raw_hash: sha256(&bytes),
        items,
    }))
}

pub fn validate_dependency_closure(
    manifest: &BootstrapManifest,
    discovered: &[DiscoveredPrd],
    statuses: &BTreeMap<PrdId, BacklogStatus>,
    completed_with_non_bootstrap_evidence: &BTreeSet<PrdId>,
) -> Result<(), BootstrapError> {
    let listed: BTreeSet<_> = manifest
        .items
        .iter()
        .map(|i| PrdId::new(i.prd_number))
        .collect();
    let by_id: BTreeMap<_, _> = discovered.iter().map(|p| (p.id.clone(), p)).collect();
    let mut errors = Vec::new();
    for item in &manifest.items {
        for dep in &by_id[&PrdId::new(item.prd_number)].dependencies {
            if !listed.contains(dep)
                && !(statuses.get(dep) == Some(&BacklogStatus::Completed)
                    && completed_with_non_bootstrap_evidence.contains(dep))
            {
                errors.push(format!(
                    "{} requires incomplete unlisted dependency {}",
                    item.path, dep
                ));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(BootstrapError::Ineligible(errors.join("; ")))
    }
}

fn decode_hash(value: &str) -> [u8; 32] {
    let mut out = [0; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).expect("validated hash")
    }
    out
}
fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, RepositoryIdentity, Vec<DiscoveredPrd>) {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".familiar")).unwrap();
        let hash = sha256(b"prd bytes\n");
        let prd = DiscoveredPrd {
            id: PrdId::new(1),
            number: 1,
            path: RepositoryPath::new("docs/prds/PRD-001.md").unwrap(),
            location: crate::PrdLocation::Active,
            title: "One".into(),
            dependencies: vec![],
            content_hash: hash.clone(),
        };
        let repo = RepositoryIdentity {
            worktree: dir.path().into(),
            key: "repo".into(),
        };
        (dir, repo, vec![prd])
    }

    #[test]
    fn strict_manifest_parses_and_hashes_format_independently() {
        let (dir, repo, discovered) = fixture();
        let hash = &discovered[0].content_hash;
        let path = dir.path().join(BOOTSTRAP_MANIFEST_PATH);
        fs::write(
            &path,
            format!("version=1\n[[completed]]\npath='docs/prds/PRD-001.md'\nsha256='{hash}'\n"),
        )
        .unwrap();
        let first = load_manifest(&repo, &discovered).unwrap().unwrap();
        fs::write(&path, format!("# comment\nversion = 1\n\n[[completed]]\nsha256 = \"{hash}\"\npath = \"docs/prds/PRD-001.md\"\n")).unwrap();
        let second = load_manifest(&repo, &discovered).unwrap().unwrap();
        assert_eq!(first.canonical_hash, second.canonical_hash);
        assert_ne!(first.raw_hash, second.raw_hash);
    }

    #[test]
    fn rejects_unknown_keys_stale_hashes_and_symlinks() {
        let (dir, repo, discovered) = fixture();
        let path = dir.path().join(BOOTSTRAP_MANIFEST_PATH);
        fs::write(&path, "version=1\nextra=true\ncompleted=[]\n").unwrap();
        assert!(matches!(
            load_manifest(&repo, &discovered),
            Err(BootstrapError::ManifestInvalid(_))
        ));
        fs::write(&path, "version=1\n[[completed]]\npath='docs/prds/PRD-001.md'\nsha256='0000000000000000000000000000000000000000000000000000000000000000'\n").unwrap();
        assert!(matches!(
            load_manifest(&repo, &discovered),
            Err(BootstrapError::ManifestStale(_))
        ));
        #[cfg(unix)]
        {
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink("elsewhere", &path).unwrap();
            assert!(matches!(
                load_manifest(&repo, &discovered),
                Err(BootstrapError::ManifestInvalid(_))
            ));
        }
    }
}
