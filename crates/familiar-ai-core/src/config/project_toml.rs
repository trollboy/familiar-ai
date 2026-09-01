use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::FamiliarError;

use super::{
    validate_identifier, Config, ExecutionContextConfig, ReferenceRootConfig, RepositoryConfig,
    ReviewConfig,
};

/// Closed, shareable repository configuration. This type deliberately has no
/// provider endpoint, credential, absolute path, or machine binding fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FamiliarToml {
    /// Shareable reporting identity. It becomes effective only when the exact
    /// familiar.toml snapshot has passed the existing local approval gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectDeclaration>,
    #[serde(default)]
    pub environments: BTreeMap<String, EnvironmentDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prd_metadata_policy: Option<String>,
    #[serde(default)]
    pub reference_roots: Vec<ReferenceRootConfig>,
    #[serde(default)]
    pub risk_vocabulary: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_context: Option<ExecutionContextConfig>,
    #[serde(default)]
    pub verification: Vec<SharedVerification>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectDeclaration {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_boundary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDeclaration {
    pub requires: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SharedVerification {
    pub check_id: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub working_directory: String,
}

impl FamiliarToml {
    pub fn parse(content: &str) -> crate::Result<Self> {
        let parsed: Self = toml::from_str(content)
            .map_err(|error| FamiliarError::Config(format!("invalid familiar.toml: {error}")))?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> crate::Result<()> {
        // Inspect all scalar strings, including future-looking argv values,
        // before any approved snapshot can become effective.
        let encoded = toml::Value::try_from(self)
            .map_err(|error| FamiliarError::Config(error.to_string()))?;
        validate_shareable_value(&encoded, "familiar.toml")?;
        if let Some(project) = &self.project {
            if !valid_project_id(&project.id) || project.name.trim().is_empty() {
                return Err(FamiliarError::Config(
                    "project declaration requires a prj_ id and non-empty name".into(),
                ));
            }
            if project
                .forked_from
                .as_deref()
                .is_some_and(|id| !valid_project_id(id))
                || (project.forked_from.is_some() != project.fork_boundary.is_some())
            {
                return Err(FamiliarError::Config(
                    "project fork requires both a valid parent id and UTC boundary".into(),
                ));
            }
            if project
                .fork_boundary
                .as_deref()
                .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_err())
            {
                return Err(FamiliarError::Config(
                    "invalid project fork boundary".into(),
                ));
            }
        }
        let mut candidate = Config::default();
        candidate.repositories.insert(
            "/".into(),
            self.repository_config(&RepositoryConfig::default()),
        );
        candidate.validate_repositories()?;
        for (role, declaration) in &self.environments {
            validate_identifier(role, "environment role").map_err(FamiliarError::Config)?;
            validate_identifier(&declaration.requires, "environment requirement")
                .map_err(FamiliarError::Config)?;
            validate_identifier(&declaration.name, "environment name")
                .map_err(FamiliarError::Config)?;
            if declaration.name.parse::<std::net::IpAddr>().is_ok()
                || declaration.name.eq_ignore_ascii_case("localhost")
            {
                return Err(FamiliarError::Config(format!(
                    "environment '{}' uses a host literal; bind hosts in machine config",
                    declaration.name
                )));
            }
        }
        for check in &self.verification {
            if check.check_id.is_empty()
                || check.argv.is_empty()
                || check.argv.iter().any(String::is_empty)
            {
                return Err(FamiliarError::Config(
                    "familiar.toml verification requires non-empty check_id and argv".into(),
                ));
            }
            if !check.working_directory.is_empty() {
                crate::RepositoryPath::new(check.working_directory.clone())
                    .map_err(|_| FamiliarError::Config("familiar.toml verification working_directory must be repository-relative and traversal-free".into()))?;
            }
        }
        Ok(())
    }

    pub fn repository_config(&self, machine: &RepositoryConfig) -> RepositoryConfig {
        let mut value = machine.clone();
        if let Some(v) = &self.profile {
            value.profile = v.clone();
        }
        if let Some(v) = &self.active_dir {
            value.active_dir = v.clone();
        }
        if let Some(v) = &self.archived_dir {
            value.archived_dir = v.clone();
        }
        if let Some(v) = &self.prd_metadata_policy {
            value.prd_metadata_policy = v.clone();
        }
        if !self.reference_roots.is_empty() {
            value.reference_roots = self.reference_roots.clone();
        }
        if !self.risk_vocabulary.is_empty() {
            value.risk_vocabulary = self.risk_vocabulary.clone();
        }
        if self.review.is_some() {
            value.review = self.review.clone();
        }
        if self.execution_context.is_some() {
            value.execution_context = self.execution_context.clone();
        }
        value
    }
}

fn valid_project_id(value: &str) -> bool {
    value.strip_prefix("prj_").is_some_and(|suffix| {
        suffix.len() >= 16
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn validate_shareable_value(value: &toml::Value, path: &str) -> crate::Result<()> {
    match value {
        toml::Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            let credential = lower.starts_with("sk-")
                || lower.contains("token=")
                || lower.contains("api_key=")
                || lower.contains("apikey=")
                || lower.contains("secret=")
                || lower.contains("bearer ")
                || lower.contains("private key")
                || lower.contains("password=");
            if credential {
                return Err(FamiliarError::Config(format!(
                    "{path} contains a credential-like value"
                )));
            }
            if Path::new(text).is_absolute() || text.starts_with('~') {
                return Err(FamiliarError::Config(format!(
                    "{path} contains an absolute or home-relative path '{text}'"
                )));
            }
        }
        toml::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_shareable_value(value, &format!("{path}[{index}]"))?;
            }
        }
        toml::Value::Table(values) => {
            for (key, value) in values {
                validate_shareable_value(value, &format!("{path}.{key}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}
