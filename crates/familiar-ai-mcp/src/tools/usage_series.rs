use crate::tool::{Tool, ToolContext, ToolError};
use async_trait::async_trait;
use chrono::Utc;
use familiar_ai_storage::{UsageBucket, UsageSeriesRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct UsageSeriesTool;

#[derive(Deserialize)]
struct Args {
    start: String,
    end: String,
    bucket: String,
    #[serde(default)]
    group_by: Vec<String>,
    #[serde(default)]
    filter: BTreeMap<String, String>,
    #[serde(default)]
    dense: bool,
}

#[async_trait]
impl Tool for UsageSeriesTool {
    fn name(&self) -> &'static str {
        "stewardship.usage_series"
    }
    fn description(&self) -> &'static str {
        "Queries cached provider-neutral project usage and spend; never performs network collection."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["start","end","bucket"],"properties":{"start":{"type":"string"},"end":{"type":"string"},"bucket":{"enum":["hour","day","week","month"]},"group_by":{"type":"array","items":{"enum":["project","provider","model","prd","execution","attempt","stage","billing_source","attribution_status"]}},"filter":{"type":"object"},"dense":{"type":"boolean"}}})
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<Value, ToolError> {
        let args: Args =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        let parse = |v: &str| {
            chrono::DateTime::parse_from_rfc3339(v)
                .map(|v| v.with_timezone(&Utc))
                .map_err(|e| ToolError::InvalidParams(e.to_string()))
        };
        let bucket = match args.bucket.as_str() {
            "hour" => UsageBucket::Hour,
            "day" => UsageBucket::Day,
            "week" => UsageBucket::Week,
            "month" => UsageBucket::Month,
            _ => return Err(ToolError::InvalidParams("invalid bucket".into())),
        };
        let points = ctx
            .storage
            .usage_series(&UsageSeriesRequest {
                start: parse(&args.start)?,
                end: parse(&args.end)?,
                bucket,
                group_by: args.group_by,
                filters: args.filter,
                dense: args.dense,
            })
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        Ok(json!({"staleness":"cached-local","network_collection":false,"points":points}))
    }
}
