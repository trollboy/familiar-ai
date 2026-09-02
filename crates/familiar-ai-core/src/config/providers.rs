use serde::{Deserialize, Serialize};

use super::{CapabilityProfileConfig, CapabilityProvenanceConfig, RuntimeCapabilityConfig};

/// PRD-059's `runtime` identity for a raw hosted Anthropic Messages API
/// worker: `[worker_registry.workers.<id>] runtime = "anthropic-api"`. No
/// `runtime_config` extension is required for this runtime — unlike
/// `ollama`, an `anthropic-api` worker's only settings beyond the generic
/// spec fields (`provider`, `model`, `auth_profile`, `capability_profile`)
/// live in the adapter's own construction config, not in operator TOML.
pub const ANTHROPIC_API_RUNTIME: &str = "anthropic-api";

/// Default declared capability profile for an `anthropic-api` worker,
/// reflecting what the Messages API declares support for out of the box.
/// Provenance starts `Declared`; probed/observed facts layer on top as the
/// worker actually runs (PRD-047 discipline) — nothing here is inferred
/// from the provider name, and an operator may still author a narrower
/// profile explicitly.
pub fn anthropic_api_default_capability_profile() -> CapabilityProfileConfig {
    use CapabilityProvenanceConfig::Declared;
    use RuntimeCapabilityConfig::*;
    CapabilityProfileConfig {
        capabilities: [
            (NativeToolCalling, Declared),
            (McpClient, Declared),
            (StructuredOutput, Declared),
            (Streaming, Declared),
            (PromptCaching, Declared),
            (ReasoningControls, Declared),
            (ParallelToolCalls, Declared),
            (UsageReportingCategories, Declared),
            (CostReportingMode, Declared),
            (RemoteOrLocal, Declared),
            (MaxContext, Declared),
        ]
        .into_iter()
        .collect(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: EndpointProviderKind,
    /// Billing implementation. Only `anthropic-organization` has a collector;
    /// external modes are typed so they fail closed instead of being mistaken
    /// for local or authoritative coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_mode: Option<BillingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_name: Option<String>,
    /// Optional explicit Platform attribution boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<InferenceRuntimeKind>,
    #[serde(default)]
    pub host: String,
    /// Deploy-target executable. Absent selects the legacy SSH transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub auth: AuthDescriptor,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    /// Deploy-target-only capability discovery. Values are diagnostics, not
    /// credentials, and are replaced on every explicit probe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Deploy-target-only, deliberately finite remote recipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<DeployRecipeConfig>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EndpointProviderKind {
    Inference,
    DeployTarget,
    Billing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BillingMode {
    AnthropicOrganization,
    OpenAiOrganization,
    Bedrock,
    Vertex,
    Foundry,
    ExternalBilling,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InferenceRuntimeKind {
    Unsloth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeployRecipeConfig {
    pub sync_argv: Vec<String>,
    pub restart_argv: Vec<String>,
    pub smoke_argv: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub enum AuthDescriptor {
    None,
    CliLogin(String),
    Env(String),
    CredentialStore(CredentialStoreDescriptor),
    SshAgent,
}

/// A durable reference to a credential managed outside Familiar. The fields
/// are deliberately restricted to stable identifiers so the descriptor has
/// one unambiguous, diagnostic-safe representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialStoreDescriptor {
    pub store: String,
    pub service: String,
    pub account: String,
}

impl std::fmt::Display for CredentialStoreDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "credential-store: {}/{}/{}",
            self.store, self.service, self.account
        )
    }
}

impl TryFrom<String> for AuthDescriptor {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value == "none" {
            Ok(Self::None)
        } else if value == "ssh-agent" {
            Ok(Self::SshAgent)
        } else if let Some(command) = value.strip_prefix("cli-login: ") {
            validate_identifier(command, "auth descriptor")?;
            Ok(Self::CliLogin(command.to_owned()))
        } else if let Some(name) = value.strip_prefix("env: ") {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                Err(format!("invalid auth descriptor '{value}'"))
            } else {
                Ok(Self::Env(name.to_owned()))
            }
        } else if let Some(reference) = value.strip_prefix("credential-store: ") {
            let fields = reference.split('/').collect::<Vec<_>>();
            if fields.len() != 3 {
                return Err(format!("invalid auth descriptor '{value}'"));
            }
            validate_identifier(fields[0], "credential store")?;
            validate_identifier(fields[1], "credential store service")?;
            validate_identifier(fields[2], "credential store account")?;
            Ok(Self::CredentialStore(CredentialStoreDescriptor {
                store: fields[0].to_owned(),
                service: fields[1].to_owned(),
                account: fields[2].to_owned(),
            }))
        } else {
            Err(format!("invalid auth descriptor '{value}'"))
        }
    }
}

impl From<AuthDescriptor> for String {
    fn from(value: AuthDescriptor) -> Self {
        match value {
            AuthDescriptor::None => "none".into(),
            AuthDescriptor::CliLogin(command) => format!("cli-login: {command}"),
            AuthDescriptor::Env(name) => format!("env: {name}"),
            AuthDescriptor::CredentialStore(reference) => reference.to_string(),
            AuthDescriptor::SshAgent => "ssh-agent".into(),
        }
    }
}

pub(super) fn validate_identifier(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Err(format!("invalid {field} '{value}'"))
    } else {
        Ok(())
    }
}

impl ProviderConfig {
    pub fn validate(&self, name: &str) -> Result<(), String> {
        validate_identifier(name, "provider name")?;
        match self.kind {
            EndpointProviderKind::Inference => validate_host(&self.host)?,
            EndpointProviderKind::DeployTarget if self.via.is_none() => {
                validate_ssh_host(&self.host)?
            }
            EndpointProviderKind::DeployTarget => {
                if !self.host.is_empty() {
                    return Err("CLI deploy-target cannot declare a host".into());
                }
            }
            EndpointProviderKind::Billing => validate_host(&self.host)?,
        }
        for model in &self.models {
            validate_model_identifier(model)?;
        }
        match self.kind {
            EndpointProviderKind::Inference => {
                if self.recipe.is_some()
                    || self.via.is_some()
                    || !self.capabilities.is_empty()
                    || self.billing_mode.is_some()
                    || self.organization_id.is_some()
                    || self.organization_name.is_some()
                {
                    return Err("inference provider has non-inference extension fields".into());
                }
            }
            EndpointProviderKind::DeployTarget => {
                if self.runtime.is_some() {
                    return Err("deploy-target cannot declare an inference runtime".into());
                }
                if self.billing_mode.is_some()
                    || self.organization_id.is_some()
                    || self.organization_name.is_some()
                {
                    return Err("deploy-target has billing extension fields".into());
                }
                if self.via.is_none() && self.auth != AuthDescriptor::SshAgent {
                    return Err("SSH deploy-target auth must be ssh-agent".into());
                }
                if let Some(via) = &self.via {
                    validate_cloud_cli(via)?;
                    if self.auth != AuthDescriptor::CliLogin(via.clone()) {
                        return Err(format!("CLI deploy-target auth must be 'cli-login: {via}'"));
                    }
                }
                if !self.models.is_empty() {
                    return Err("deploy-target provider cannot declare models".into());
                }
                let recipe = self
                    .recipe
                    .as_ref()
                    .ok_or("deploy-target recipe is missing")?;
                if recipe.sync_argv.is_empty()
                    || recipe.restart_argv.is_empty()
                    || recipe.smoke_argv.is_empty()
                    || recipe
                        .sync_argv
                        .iter()
                        .chain(&recipe.restart_argv)
                        .chain(&recipe.smoke_argv)
                        .any(|v| v.is_empty())
                {
                    return Err("deploy-target recipe commands must be non-empty".into());
                }
                if let Some(via) = &self.via {
                    for argv in [&recipe.sync_argv, &recipe.restart_argv, &recipe.smoke_argv] {
                        if argv.first() != Some(via) {
                            return Err(format!(
                                "CLI deploy-target recipe commands must execute through '{via}'"
                            ));
                        }
                    }
                }
            }
            EndpointProviderKind::Billing => {
                if self.runtime.is_some()
                    || self.recipe.is_some()
                    || !self.models.is_empty()
                    || !self.capabilities.is_empty()
                    || self.via.is_some()
                {
                    return Err("billing provider has non-billing extension fields".into());
                }
                let mode = self
                    .billing_mode
                    .ok_or("billing provider mode is missing")?;
                if mode == BillingMode::AnthropicOrganization {
                    if !matches!(self.auth, AuthDescriptor::Env(_)) {
                        return Err("anthropic organization billing auth must use an `env: NAME` descriptor".into());
                    }
                    validate_identifier(
                        self.organization_id
                            .as_deref()
                            .ok_or("billing organization identity is missing")?,
                        "organization id",
                    )?;
                    if self
                        .organization_name
                        .as_deref()
                        .map_or(true, str::is_empty)
                    {
                        return Err("billing organization name is missing".into());
                    }
                }
                if !matches!(self.auth, AuthDescriptor::Env(_)) {
                    return Err(
                        "billing provider auth must be an env: NAME Admin credential reference"
                            .into(),
                    );
                }
                if let Some(project_id) = self.project_id.as_deref() {
                    validate_identifier(project_id, "project id")?;
                }
            }
        }
        if let Some(value) = &self.verified_at {
            chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|_| format!("invalid verified_at '{value}'"))?;
        }
        Ok(())
    }
}

pub fn validate_cloud_cli(value: &str) -> Result<(), String> {
    match value {
        "az" | "aws" | "gcloud" | "doctl" => Ok(()),
        _ => Err(format!("unsupported cloud CLI '{value}'")),
    }
}

pub(super) fn validate_model_identifier(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
    {
        Err(format!("invalid model '{value}'"))
    } else {
        Ok(())
    }
}

pub fn validate_host(value: &str) -> Result<(), String> {
    let Some((host, port)) = value.rsplit_once(':') else {
        return Err(format!("malformed host '{value}'"));
    };
    if host.is_empty()
        || host.contains(['/', '@', ' '])
        || port.parse::<u16>().ok().filter(|port| *port > 0).is_none()
    {
        return Err(format!("malformed host '{value}'"));
    }
    Ok(())
}

pub fn validate_ssh_host(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.starts_with('-')
        || value.contains(['/', '@', ' ', '\t', '\n'])
        || value.chars().any(char::is_control)
    {
        Err(format!("malformed ssh host '{value}'"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_api_runtime_const_is_a_valid_identifier() {
        assert!(validate_identifier(ANTHROPIC_API_RUNTIME, "runtime id").is_ok());
    }

    #[test]
    fn anthropic_api_default_capability_profile_declares_but_never_infers() {
        let profile = anthropic_api_default_capability_profile();
        assert_eq!(
            profile
                .capabilities
                .get(&RuntimeCapabilityConfig::NativeToolCalling),
            Some(&CapabilityProvenanceConfig::Declared)
        );
        assert_eq!(
            profile
                .capabilities
                .get(&RuntimeCapabilityConfig::PromptCaching),
            Some(&CapabilityProvenanceConfig::Declared)
        );
        // Capabilities this document never claims for the runtime stay
        // absent rather than defaulted — e.g. deterministic seeding is not
        // something the Messages API declares.
        assert_eq!(
            profile
                .capabilities
                .get(&RuntimeCapabilityConfig::DeterministicSeed),
            None
        );
    }
}
