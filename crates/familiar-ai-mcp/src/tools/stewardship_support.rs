//! Shared plumbing for the stewardship read and mutation tools (PRD-035):
//! repository identity resolution, pagination bounds, and response
//! redaction. Repository identity is resolved exactly as the `familiar-ai`
//! CLI resolves it (`FilesystemBacklogDiscovery`), so a tool call from a
//! given working directory always sees the same repository the CLI would.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use familiar_ai_core::config::Config;
use familiar_ai_core::{
    validate_graph, BacklogDiscovery, DiscoveredPrd, FilesystemBacklogDiscovery, RepositoryIdentity,
};

use crate::tool::ToolError;

/// Upper bound on any single page returned by a stewardship read tool.
pub const HARD_LIMIT: usize = 100;
pub const DEFAULT_LIMIT: usize = 20;

/// Free-text fields (usage/test-evidence/findings JSON blobs) are bounded
/// and scanned for secret markers before leaving the process. A field this
/// large is either misconfigured or a raw dump the caller never intended to
/// disclose in full; the response says so rather than truncating silently.
pub const MAX_FIELD_BYTES: usize = 8 * 1024;

const SECRET_MARKERS: &[&str] = &[
    "-----begin private key-----",
    "-----begin rsa private key-----",
    "aws_secret_access_key",
    "authorization: bearer ",
    "github_pat_",
    "sk-proj-",
];

pub fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, HARD_LIMIT)
}

fn contains_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Redact a stored JSON-text field for disclosure: a detected secret marker
/// replaces the value entirely; an oversized value is reported truncated
/// rather than silently trimmed; otherwise the JSON is embedded structured
/// so callers do not have to parse a nested string.
pub fn redact_json_field(raw: &str) -> Value {
    if contains_secret(raw) {
        return json!({"redacted": true, "reason": "secret marker detected"});
    }
    if raw.len() > MAX_FIELD_BYTES {
        let cut = floor_char_boundary(raw, MAX_FIELD_BYTES);
        return json!({
            "truncated": true,
            "original_bytes": raw.len(),
            "preview": &raw[..cut],
        });
    }
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Largest byte index `<= max` that lands on a UTF-8 char boundary of `s`.
/// Slicing a `&str` at a non-boundary index panics, and multi-byte
/// characters routinely straddle a fixed byte offset in model/execution
/// output, so the cut point must be found rather than assumed.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    let mut cut = max.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// Resolve the repository the tool call should read/mutate. `repository_path`
/// defaults to the server process's current working directory, matching how
/// the `familiar-ai` CLI resolves its repository from the caller's cwd.
pub fn resolve_repository(repository_path: Option<&str>) -> Result<RepositoryIdentity, ToolError> {
    let start: PathBuf = match repository_path {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()
            .map_err(|e| ToolError::Internal(format!("cannot resolve current directory: {e}")))?,
    };
    FilesystemBacklogDiscovery
        .resolve(&start)
        .map_err(|e| ToolError::InvalidParams(format!("cannot resolve repository: {e}")))
}

/// Discover and validate the full backlog graph for `repository`, exactly as
/// the CLI does before dispatching a backlog command.
pub fn discover_prds(
    repository: &RepositoryIdentity,
    config: &Config,
) -> Result<Vec<DiscoveredPrd>, ToolError> {
    let layout_config = config
        .repository(&repository.worktree)
        .map_err(|e| ToolError::Internal(format!("repository policy resolution failed: {e}")))?;
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(repository, &layout_config.layout())
        .map_err(|e| ToolError::Internal(format!("backlog discovery failed: {e}")))?;
    validate_graph(&discovered)
        .map_err(|e| ToolError::InvalidParams(format!("backlog graph is invalid: {e}")))?;
    Ok(discovered)
}

/// Resolve one supplied PRD path to its exact discovered bytes, mirroring
/// the CLI's `resolve_run_prd`.
pub fn resolve_target<'a>(
    repository: &RepositoryIdentity,
    discovered: &'a [DiscoveredPrd],
    supplied_path: &str,
) -> Result<&'a DiscoveredPrd, ToolError> {
    let path = Path::new(supplied_path);
    let canonical = if path.is_absolute() {
        path.to_owned()
    } else {
        repository.worktree.join(path)
    };
    let canonical = canonical
        .canonicalize()
        .map_err(|e| ToolError::InvalidParams(format!("{supplied_path}: {e}")))?;
    let relative = canonical
        .strip_prefix(&repository.worktree)
        .map_err(|_| {
            ToolError::InvalidParams(format!("{supplied_path} is outside the repository"))
        })?
        .to_str()
        .ok_or_else(|| ToolError::InvalidParams("run path is not UTF-8".into()))?
        .replace('\\', "/");
    discovered
        .iter()
        .find(|prd| prd.path.as_str() == relative)
        .ok_or_else(|| {
            ToolError::InvalidParams(format!("{supplied_path} is not an active backlog entry"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_applies_default_and_hard_ceiling() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(1_000)), HARD_LIMIT);
        assert_eq!(clamp_limit(Some(5)), 5);
    }

    #[test]
    fn redact_json_field_hides_secret_markers() {
        let value = redact_json_field("Authorization: Bearer sekrit-token");
        assert_eq!(value["redacted"], true);
    }

    #[test]
    fn redact_json_field_bounds_oversized_values() {
        let raw = "x".repeat(MAX_FIELD_BYTES + 1);
        let value = redact_json_field(&raw);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["original_bytes"], MAX_FIELD_BYTES + 1);
    }

    #[test]
    fn redact_json_field_bounds_oversized_values_with_multibyte_boundary() {
        // A 3-byte UTF-8 character ('€') straddling the MAX_FIELD_BYTES cut
        // point must not panic; the cut must land before the character.
        let mut raw = "x".repeat(MAX_FIELD_BYTES - 1);
        raw.push('€');
        raw.push_str("more text to exceed the bound");
        let value = redact_json_field(&raw);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["original_bytes"], raw.len());
        let preview = value["preview"].as_str().unwrap();
        assert!(preview.len() <= MAX_FIELD_BYTES);
        assert_eq!(preview, &raw[..MAX_FIELD_BYTES - 1]);
    }

    #[test]
    fn redact_json_field_embeds_ordinary_json_structured() {
        let value = redact_json_field(r#"{"a":1}"#);
        assert_eq!(value["a"], 1);
    }

    #[test]
    fn redact_json_field_falls_back_to_string_for_non_json() {
        let value = redact_json_field("plain text");
        assert_eq!(value, Value::String("plain text".into()));
    }
}
