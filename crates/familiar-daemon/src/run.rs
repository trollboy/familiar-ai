use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use chrono::Utc;
use familiar_core::{AppPaths, Config, ExecutionPrice};
use familiar_storage::{
    Database, ExecutionFinalization, ExecutionHistoryRepository, ExecutionStart,
};
use serde_json::Value;

const REFERENCE_PREFIXES: [&str; 2] = ["docs/adr/", "docs/contracts/"];
const EXECUTION_CONSTRAINTS: &str = r#"- Implement the supplied PRD exactly as written and do not broaden its scope.
- Treat repository source and Git state as authoritative.
- Inspect the existing implementation and identify blocking conflicts before editing.
- Do not modify architecture documents, ADRs, contracts, or existing PRDs.
- Do not implement later PRDs or perform unrelated cleanup.
- Preserve existing user changes in the worktree.
- When implementation is complete, audit every acceptance criterion, run focused tests, formatting, static analysis, and attempt the workspace test suite.
- Distinguish implementation-caused failures from pre-existing failures and summarize changed files and deviations.
- Stop after completing and reporting the supplied PRD."#;

static EXECUTION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum RunError {
    CurrentDirectory(io::Error),
    RepositoryRootNotFound(PathBuf),
    InvalidPrdPath(String),
    ReadDocument {
        path: PathBuf,
        source: io::Error,
    },
    Git(String),
    Config(String),
    Storage(String),
    Spawn {
        executable: String,
        source: io::Error,
    },
    Feed(io::Error),
    Wait(io::Error),
    Output(io::Error),
    HistoryFinalize {
        execution_id: String,
        detail: String,
    },
}
impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(e) => write!(f, "cannot resolve current directory: {e}"),
            Self::RepositoryRootNotFound(p) => write!(
                f,
                "no Git repository root found at or above {}",
                p.display()
            ),
            Self::InvalidPrdPath(m) => f.write_str(m),
            Self::ReadDocument { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Self::Git(m) => write!(f, "Git query failed: {m}"),
            Self::Config(m) => write!(f, "configuration failed: {m}"),
            Self::Storage(m) => write!(f, "execution history failed: {m}"),
            Self::Spawn { executable, source } => {
                write!(f, "cannot launch Codex executable {executable:?}: {source}")
            }
            Self::Feed(e) => write!(f, "cannot feed execution prompt to Codex: {e}"),
            Self::Wait(e) => write!(f, "cannot wait for Codex: {e}"),
            Self::Output(e) => write!(f, "cannot read Codex structured output: {e}"),
            Self::HistoryFinalize {
                execution_id,
                detail,
            } => write!(f, "history_finalize_failed for {execution_id}: {detail}"),
        }
    }
}
impl std::error::Error for RunError {}

#[derive(Debug)]
struct Context {
    worktree: PathBuf,
    repository: PathBuf,
    commit: Option<String>,
    prd_path: PathBuf,
    prd_identity: String,
}

pub fn execute(prd_path: &Path, codex_executable: &str) -> Result<ExitStatus, RunError> {
    let paths = AppPaths::new();
    let config = Config::load(Some(&paths.config_dir.join("config.toml")))
        .map_err(|e| RunError::Config(e.to_string()))?;
    execute_with_config(prd_path, codex_executable, &config, &paths)
}

pub fn execute_with_config(
    prd_path: &Path,
    codex_executable: &str,
    config: &Config,
    paths: &AppPaths,
) -> Result<ExitStatus, RunError> {
    let current = env::current_dir().map_err(RunError::CurrentDirectory)?;
    let context = resolve_context(&current, prd_path)?;
    let prompt = build_prompt(&context.worktree, &context.prd_path)?;
    let database_path = config.database.resolve_path(&paths.data_dir);
    let db = Database::open(&database_path).map_err(|e| RunError::Storage(e.to_string()))?;
    db.run_migrations()
        .map_err(|e| RunError::Storage(e.to_string()))?;
    let id = new_id();
    let started_at = Utc::now().to_rfc3339();
    let timer = Instant::now();
    let mut unavailable = BTreeMap::new();
    for field in [
        "ended_at",
        "duration_ms",
        "agent_version",
        "model",
        "input_tokens",
        "output_tokens",
        "cached_tokens",
        "total_tokens",
        "estimated_cost_microusd",
        "exit_code",
    ] {
        unavailable.insert(field.into(), "runner_interrupted".into());
    }
    if context.commit.is_none() {
        unavailable.insert("git_commit".into(), "git_unavailable".into());
    }
    ExecutionHistoryRepository::new(db.conn())
        .insert_running(&ExecutionStart {
            execution_id: id.clone(),
            started_at,
            repository: slash(&context.repository),
            worktree: slash(&context.worktree),
            git_commit: context.commit.clone(),
            prd_path: context.prd_identity.clone(),
            unavailable_fields: unavailable.clone(),
        })
        .map_err(|e| RunError::Storage(e.to_string()))?;

    let version = probe_version(codex_executable);
    if version.is_none() {
        unavailable.insert("agent_version".into(), "version_probe_failed".into());
    }
    let mut child = match Command::new(codex_executable)
        .args(["exec", "--json", "-"])
        .current_dir(&context.worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(source) => {
            let finalization = terminal(
                &timer,
                version,
                Metrics::default(),
                "launch_failed",
                None,
                None,
                unavailable,
                config,
            );
            finalize(&db, &id, &finalization)?;
            return Err(RunError::Spawn {
                executable: codex_executable.to_owned(),
                source,
            });
        }
    };
    let feed = child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(prompt.as_bytes());
    let mut metrics = Metrics::default();
    let output_result = stream_json(
        child.stdout.take().expect("piped stdout"),
        &mut io::stdout(),
        &mut metrics,
    );
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            unavailable.insert("exit_code".into(), "no_normal_exit_code".into());
            let finalization = terminal(
                &timer,
                version,
                metrics,
                "failed",
                None,
                None,
                unavailable,
                config,
            );
            finalize(&db, &id, &finalization)?;
            return Err(RunError::Wait(error));
        }
    };
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    let (outcome, exit_code) = if let Some(code) = status.code() {
        (if code == 0 { "succeeded" } else { "failed" }, Some(code))
    } else {
        ("signaled", None)
    };
    if exit_code.is_none() {
        unavailable.insert("exit_code".into(), "no_normal_exit_code".into());
    }
    let final_outcome = if feed.is_err() && status.success() {
        "input_failed"
    } else {
        outcome
    };
    let finalization = terminal(
        &timer,
        version,
        metrics,
        final_outcome,
        exit_code,
        signal,
        unavailable,
        config,
    );
    finalize(&db, &id, &finalization)?;
    if let Err(error) = output_result {
        return Err(RunError::Output(error));
    }
    match feed {
        Ok(()) => Ok(status),
        Err(_) if !status.success() => Ok(status),
        Err(error) => Err(RunError::Feed(error)),
    }
}

fn finalize(db: &Database, id: &str, value: &ExecutionFinalization) -> Result<(), RunError> {
    ExecutionHistoryRepository::new(db.conn())
        .finalize(id, value)
        .map_err(|e| RunError::HistoryFinalize {
            execution_id: id.into(),
            detail: e.to_string(),
        })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metrics {
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
}

pub fn parse_event(line: &str, metrics: &mut Metrics) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "turn.started" => None,
        "turn.completed" => {
            if let Some(usage) = value.get("usage").and_then(Value::as_object) {
                metrics.input_tokens = uint(usage.get("input_tokens"));
                metrics.output_tokens = uint(usage.get("output_tokens"));
                metrics.cached_tokens = uint(usage.get("cached_input_tokens"));
            }
            None
        }
        "item.completed" => {
            let item = value.get("item")?;
            match item.get("type").and_then(Value::as_str)? {
                "agent_message" => item.get("text").and_then(Value::as_str).map(str::to_owned),
                "reasoning" => item.get("text").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            }
        }
        "error" => value
            .get("message")
            .and_then(Value::as_str)
            .map(|s| format!("error: {s}")),
        _ => None,
    }
}
fn uint(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64)
        .filter(|value| *value <= i64::MAX as u64)
}
fn stream_json<R: io::Read, W: Write>(
    reader: R,
    output: &mut W,
    metrics: &mut Metrics,
) -> io::Result<()> {
    for line in BufReader::new(reader).lines() {
        let line = line?;
        if let Some(text) = parse_event(&line, metrics) {
            writeln!(output, "{text}")?;
            output.flush()?;
        } else if serde_json::from_str::<Value>(&line).is_err() {
            writeln!(output, "{line}")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn terminal(
    timer: &Instant,
    version: Option<String>,
    metrics: Metrics,
    outcome: &str,
    exit_code: Option<i32>,
    signal: Option<i32>,
    mut missing: BTreeMap<String, String>,
    config: &Config,
) -> ExecutionFinalization {
    for field in ["ended_at", "duration_ms", "signal"] {
        missing.remove(field);
    }
    if version.is_some() {
        missing.remove("agent_version");
    }
    if exit_code.is_some() {
        missing.remove("exit_code");
    } else {
        missing.insert("exit_code".into(), "no_normal_exit_code".into());
    }
    if metrics.model.is_none() {
        missing.insert("model".into(), "agent_not_reported".into());
    } else {
        missing.remove("model");
    }
    for (name, value) in [
        ("input_tokens", metrics.input_tokens),
        ("output_tokens", metrics.output_tokens),
        ("cached_tokens", metrics.cached_tokens),
    ] {
        if value.is_none() {
            missing.insert(name.into(), "usage_not_reported".into());
        } else {
            missing.remove(name);
        }
    }
    let total = match (metrics.input_tokens, metrics.output_tokens) {
        (Some(a), Some(b)) => a.checked_add(b).filter(|value| *value <= i64::MAX as u64),
        _ => None,
    };
    if total.is_none() {
        missing.insert(
            "total_tokens".into(),
            if metrics.input_tokens.is_some() && metrics.output_tokens.is_some() {
                "arithmetic_overflow"
            } else {
                "usage_incomplete"
            }
            .into(),
        );
    } else {
        missing.remove("total_tokens");
    }
    let price = metrics
        .model
        .as_ref()
        .and_then(|m| config.execution_history.pricing.get(m));
    let (cost, rates, reason) = calculate_cost(
        metrics.input_tokens,
        metrics.cached_tokens,
        metrics.output_tokens,
        price,
    );
    if cost.is_none() {
        missing.insert("estimated_cost_microusd".into(), reason.into());
    } else {
        missing.remove("estimated_cost_microusd");
    }
    ExecutionFinalization {
        ended_at: Utc::now().to_rfc3339(),
        duration_ms: u64::try_from(timer.elapsed().as_millis())
            .unwrap_or(i64::MAX as u64)
            .min(i64::MAX as u64),
        agent_version: version,
        model: metrics.model,
        input_tokens: metrics.input_tokens,
        output_tokens: metrics.output_tokens,
        cached_tokens: metrics.cached_tokens,
        total_tokens: total,
        estimated_cost_microusd: cost,
        input_rate: rates.0,
        cached_input_rate: rates.1,
        output_rate: rates.2,
        outcome: outcome.into(),
        exit_code,
        signal,
        unavailable_fields: missing,
    }
}

#[allow(clippy::type_complexity)]
pub fn calculate_cost(
    input: Option<u64>,
    cached: Option<u64>,
    output: Option<u64>,
    price: Option<&ExecutionPrice>,
) -> (
    Option<u64>,
    (Option<u64>, Option<u64>, Option<u64>),
    &'static str,
) {
    let Some(price) = price else {
        return (None, (None, None, None), "pricing_not_configured");
    };
    let rates = (
        price.input_microusd_per_million,
        price.cached_input_microusd_per_million,
        price.output_microusd_per_million,
    );
    if [rates.0, rates.1, rates.2]
        .into_iter()
        .flatten()
        .any(|rate| rate > i64::MAX as u64)
    {
        return (None, (None, None, None), "arithmetic_overflow");
    }
    let (Some(ir), Some(cr), Some(or)) = rates else {
        return (None, rates, "pricing_rate_incomplete");
    };
    let (Some(i), Some(c), Some(o)) = (input, cached, output) else {
        return (None, rates, "usage_incomplete");
    };
    let Some(u) = i.checked_sub(c) else {
        return (None, rates, "usage_incomplete");
    };
    let numerator = u
        .checked_mul(ir)
        .and_then(|x| c.checked_mul(cr).and_then(|y| x.checked_add(y)))
        .and_then(|x| o.checked_mul(or).and_then(|y| x.checked_add(y)));
    let Some(n) = numerator else {
        return (None, rates, "arithmetic_overflow");
    };
    let Some(rounded) = n.checked_add(500_000) else {
        return (None, rates, "arithmetic_overflow");
    };
    let cost = rounded / 1_000_000;
    if cost > i64::MAX as u64 {
        return (None, rates, "arithmetic_overflow");
    }
    (Some(cost), rates, "")
}

fn probe_version(executable: &str) -> Option<String> {
    let output = Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() || !line.to_ascii_lowercase().contains("codex") {
        None
    } else {
        Some(line.to_owned())
    }
}
fn new_id() -> String {
    format!(
        "{:020}-{:010}-{:06}",
        Utc::now().timestamp_micros(),
        std::process::id(),
        EXECUTION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
fn git(cwd: &Path, args: &[&str]) -> Result<Option<String>, RunError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| RunError::Git(e.to_string()))?;
    if !out.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(out.stdout)
        .map_err(|e| RunError::Git(e.to_string()))?
        .trim()
        .to_owned();
    Ok((!value.is_empty()).then_some(value))
}
fn resolve_context(start: &Path, supplied: &Path) -> Result<Context, RunError> {
    let worktree_raw = git(start, &["rev-parse", "--show-toplevel"])?
        .ok_or_else(|| RunError::Git("worktree root unavailable".into()))?;
    let worktree = PathBuf::from(worktree_raw)
        .canonicalize()
        .map_err(RunError::CurrentDirectory)?;
    let common = git(&worktree, &["rev-parse", "--git-common-dir"])?
        .ok_or_else(|| RunError::Git("common directory unavailable".into()))?;
    let common = PathBuf::from(common);
    let repository = if common.is_absolute() {
        common
    } else {
        worktree.join(common)
    }
    .canonicalize()
    .map_err(RunError::CurrentDirectory)?;
    let prd_path = validate_prd_path(&worktree, supplied)?;
    let identity = slash(prd_path.strip_prefix(&worktree).expect("validated PRD"));
    let commit = git(&worktree, &["rev-parse", "--verify", "HEAD"])?;
    Ok(Context {
        worktree,
        repository,
        commit,
        prd_path,
        prd_identity: identity,
    })
}

pub fn resolve_repository_root(start: &Path) -> Result<PathBuf, RunError> {
    git(start, &["rev-parse", "--show-toplevel"])?
        .ok_or_else(|| RunError::RepositoryRootNotFound(start.into()))
        .and_then(|p| {
            PathBuf::from(p)
                .canonicalize()
                .map_err(RunError::CurrentDirectory)
        })
}
pub fn build_prompt(repository_root: &Path, supplied_path: &Path) -> Result<String, RunError> {
    let repository_root = repository_root
        .canonicalize()
        .map_err(RunError::CurrentDirectory)?;
    let prd_path = validate_prd_path(&repository_root, supplied_path)?;
    let prd = read_utf8(&prd_path)?;
    let references = discover_references(&repository_root, &prd)?;
    let relative_prd = prd_path
        .strip_prefix(&repository_root)
        .expect("validated path");
    let mut prompt = String::new();
    prompt.push_str("# Familiar execution request\n\nImplement the PRD below in this repository.\n\n## Fixed execution constraints\n\n");
    prompt.push_str(EXECUTION_CONSTRAINTS);
    prompt.push_str("\n\n## PRD: ");
    prompt.push_str(&relative_prd.to_string_lossy());
    prompt.push_str("\n\n");
    prompt.push_str(&prd);
    prompt.push('\n');
    for reference in references {
        let relative = reference
            .strip_prefix(&repository_root)
            .expect("contained reference");
        prompt.push_str("\n## Directly referenced document: ");
        prompt.push_str(&relative.to_string_lossy());
        prompt.push_str("\n\n");
        prompt.push_str(&read_utf8(&reference)?);
        prompt.push('\n');
    }
    Ok(prompt)
}
fn validate_prd_path(repository_root: &Path, supplied_path: &Path) -> Result<PathBuf, RunError> {
    if supplied_path.as_os_str().is_empty() {
        return Err(RunError::InvalidPrdPath("PRD path cannot be empty".into()));
    }
    let candidate = if supplied_path.is_absolute() {
        supplied_path.into()
    } else {
        env::current_dir()
            .map_err(RunError::CurrentDirectory)?
            .join(supplied_path)
    };
    let path = candidate.canonicalize().map_err(|e| {
        RunError::InvalidPrdPath(format!(
            "cannot resolve PRD path {}: {e}",
            candidate.display()
        ))
    })?;
    let prd_dir = repository_root
        .join("docs/prds")
        .canonicalize()
        .map_err(|e| {
            RunError::InvalidPrdPath(format!("cannot resolve repository PRD directory: {e}"))
        })?;
    if !path.starts_with(&prd_dir) {
        return Err(RunError::InvalidPrdPath(format!(
            "PRD path must be contained in {}",
            prd_dir.display()
        )));
    }
    if !path.is_file() {
        return Err(RunError::InvalidPrdPath(format!(
            "PRD path is not a regular file: {}",
            path.display()
        )));
    }
    if path.extension().and_then(|v| v.to_str()) != Some("md") {
        return Err(RunError::InvalidPrdPath(format!(
            "PRD path must have a .md extension: {}",
            path.display()
        )));
    }
    Ok(path)
}
fn discover_references(repository_root: &Path, prd: &str) -> Result<Vec<PathBuf>, RunError> {
    let mut paths = BTreeSet::new();
    for prefix in REFERENCE_PREFIXES {
        let mut rest = prd;
        while let Some(i) = rest.find(prefix) {
            rest = &rest[i..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')))
                .unwrap_or(rest.len());
            let reference = rest[..end].trim_end_matches('.');
            if reference.ends_with(".md") {
                paths.insert(reference.to_owned());
            }
            rest = &rest[end..];
        }
    }
    paths
        .into_iter()
        .map(|relative| {
            let path = repository_root
                .join(&relative)
                .canonicalize()
                .map_err(|e| {
                    RunError::InvalidPrdPath(format!(
                        "directly referenced document {relative} cannot be resolved: {e}"
                    ))
                })?;
            let allowed = if relative.starts_with("docs/adr/") {
                repository_root.join("docs/adr")
            } else {
                repository_root.join("docs/contracts")
            };
            if !path.starts_with(allowed) || !path.is_file() {
                return Err(RunError::InvalidPrdPath(format!(
                    "directly referenced document escapes its documentation directory: {relative}"
                )));
            }
            Ok(path)
        })
        .collect()
}
fn read_utf8(path: &Path) -> Result<String, RunError> {
    fs::read_to_string(path).map_err(|source| RunError::ReadDocument {
        path: path.into(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_only_typed_terminal_usage() {
        let mut m = Metrics::default();
        parse_event(
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":3,"output_tokens":4}}"#,
            &mut m,
        );
        assert_eq!(m.input_tokens, Some(10));
        assert_eq!(m.cached_tokens, Some(3));
        assert_eq!(m.output_tokens, Some(4));
        assert_eq!(m.model, None);
    }
    #[test]
    fn malformed_and_invalid_values_do_not_fabricate_metrics() {
        let mut m = Metrics::default();
        parse_event("not json", &mut m);
        parse_event(
            r#"{"type":"turn.completed","usage":{"input_tokens":-1,"output_tokens":1.5}}"#,
            &mut m,
        );
        assert_eq!(m, Metrics::default());
    }
    #[test]
    fn cost_uses_cached_rate_and_rounds_half_up() {
        let p = ExecutionPrice {
            input_microusd_per_million: Some(2_000_000),
            cached_input_microusd_per_million: Some(1_000_000),
            output_microusd_per_million: Some(3_000_000),
        };
        assert_eq!(
            calculate_cost(Some(10), Some(4), Some(2), Some(&p)).0,
            Some(22)
        );
    }
    #[test]
    fn missing_cached_usage_is_not_zero() {
        let p = ExecutionPrice {
            input_microusd_per_million: Some(1),
            cached_input_microusd_per_million: Some(1),
            output_microusd_per_million: Some(1),
        };
        assert_eq!(
            calculate_cost(Some(1), None, Some(1), Some(&p)).2,
            "usage_incomplete"
        );
    }
}
