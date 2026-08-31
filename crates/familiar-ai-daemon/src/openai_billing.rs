//! OpenAI accounting boundary. Transport credentials remain in the host-side
//! caller and this module accepts only already-authenticated response bodies.

use std::collections::BTreeSet;

use ring::digest::{digest, SHA256};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexBillingMode {
    SubscriptionDeclaration,
    LocalEstimate,
    WorkspaceEntitlement,
    Unknown,
}

/// Generic login status has no monetary meaning. Classification requires an
/// explicit machine field or a corroborated operator declaration.
pub fn classify_codex_auth(status: &Value, declared: Option<&str>) -> CodexBillingMode {
    if let Some(mode) = status
        .get("auth_mode")
        .and_then(Value::as_str)
        .and_then(parse_mode)
    {
        return mode;
    }
    match (
        declared.and_then(parse_mode),
        status
            .get("corroborating_auth_mode")
            .and_then(Value::as_str)
            .and_then(parse_mode),
    ) {
        (Some(declared), Some(observed)) if declared == observed => declared,
        _ => CodexBillingMode::Unknown,
    }
}

fn parse_mode(value: &str) -> Option<CodexBillingMode> {
    match value {
        "chatgpt" => Some(CodexBillingMode::SubscriptionDeclaration),
        "api-key" => Some(CodexBillingMode::LocalEstimate),
        "access-token" => Some(CodexBillingMode::WorkspaceEntitlement),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCostItem {
    pub organization_id: String,
    pub project_id: Option<String>,
    pub bucket_start: i64,
    pub bucket_end: i64,
    pub line_item: String,
    pub classification: String,
    pub amount_lexical: String,
    pub currency: String,
    pub payload_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCostPage {
    pub items: Vec<OpenAiCostItem>,
    pub next_page: Option<String>,
}

pub fn parse_cost_page(
    body: &str,
    expected_organization: Option<&str>,
) -> Result<ParsedCostPage, String> {
    let root: Value =
        serde_json::from_str(body).map_err(|_| "OpenAI Costs returned malformed JSON")?;
    let data = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or("OpenAI Costs response has no data array")?;
    let mut items = Vec::new();
    for bucket in data {
        let start = bucket
            .get("start_time")
            .and_then(Value::as_i64)
            .ok_or("cost bucket missing start_time")?;
        let end = bucket
            .get("end_time")
            .and_then(Value::as_i64)
            .ok_or("cost bucket missing end_time")?;
        for result in bucket
            .get("results")
            .and_then(Value::as_array)
            .ok_or("cost bucket missing results")?
        {
            let organization = result
                .get("organization_id")
                .and_then(Value::as_str)
                .ok_or("cost row missing organization_id")?;
            if expected_organization.is_some_and(|expected| expected != organization) {
                return Err(
                    "OpenAI organization identity changed; reject the ambiguous collector binding"
                        .into(),
                );
            }
            let amount = result
                .get("amount")
                .and_then(Value::as_object)
                .ok_or("cost row missing amount")?;
            let lexical = amount
                .get("value")
                .and_then(Value::as_number)
                .ok_or("cost amount.value is not a JSON number")?
                .to_string();
            let currency = amount
                .get("currency")
                .and_then(Value::as_str)
                .ok_or("cost amount.currency is missing")?
                .to_owned();
            let line_item = result
                .get("line_item")
                .and_then(Value::as_str)
                .unwrap_or("unclassified")
                .to_owned();
            let classification = result
                .get("classification")
                .and_then(Value::as_str)
                .unwrap_or(&line_item)
                .to_owned();
            items.push(OpenAiCostItem {
                organization_id: organization.to_owned(),
                project_id: result
                    .get("project_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                bucket_start: start,
                bucket_end: end,
                line_item,
                classification,
                amount_lexical: lexical,
                currency,
                payload_hash: hash(result.to_string().as_bytes()),
            });
        }
    }
    Ok(ParsedCostPage {
        items,
        next_page: root
            .get("next_page")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Validate a complete page chain before the caller commits any rows.
pub fn collect_fixture_pages(
    pages: &[&str],
    expected_organization: Option<&str>,
) -> Result<Vec<OpenAiCostItem>, String> {
    let mut next_page = None;
    let mut seen = BTreeSet::new();
    let mut all = Vec::new();
    for body in pages {
        if !seen.insert(hash(body.as_bytes())) {
            return Err(
                "duplicate OpenAI Costs page; retry collection from the last complete window"
                    .into(),
            );
        }
        let page = parse_cost_page(body, expected_organization)?;
        all.extend(page.items);
        next_page = page.next_page;
    }
    if next_page.is_some() {
        return Err("OpenAI Costs pagination incomplete; check OPENAI_ADMIN_KEY authority or expiry, then retry".into());
    }
    Ok(all)
}

fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_login_never_classifies_money() {
        assert_eq!(
            classify_codex_auth(&serde_json::json!({"logged_in":true}), None),
            CodexBillingMode::Unknown
        );
        assert_eq!(
            classify_codex_auth(&serde_json::json!({"auth_mode":"chatgpt"}), None),
            CodexBillingMode::SubscriptionDeclaration
        );
    }

    #[test]
    fn pagination_projects_adjustments_and_partial_failure_are_deterministic() {
        let first = r#"{"data":[{"start_time":1,"end_time":2,"results":[{"organization_id":"org_a","project_id":"proj_1","line_item":"completions","amount":{"value":0.0000000015,"currency":"usd"}}]}],"next_page":"c2"}"#;
        let second = r#"{"data":[{"start_time":1,"end_time":2,"results":[{"organization_id":"org_a","project_id":null,"line_item":"adjustment","classification":"credit","amount":{"value":-1,"currency":"usd"}}]}],"next_page":null}"#;
        let rows = collect_fixture_pages(&[first, second], Some("org_a")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].project_id.as_deref(), Some("proj_1"));
        assert_eq!(rows[1].classification, "credit");
        assert!(collect_fixture_pages(&[first], Some("org_a")).is_err());
        assert!(collect_fixture_pages(&[first, first], Some("org_a")).is_err());
    }
}
