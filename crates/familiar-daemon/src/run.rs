use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use chrono::Utc;
use familiar_agent::{AgentExecutionError, CodingAgent, ExecutionRequest, ExecutionResult};
use familiar_context::{
    ContextCompilationError, ContextCompiler, ContextRequest, ExecutionContext,
};
use familiar_core::{AppPaths, Config, ExecutionPrice};
use familiar_storage::{
    Database, ExecutionFinalization, ExecutionHistoryRepository, ExecutionStart,
};

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
    Context(ContextCompilationError),
    Config(String),
    Storage(String),
    Agent(AgentExecutionError),
    HistoryFinalize {
        execution_id: String,
        detail: String,
    },
}
impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(e) => write!(f, "cannot resolve current directory: {e}"),
            Self::Context(error) => error.fmt(f),
            Self::Config(m) => write!(f, "configuration failed: {m}"),
            Self::Storage(m) => write!(f, "execution history failed: {m}"),
            Self::Agent(error) => error.fmt(f),
            Self::HistoryFinalize {
                execution_id,
                detail,
            } => write!(f, "history_finalize_failed for {execution_id}: {detail}"),
        }
    }
}
impl std::error::Error for RunError {}

pub fn execute(prd_path: &Path, agent: &dyn CodingAgent) -> Result<ExecutionResult, RunError> {
    let paths = AppPaths::new();
    let config = Config::load(Some(&paths.config_dir.join("config.toml")))
        .map_err(|e| RunError::Config(e.to_string()))?;
    execute_with_config(prd_path, agent, &config, &paths)
}

pub fn execute_with_config(
    prd_path: &Path,
    agent: &dyn CodingAgent,
    config: &Config,
    paths: &AppPaths,
) -> Result<ExecutionResult, RunError> {
    let current = env::current_dir().map_err(RunError::CurrentDirectory)?;
    let context = ContextCompiler::new()
        .compile(ContextRequest {
            repository: &current,
            prd: prd_path,
        })
        .map_err(RunError::Context)?;
    let prompt = render_prompt(&context);
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
    if context.repository.git_commit.is_none() {
        unavailable.insert("git_commit".into(), "git_unavailable".into());
    }
    ExecutionHistoryRepository::new(db.conn())
        .insert_running(&ExecutionStart {
            execution_id: id.clone(),
            started_at,
            repository: slash(&context.repository.repository),
            worktree: slash(&context.repository.worktree),
            git_commit: context.repository.git_commit.clone(),
            prd_path: context.prd.path.clone(),
            unavailable_fields: unavailable.clone(),
        })
        .map_err(|e| RunError::Storage(e.to_string()))?;

    let execution = agent.execute(
        ExecutionRequest {
            working_directory: &context.repository.worktree,
            prompt: &prompt,
        },
        &mut io::stdout(),
    );
    let (result, outcome) = match &execution {
        Ok(result) => (result, outcome(result)),
        Err(AgentExecutionError::Launch { result, .. }) => (result.as_ref(), "launch_failed"),
        Err(AgentExecutionError::Input { result, .. }) => (result.as_ref(), "input_failed"),
        Err(AgentExecutionError::Wait { result, .. }) => (result.as_ref(), "failed"),
        Err(AgentExecutionError::Output { result, .. }) => (result.as_ref(), outcome(result)),
    };
    if result.agent_version.is_none() {
        unavailable.insert("agent_version".into(), "version_probe_failed".into());
    }
    let finalization = terminal(&timer, result, outcome, unavailable, config);
    finalize(&db, &id, &finalization)?;
    execution.map_err(RunError::Agent)
}

fn outcome(result: &ExecutionResult) -> &'static str {
    match result.exit_code {
        Some(0) => "succeeded",
        Some(_) => "failed",
        None if result.signal.is_some() => "signaled",
        None => "failed",
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

fn terminal(
    timer: &Instant,
    result: &ExecutionResult,
    outcome: &str,
    mut missing: BTreeMap<String, String>,
    config: &Config,
) -> ExecutionFinalization {
    for field in ["ended_at", "duration_ms", "signal"] {
        missing.remove(field);
    }
    if result.agent_version.is_some() {
        missing.remove("agent_version");
    }
    if result.exit_code.is_some() {
        missing.remove("exit_code");
    } else {
        missing.insert("exit_code".into(), "no_normal_exit_code".into());
    }
    if result.model.is_none() {
        missing.insert("model".into(), "agent_not_reported".into());
    } else {
        missing.remove("model");
    }
    for (name, value) in [
        ("input_tokens", result.input_tokens),
        ("output_tokens", result.output_tokens),
        ("cached_tokens", result.cached_tokens),
    ] {
        if value.is_none() {
            missing.insert(name.into(), "usage_not_reported".into());
        } else {
            missing.remove(name);
        }
    }
    let total = match (result.input_tokens, result.output_tokens) {
        (Some(a), Some(b)) => a.checked_add(b).filter(|value| *value <= i64::MAX as u64),
        _ => None,
    };
    if total.is_none() {
        missing.insert(
            "total_tokens".into(),
            if result.input_tokens.is_some() && result.output_tokens.is_some() {
                "arithmetic_overflow"
            } else {
                "usage_incomplete"
            }
            .into(),
        );
    } else {
        missing.remove("total_tokens");
    }
    let price = result
        .model
        .as_ref()
        .and_then(|m| config.execution_history.pricing.get(m));
    let (cost, rates, reason) = calculate_cost(
        result.input_tokens,
        result.cached_tokens,
        result.output_tokens,
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
        agent_version: result.agent_version.clone(),
        model: result.model.clone(),
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
        cached_tokens: result.cached_tokens,
        total_tokens: total,
        estimated_cost_microusd: cost,
        input_rate: rates.0,
        cached_input_rate: rates.1,
        output_rate: rates.2,
        outcome: outcome.into(),
        exit_code: result.exit_code,
        signal: result.signal,
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
pub fn build_prompt(repository_root: &Path, supplied_path: &Path) -> Result<String, RunError> {
    let supplied = if supplied_path.is_absolute() {
        supplied_path.to_owned()
    } else {
        env::current_dir()
            .map_err(RunError::CurrentDirectory)?
            .join(supplied_path)
    };
    let context = ContextCompiler::new()
        .compile(ContextRequest {
            repository: repository_root,
            prd: &supplied,
        })
        .map_err(RunError::Context)?;
    Ok(render_prompt(&context))
}

fn render_prompt(context: &ExecutionContext) -> String {
    let mut prompt = String::new();
    prompt.push_str("# Familiar execution request\n\nImplement the PRD below in this repository.\n\n## Fixed execution constraints\n\n");
    prompt.push_str(EXECUTION_CONSTRAINTS);
    prompt.push_str("\n\n## PRD: ");
    prompt.push_str(&context.prd.path);
    prompt.push_str("\n\n");
    prompt.push_str(&context.prd.content);
    prompt.push('\n');
    for document in &context.documents {
        prompt.push_str("\n## Directly referenced document: ");
        prompt.push_str(&document.path);
        prompt.push_str("\n\n");
        prompt.push_str(&document.content);
        prompt.push('\n');
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_context::{ContextDocument, DocumentKind, InclusionReason, RepositoryContext};
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeAgent {
        request: Mutex<Option<(PathBuf, String)>>,
        result: ExecutionResult,
    }

    impl CodingAgent for FakeAgent {
        fn execute(
            &self,
            request: ExecutionRequest<'_>,
            _output: &mut dyn io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            *self.request.lock().unwrap() = Some((
                request.working_directory.to_owned(),
                request.prompt.to_owned(),
            ));
            Ok(self.result.clone())
        }
    }

    fn legacy_prompt(repository: &Path, prd_path: &Path) -> String {
        let prd = fs::read_to_string(prd_path).unwrap();
        let mut references = BTreeSet::new();
        for prefix in ["docs/adr/", "docs/contracts/"] {
            let mut rest = prd.as_str();
            while let Some(index) = rest.find(prefix) {
                rest = &rest[index..];
                let end = rest
                    .find(|character: char| {
                        !(character.is_ascii_alphanumeric()
                            || matches!(character, '/' | '-' | '_' | '.'))
                    })
                    .unwrap_or(rest.len());
                let reference = rest[..end].trim_end_matches('.');
                if reference.ends_with(".md") {
                    references.insert(reference.to_owned());
                }
                rest = &rest[end..];
            }
        }
        let identity = prd_path.strip_prefix(repository).unwrap().to_string_lossy();
        let mut prompt = format!(
            "# Familiar execution request\n\nImplement the PRD below in this repository.\n\n## Fixed execution constraints\n\n{EXECUTION_CONSTRAINTS}\n\n## PRD: {identity}\n\n{prd}\n"
        );
        for reference in references {
            prompt.push_str(&format!(
                "\n## Directly referenced document: {reference}\n\n{}\n",
                fs::read_to_string(repository.join(&reference)).unwrap()
            ));
        }
        prompt
    }

    #[test]
    fn prompt_bytes_match_legacy_renderer() {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let prd = repository.join("docs/prds/PRD-003.md");
        assert_eq!(
            build_prompt(&repository, &prd).unwrap().as_bytes(),
            legacy_prompt(&repository, &prd).as_bytes()
        );
    }

    #[test]
    fn supporting_document_uses_existing_rendering_form() {
        let context = ExecutionContext {
            repository: RepositoryContext {
                repository: PathBuf::from("repo"),
                worktree: PathBuf::from("worktree"),
                git_commit: None,
            },
            prd: ContextDocument {
                path: "docs/prds/work.md".into(),
                kind: DocumentKind::Prd,
                content: "prd".into(),
                inclusion: InclusionReason::RequestedPrd,
                estimated_tokens: 1,
            },
            documents: vec![ContextDocument {
                path: "docs/supporting/input.md".into(),
                kind: DocumentKind::Supporting,
                content: "support".into(),
                inclusion: InclusionReason::DirectReference {
                    referenced_by: "docs/prds/work.md".into(),
                },
                estimated_tokens: 2,
            }],
            estimated_tokens: 3,
        };
        assert!(render_prompt(&context)
            .contains("\n## Directly referenced document: docs/supporting/input.md\n\nsupport\n"));
    }

    #[test]
    fn orchestration_uses_neutral_agent_and_preserves_history_mapping() {
        let temp = tempfile::tempdir().unwrap();
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        let mut config = Config::default();
        let database_path = temp.path().join("history.db");
        config.database.path = Some(database_path.clone());
        let paths = AppPaths {
            config_dir: temp.path().join("config"),
            data_dir: temp.path().join("data"),
            state_dir: temp.path().join("state"),
            runtime_dir: temp.path().join("runtime"),
            log_dir: temp.path().join("log"),
            socket_path: temp.path().join("runtime/socket"),
            pid_path: temp.path().join("state/pid"),
        };
        let agent = FakeAgent {
            request: Mutex::new(None),
            result: ExecutionResult {
                agent_version: Some("test-agent 1".into()),
                model: Some("priced-model".into()),
                input_tokens: Some(10),
                output_tokens: Some(4),
                cached_tokens: Some(3),
                exit_code: Some(23),
                signal: None,
            },
        };

        let result = execute_with_config(
            &repository.join("docs/prds/PRD-004.md"),
            &agent,
            &config,
            &paths,
        )
        .unwrap();
        assert_eq!(result.exit_code, Some(23));
        let captured = agent.request.lock().unwrap();
        let (working_directory, prompt) = captured.as_ref().unwrap();
        assert_eq!(working_directory, &repository);
        assert_eq!(
            prompt,
            &build_prompt(&repository, &repository.join("docs/prds/PRD-004.md")).unwrap()
        );

        let database = Database::open(&database_path).unwrap();
        let rows = ExecutionHistoryRepository::new(database.conn())
            .recent(1)
            .unwrap();
        assert_eq!(rows[0].outcome, "failed");
        assert_eq!(rows[0].exit_code, Some(23));
        assert_eq!(rows[0].input_tokens, Some(10));
        assert_eq!(rows[0].total_tokens, Some(14));
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
