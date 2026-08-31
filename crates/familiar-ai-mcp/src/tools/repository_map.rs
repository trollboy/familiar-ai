use crate::tool::{Tool, ToolContext, ToolError};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct RepositoryMapTool;
#[derive(Deserialize)]
struct Input {
    repository: PathBuf,
    #[serde(default)]
    files: Vec<PathBuf>,
    #[serde(default = "limit")]
    max_symbols: usize,
}
fn limit() -> usize {
    500
}

#[async_trait]
impl Tool for RepositoryMapTool {
    fn name(&self) -> &'static str {
        "context.repository_map"
    }
    fn description(&self) -> &'static str {
        "Read deterministic signatures and reference edges; coverage is explicit."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","required":["repository"],"properties":{"repository":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"max_symbols":{"type":"integer","minimum":1,"maximum":5000}}})
    }
    async fn call(&self, args: Value, _: &ToolContext) -> Result<Value, ToolError> {
        let input: Input =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        let repository = input
            .repository
            .canonicalize()
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?;
        if input.files.is_empty() {
            if let Ok(paths) = familiar_ai_core::AppPaths::resolve() {
                let cached = paths.data_dir.join("repomaps").join(format!(
                    "{}.map",
                    familiar_ai_repomap::repository_cache_key(&repository)
                ));
                if let Ok(map) = std::fs::read_to_string(cached) {
                    return Ok(json!({"map":map,"coverage":"maintained"}));
                }
            }
        }
        let mut map = familiar_ai_repomap::RepositoryMap::new(false);
        for file in input.files {
            let path = if file.is_absolute() {
                file
            } else {
                repository.join(file)
            };
            let _ = map.reindex_file(&repository, &path);
        }
        let serialized = String::from_utf8(map.serialize(input.max_symbols.min(5000)))
            .expect("UTF-8 serialization");
        Ok(
            json!({"map":serialized,"coverage":"partial","missing":map.missing_coverage().collect::<Vec<_>>() }),
        )
    }
}
