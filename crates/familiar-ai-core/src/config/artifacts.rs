use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::FamiliarError;

pub const ARTIFACT_MANIFEST_SCHEMA: u32 = 1;

pub const ARTIFACT_DIGEST_ALGORITHM: &str = "sha256-v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifestFile {
    pub path: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub schema_version: u32,
    pub digest_algorithm: String,
    #[serde(default)]
    pub files: Vec<ArtifactManifestFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_artifact: Option<String>,
    #[serde(default)]
    pub adapters: Vec<String>,
    #[serde(default)]
    pub merged: bool,
    /// Canonical JSON supplied by the operator for identity-bearing settings
    /// such as quantization, tokenizer/templates, context, and inference
    /// parameters. Object keys are canonicalized before hashing.
    #[serde(default)]
    pub identity: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifactProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_revision: Option<String>,
    /// A runtime/upstream assertion only; never establishes verified identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplied_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fine_tune: Option<String>,
    #[serde(default)]
    pub adapters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_application: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_configuration: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_parameters: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default)]
    pub usage_restrictions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifactConfig {
    pub id: String,
    #[serde(default)]
    pub state: ArtifactVerificationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<ArtifactManifest>,
    #[serde(default)]
    pub provenance: ModelArtifactProvenance,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactVerificationState {
    Verified,
    #[default]
    DegradedUnverifiedAlias,
}

impl ModelArtifactConfig {
    pub fn routing_eligible(&self, require_verified: bool) -> bool {
        !require_verified || self.state == ArtifactVerificationState::Verified
    }
}

/// Probe and canonically digest an explicit set of identity-bearing files.
/// The set is deliberately operator-authored: caches and incidental files do
/// not become identity merely because they happen to share a directory.
pub fn derive_artifact_manifest(
    root: &Path,
    relative_files: &[PathBuf],
    base_artifact: Option<String>,
    adapters: Vec<String>,
    merged: bool,
    identity: BTreeMap<String, serde_json::Value>,
) -> crate::Result<(String, ArtifactManifest)> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| FamiliarError::Config(format!("cannot resolve artifact root: {error}")))?;
    let mut normalized = BTreeSet::new();
    for relative in relative_files {
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(FamiliarError::Config(format!(
                "artifact path '{}' must be normalized and relative",
                relative.display()
            )));
        }
        let text = relative.to_string_lossy().replace('\\', "/");
        if text.is_empty() || !normalized.insert(text) {
            return Err(FamiliarError::Config(
                "artifact file list is empty or duplicated".into(),
            ));
        }
    }
    if normalized.is_empty() && base_artifact.is_none() {
        return Err(FamiliarError::Config(
            "verified artifact requires identity-bearing files".into(),
        ));
    }
    let mut files = Vec::with_capacity(normalized.len());
    for relative in normalized {
        let joined = canonical_root.join(&relative);
        let resolved = joined.canonicalize().map_err(|error| {
            FamiliarError::Config(format!("cannot read artifact file '{relative}': {error}"))
        })?;
        if !resolved.starts_with(&canonical_root) {
            return Err(FamiliarError::Config(format!(
                "artifact symlink '{relative}' escapes its root"
            )));
        }
        let metadata = resolved.metadata().map_err(|error| {
            FamiliarError::Config(format!(
                "cannot inspect artifact file '{relative}': {error}"
            ))
        })?;
        if !metadata.is_file() {
            return Err(FamiliarError::Config(format!(
                "artifact entry '{relative}' is not a file"
            )));
        }
        let mut input = File::open(&resolved).map_err(|error| {
            FamiliarError::Config(format!("cannot open artifact file '{relative}': {error}"))
        })?;
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer).map_err(|error| {
                FamiliarError::Config(format!("cannot digest artifact file '{relative}': {error}"))
            })?;
            if count == 0 {
                break;
            }
            context.update(&buffer[..count]);
        }
        files.push(ArtifactManifestFile {
            path: relative,
            size: metadata.len(),
            digest: format!("sha256:{}", hex_digest(context.finish().as_ref())),
        });
    }
    let manifest = ArtifactManifest {
        schema_version: ARTIFACT_MANIFEST_SCHEMA,
        digest_algorithm: ARTIFACT_DIGEST_ALGORITHM.into(),
        files,
        base_artifact,
        adapters,
        merged,
        identity,
    };
    let canonical =
        serde_json::to_vec(&manifest).map_err(|error| FamiliarError::Config(error.to_string()))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &canonical);
    Ok((format!("sha256:{}", hex_digest(digest.as_ref())), manifest))
}

pub(super) fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    use std::fs;

    fn fixture(root: &Path) {
        fs::create_dir_all(root.join("tokenizer")).unwrap();
        fs::write(root.join("weights.gguf"), b"weights-v1").unwrap();
        fs::write(root.join("tokenizer/tokenizer.json"), b"tokenizer-v1").unwrap();
        fs::write(root.join("chat.jinja"), b"{{ messages }}").unwrap();
    }

    fn derive(root: &Path, files: Vec<PathBuf>) -> String {
        derive_artifact_manifest(root, &files, None, vec![], false, BTreeMap::new())
            .unwrap()
            .0
    }

    #[test]
    fn copied_artifact_and_reordered_input_have_the_same_identity() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        fixture(first.path());
        fixture(second.path());
        let files = ["weights.gguf", "tokenizer/tokenizer.json", "chat.jinja"];
        let a = derive(first.path(), files.iter().map(PathBuf::from).collect());
        let b = derive(
            second.path(),
            files.iter().rev().map(PathBuf::from).collect(),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn every_identity_bearing_change_changes_identity() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        let files = vec![
            PathBuf::from("weights.gguf"),
            PathBuf::from("tokenizer/tokenizer.json"),
            PathBuf::from("chat.jinja"),
        ];
        let original = derive(root.path(), files.clone());
        for (path, bytes) in [
            ("weights.gguf", b"weights-v2".as_slice()),
            ("tokenizer/tokenizer.json", b"tokenizer-v2"),
            ("chat.jinja", b"changed"),
        ] {
            fixture(root.path());
            fs::write(root.path().join(path), bytes).unwrap();
            assert_ne!(original, derive(root.path(), files.clone()), "{path}");
        }
        let mut identity = BTreeMap::new();
        identity.insert("quantization".into(), serde_json::json!({"bits":4}));
        let quantized =
            derive_artifact_manifest(root.path(), &files, None, vec![], false, identity)
                .unwrap()
                .0;
        assert_ne!(original, quantized);
    }

    #[test]
    fn dynamic_adapter_order_and_merged_state_are_distinct() {
        let base = format!("sha256:{}", "a".repeat(64));
        let id = |adapters, merged| {
            derive_artifact_manifest(
                Path::new("."),
                &[],
                Some(base.clone()),
                adapters,
                merged,
                BTreeMap::new(),
            )
            .unwrap()
            .0
        };
        assert_ne!(
            id(vec!["a".into(), "b".into()], false),
            id(vec!["b".into(), "a".into()], false)
        );
        assert_ne!(id(vec!["a".into()], false), id(vec!["a".into()], true));
    }

    #[test]
    fn refuses_absolute_traversal_missing_and_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        for bad in [
            PathBuf::from("/weights.gguf"),
            PathBuf::from("../weights.gguf"),
            PathBuf::from("missing.gguf"),
        ] {
            assert!(derive_artifact_manifest(
                root.path(),
                &[bad],
                None,
                vec![],
                false,
                BTreeMap::new()
            )
            .is_err());
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hosts", root.path().join("escape")).unwrap();
            assert!(derive_artifact_manifest(
                root.path(),
                &[PathBuf::from("escape")],
                None,
                vec![],
                false,
                BTreeMap::new()
            )
            .is_err());
        }
    }
}
