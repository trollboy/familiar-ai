use std::time::Duration;

use familiar_ai_core::config::{AuthDescriptor, BillingMode, ProviderConfig};
use familiar_ai_storage::{BillingRepository, ProviderCostRow};
use reqwest::blocking::Client;
use serde_json::Value;

pub const INDIVIDUAL_REMEDY: &str = "credential lacks organization authority — individual API accounts have no Admin API; view billing in the Claude Console, or supply an organization Admin key";
pub const MISSING_CREDENTIAL_REMEDY: &str =
    "billing credential reference is unavailable — export the referenced organization Admin key";
pub const EXPIRED_CREDENTIAL_REMEDY: &str =
    "billing credential is expired or invalid — replace the referenced organization Admin key";
pub const INSUFFICIENT_ROLE_REMEDY: &str = "credential has insufficient role — supply an organization Admin key with billing read authority";
pub const EXTERNAL_REMEDY: &str = "external billing collector unsupported — local-estimate-only coverage until a dedicated PRD authorizes a collector";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizationIdentity {
    pub id: String,
    pub name: String,
}

/// Agent resolution is the last host-side boundary before model subprocesses
/// are built. Remove every configured billing secret from the ambient process
/// so neither Claude Code nor any other worker can inherit it.
pub fn scrub_credentials_from_process_environment(config: &familiar_ai_core::Config) {
    for provider in config.providers.values() {
        if provider.kind == familiar_ai_core::config::EndpointProviderKind::Billing {
            if let AuthDescriptor::Env(name) = &provider.auth {
                std::env::remove_var(name);
            }
        }
    }
}

pub fn probe_organization(provider: &ProviderConfig) -> Result<OrganizationIdentity, String> {
    if provider.billing_mode != Some(BillingMode::AnthropicOrganization) {
        return Err(EXTERNAL_REMEDY.into());
    }
    let credential = credential(&provider.auth)?;
    let response = client()?
        .get(endpoint(&provider.host, "/v1/organizations/me"))
        .header("x-api-key", credential)
        .header("anthropic-version", "2023-06-01")
        .send()
        .map_err(|_| {
            format!("organization identity endpoint unavailable — {MISSING_CREDENTIAL_REMEDY}")
        })?;
    match response.status().as_u16() {
        401 => return Err(EXPIRED_CREDENTIAL_REMEDY.into()),
        403 => return Err(INDIVIDUAL_REMEDY.into()),
        code if !(200..300).contains(&code) => {
            return Err(format!(
                "organization identity probe returned HTTP {code} — {INSUFFICIENT_ROLE_REMEDY}"
            ))
        }
        _ => {}
    }
    let body: Value = response
        .json()
        .map_err(|_| "organization identity endpoint returned malformed data".to_string())?;
    let object = body.get("organization").unwrap_or(&body);
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| INDIVIDUAL_REMEDY.to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unnamed organization");
    Ok(OrganizationIdentity {
        id: id.into(),
        name: name.into(),
    })
}

pub fn collect(
    provider_name: &str,
    provider: &ProviderConfig,
    start: &str,
    end: &str,
    repo: &BillingRepository<'_>,
) -> Result<usize, String> {
    if provider.billing_mode != Some(BillingMode::AnthropicOrganization) {
        return Err(EXTERNAL_REMEDY.into());
    }
    let identity = probe_organization(provider)?;
    if provider.organization_id.as_deref() != Some(identity.id.as_str()) {
        return Err("billing credential resolved to a different organization — collection refused; re-add the source with the intended organization Admin key".into());
    }
    let credential = credential(&provider.auth)?;
    let mut cursor: Option<String> = None;
    let mut rows = Vec::new();
    loop {
        let mut request = client()?
            .get(endpoint(&provider.host, "/v1/organizations/cost_report"))
            .header("x-api-key", &credential)
            .header("anthropic-version", "2023-06-01")
            .query(&[
                ("starting_at", start),
                ("ending_at", end),
                ("group_by[]", "workspace_id"),
                ("group_by[]", "description"),
            ]);
        if let Some(value) = cursor.as_deref() {
            request = request.query(&[("page", value)]);
        }
        let response = match request.send() {
            Ok(v) => v,
            Err(_) => {
                let remedy="cost report unavailable — retry explicit collection after restoring the endpoint";
                repo.record_failed(provider_name, start, end, cursor.as_deref(), remedy)
                    .map_err(|e| e.to_string())?;
                return Err(remedy.into());
            }
        };
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            let remedy = if status == 401 {
                EXPIRED_CREDENTIAL_REMEDY
            } else {
                INSUFFICIENT_ROLE_REMEDY
            };
            repo.record_failed(provider_name, start, end, cursor.as_deref(), remedy)
                .map_err(|e| e.to_string())?;
            return Err(remedy.into());
        }
        if !(200..300).contains(&status) {
            let remedy=format!("cost report returned HTTP {status} — retry explicit collection; prior complete snapshot retained");
            repo.record_failed(provider_name, start, end, cursor.as_deref(), &remedy)
                .map_err(|e| e.to_string())?;
            return Err(remedy);
        }
        let body: Value = match response.json() {
            Ok(v) => v,
            Err(_) => {
                let remedy =
                    "cost report returned malformed data — prior complete snapshot retained";
                repo.record_failed(provider_name, start, end, cursor.as_deref(), remedy)
                    .map_err(|e| e.to_string())?;
                return Err(remedy.into());
            }
        };
        let Some(data) = body.get("data").and_then(Value::as_array) else {
            let remedy =
                "cost report response is missing data array — prior complete snapshot retained";
            repo.record_failed(provider_name, start, end, cursor.as_deref(), remedy)
                .map_err(|e| e.to_string())?;
            return Err(remedy.into());
        };
        for entry in data {
            match parse_row(entry, start, end, &credential) {
                Ok(row) => rows.push(row),
                Err(error) => {
                    let remedy = format!("{error} — prior complete snapshot retained");
                    repo.record_failed(provider_name, start, end, cursor.as_deref(), &remedy)
                        .map_err(|e| e.to_string())?;
                    return Err(remedy);
                }
            }
        }
        let more = body
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !more {
            break;
        }
        let next = body
            .get("next_page")
            .or_else(|| body.get("next_page_token"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let Some(next) = next else {
            let remedy = "cost report claims another page without next_page — prior complete snapshot retained";
            repo.record_failed(provider_name, start, end, cursor.as_deref(), remedy)
                .map_err(|e| e.to_string())?;
            return Err(remedy.into());
        };
        if cursor.as_deref() == Some(next) {
            let remedy =
                "cost report repeated a pagination cursor — prior complete snapshot retained";
            repo.record_failed(provider_name, start, end, cursor.as_deref(), remedy)
                .map_err(|e| e.to_string())?;
            return Err(remedy.into());
        }
        cursor = Some(next.into());
    }
    repo.commit_complete(provider_name, start, end, &rows)
        .map_err(|e| e.to_string())
}

fn parse_row(
    value: &Value,
    default_start: &str,
    default_end: &str,
    credential: &str,
) -> Result<ProviderCostRow, String> {
    let amount = value.get("amount");
    let lexical = amount
        .and_then(|v| v.get("amount"))
        .or(amount)
        .and_then(Value::as_str)
        .or_else(|| value.get("cost").and_then(Value::as_str))
        .ok_or("cost row amount is not a string")?;
    let currency = amount
        .and_then(|v| v.get("currency"))
        .and_then(Value::as_str)
        .or_else(|| value.get("currency").and_then(Value::as_str))
        .unwrap_or("USD");
    if currency != "USD" {
        return Err(format!(
            "unsupported billing currency '{currency}' — collection failed closed"
        ));
    }
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("unclassified");
    let class = value
        .get("charge_type")
        .or_else(|| value.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            let lower = description.to_ascii_lowercase();
            if lower.contains("credit") {
                "credit"
            } else if lower.contains("adjust") {
                "adjustment"
            } else if lower.contains("token") {
                "token-spend"
            } else {
                "non-token"
            }
        });
    Ok(ProviderCostRow {
        bucket_start: value
            .get("starting_at")
            .and_then(Value::as_str)
            .unwrap_or(default_start)
            .into(),
        bucket_end: value
            .get("ending_at")
            .and_then(Value::as_str)
            .unwrap_or(default_end)
            .into(),
        workspace_id: value
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        description: description.into(),
        charge_class: class.into(),
        currency: currency.into(),
        amount_lexical: lexical.into(),
        provider_payload: serde_json::to_string(&redact_provider_value(value, credential))
            .map_err(|e| e.to_string())?,
    })
}

fn redact_provider_value(value: &Value, credential: &str) -> Value {
    match value {
        Value::String(text) if text == credential => Value::String("[REDACTED]".into()),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| redact_provider_value(value, credential))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let value = if lower.contains("authorization")
                        || lower.contains("api_key")
                        || lower.contains("token")
                        || lower.contains("secret")
                    {
                        Value::String("[REDACTED]".into())
                    } else {
                        redact_provider_value(value, credential)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn credential(auth: &AuthDescriptor) -> Result<String, String> {
    match auth {
        AuthDescriptor::Env(name) => std::env::var(name)
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{MISSING_CREDENTIAL_REMEDY}: {name}")),
        _ => Err(
            "billing auth must use an `env: NAME` descriptor; credential values are never accepted"
                .into(),
        ),
    }
}
fn endpoint(host: &str, path: &str) -> String {
    if host.starts_with("localhost:")
        || host.starts_with("127.0.0.1:")
        || host.starts_with("[::1]:")
    {
        format!("http://{host}{path}")
    } else {
        format!("https://{host}{path}")
    }
}
fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("could not build billing client ({e})"))
}
