use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{DeliveryConfig, ExecutionContextConfig, ReviewConfig};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    #[serde(default = "default_profile_name")]
    pub profile: String,
    #[serde(default = "default_active_dir")]
    pub active_dir: String,
    #[serde(default = "default_archived_dir")]
    pub archived_dir: String,
    /// `incremental` accepts legacy documents with exact migration diagnostics;
    /// `strict` requires the structured front-matter contract.
    #[serde(default = "default_prd_metadata_policy")]
    pub prd_metadata_policy: String,
    #[serde(default)]
    pub reference_roots: Vec<ReferenceRootConfig>,
    /// Closed vocabulary of permitted `risk_classes` values. Structured PRD
    /// parsing and `metadata-check` reject any declared risk class outside
    /// it; an unconfigured or empty vocabulary rejects every structured PRD
    /// that declares risk classes.
    #[serde(default)]
    pub risk_vocabulary: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_context: Option<ExecutionContextConfig>,
    /// Repository-owned delivery authority. Absence is fail-closed at the
    /// publication boundary; the global legacy delivery section grants none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryConfig>,
    /// Machine-local environment-name to provider-name bindings. Never read
    /// from familiar.toml.
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRootConfig {
    pub prefix: String,
    pub kind: ReferenceKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReferenceKind {
    Prd,
    Adr,
    Contract,
    Supporting,
}

fn default_profile_name() -> String {
    "canonical".into()
}

fn default_active_dir() -> String {
    "docs/prds".into()
}

fn default_archived_dir() -> String {
    "docs/prds/done".into()
}

fn default_prd_metadata_policy() -> String {
    "incremental".into()
}

impl Default for RepositoryConfig {
    fn default() -> Self {
        Self {
            profile: default_profile_name(),
            active_dir: default_active_dir(),
            archived_dir: default_archived_dir(),
            prd_metadata_policy: default_prd_metadata_policy(),
            reference_roots: Vec::new(),
            risk_vocabulary: Vec::new(),
            review: None,
            execution_context: None,
            delivery: None,
            bindings: BTreeMap::new(),
        }
    }
}

impl RepositoryConfig {
    pub fn delivery_policy(&self) -> Result<&DeliveryConfig, String> {
        self.delivery.as_ref().ok_or_else(|| {
            "repository delivery policy is missing; merge and deploy are not authorized".into()
        })
    }
    pub fn layout(&self) -> crate::BacklogLayout {
        crate::BacklogLayout {
            profile: crate::BacklogProfile::parse(&self.profile).expect("validated profile"),
            active_dir: crate::RepositoryPath::new(self.active_dir.clone())
                .expect("validated active_dir"),
            archived_dir: crate::RepositoryPath::new(self.archived_dir.clone())
                .expect("validated archived_dir"),
            metadata_policy: crate::PrdMetadataPolicy::parse(&self.prd_metadata_policy)
                .expect("validated prd_metadata_policy"),
            risk_vocabulary: self.risk_vocabulary.clone(),
        }
    }
    pub fn resolved_reference_roots(&self) -> Vec<ReferenceRootConfig> {
        if self.reference_roots.is_empty() {
            default_reference_roots()
        } else {
            self.reference_roots.clone()
        }
    }
}

fn default_reference_roots() -> Vec<ReferenceRootConfig> {
    [
        ("docs/adr/", ReferenceKind::Adr),
        ("docs/contracts/", ReferenceKind::Contract),
        ("docs/supporting/", ReferenceKind::Supporting),
    ]
    .into_iter()
    .map(|(prefix, kind)| ReferenceRootConfig {
        prefix: prefix.into(),
        kind,
    })
    .collect()
}
