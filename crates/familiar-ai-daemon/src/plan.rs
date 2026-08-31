use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use familiar_ai_agent::{CodingAgent, ExecutionBudget, ExecutionRequest, FilesystemPolicy};
use familiar_ai_core::{
    structured_prd_metadata, validate_graph, BacklogDiscovery, Config, DiscoveredPrd,
    FilesystemBacklogDiscovery, PlannerConfig, PrdId, PrdLocation, RepositoryIdentity,
    RepositoryPath,
};
use familiar_ai_review::parse_expected_files;
use familiar_ai_storage::{
    Database, ExecutionFinalization, ExecutionHistoryRepository, ExecutionStart,
    PlannerBatchRepository,
};

const REQUIRED: [&str; 7] = [
    "Objective",
    "Scope",
    "Non-goals",
    "Acceptance Criteria",
    "Test Strategy",
    "Expected Files",
    "Definition of Done",
];

/// Plan authoring uses the scheduler's single conflict implementation.
pub fn validate_wave_width(
    root: &Path,
    prds: &[familiar_ai_core::DiscoveredPrd],
    claimed: usize,
) -> Result<crate::drive::WidthReport, String> {
    crate::drive::validate_claimed_width(root, prds, claimed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub filename: String,
    pub content: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSummary {
    pub filename: String,
    pub title: String,
    pub dependencies: Vec<String>,
    pub expected_files: usize,
    pub graph_width: usize,
    pub achievable_width: usize,
}

fn diagnostic(file: &str, line: usize, rule: &str, detail: impl std::fmt::Display) -> String {
    format!("{file}:{line}: rule={rule}: {detail}")
}

pub fn parse_agent_output(raw: &str) -> Result<Vec<Draft>, String> {
    let mut out = Vec::new();
    let mut current: Option<(String, String)> = None;
    for (index, line) in raw.lines().enumerate() {
        if let Some(name) = line
            .strip_prefix("=== ")
            .and_then(|s| s.strip_suffix(" ==="))
        {
            if let Some((filename, content)) = current.take() {
                out.push(Draft { filename, content });
            }
            if !is_filename(name) {
                return Err(diagnostic(
                    name,
                    index + 1,
                    "output.filename",
                    "expected PRD-NNN.md marker",
                ));
            }
            current = Some((name.to_owned(), String::new()));
        } else if let Some((_, content)) = current.as_mut() {
            content.push_str(line);
            content.push('\n');
        } else if !line.trim().is_empty() {
            return Err(diagnostic(
                "agent-output",
                index + 1,
                "output.marker",
                "content precedes first === PRD-NNN.md === marker",
            ));
        }
    }
    if let Some((filename, content)) = current {
        out.push(Draft { filename, content });
    }
    if out.is_empty() {
        return Err(diagnostic(
            "agent-output",
            1,
            "output.empty",
            "no PRD drafts found",
        ));
    }
    Ok(out)
}

fn is_filename(name: &str) -> bool {
    name.len() == 10
        && name.starts_with("PRD-")
        && name.ends_with(".md")
        && name[4..7].bytes().all(|b| b.is_ascii_digit())
}
fn number(name: &str) -> u64 {
    name[4..7].parse().expect("validated filename")
}

fn claimed_numbers(root: &Path, excluding: Option<&Path>) -> Result<BTreeSet<u64>, String> {
    let mut set = BTreeSet::new();
    let prds = root.join("docs/prds");
    if !prds.exists() {
        return Ok(set);
    }
    let mut dirs = vec![prds];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if excluding.is_some_and(|excluded| path.starts_with(excluded)) {
                continue;
            }
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| is_filename(n))
            {
                set.insert(number(name));
            }
        }
    }
    Ok(set)
}

fn heading_line(content: &str, heading: &str) -> Option<usize> {
    content
        .lines()
        .position(|line| line == format!("## {heading}"))
        .map(|i| i + 1)
}
fn dependencies(file: &str, content: &str) -> Result<Vec<String>, String> {
    for (i, line) in content.lines().enumerate() {
        if let Some(value) = line.strip_prefix("**Depends on:** ") {
            if value == "none" {
                return Ok(Vec::new());
            }
            let deps: Vec<String> = value.split(',').map(|s| s.trim().to_owned()).collect();
            if deps.iter().any(|d| {
                d.len() != 7
                    || !d.starts_with("PRD-")
                    || !d[4..].bytes().all(|byte| byte.is_ascii_digit())
            }) {
                return Err(diagnostic(
                    file,
                    i + 1,
                    "dependencies.syntax",
                    "dependencies must be comma-separated PRD-NNN identifiers or none",
                ));
            }
            return Ok(deps);
        }
    }
    Err(diagnostic(
        file,
        1,
        "sections.depends_on",
        "required Depends on line is missing",
    ))
}
fn dependency_line(content: &str) -> usize {
    content
        .lines()
        .position(|line| line.starts_with("**Depends on:** "))
        .map(|line| line + 1)
        .unwrap_or(1)
}
fn title(file: &str, content: &str) -> Result<String, String> {
    let first = content
        .lines()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(0);
    let prefix = format!("# {}: ", file.trim_end_matches(".md"));
    content
        .lines()
        .nth(first)
        .and_then(|l| l.strip_prefix(&prefix))
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            diagnostic(
                file,
                first + 1,
                "heading",
                "heading must match filename and contain a title",
            )
        })
}

pub fn validate_batch(
    root: &Path,
    drafts: &[Draft],
    limits: &PlannerConfig,
    excluding: Option<&Path>,
) -> Result<Vec<DraftSummary>, String> {
    if drafts.is_empty() {
        return Err(diagnostic("batch", 1, "batch.empty", "batch has no PRDs"));
    }
    if drafts.len() > limits.max_prds_per_batch {
        return Err(diagnostic(
            "batch",
            1,
            "size.max_prds_per_batch",
            format!("{} exceeds {}", drafts.len(), limits.max_prds_per_batch),
        ));
    }
    for draft in drafts {
        if !is_filename(&draft.filename) {
            return Err(diagnostic(
                &draft.filename,
                1,
                "filename",
                "expected PRD-NNN.md",
            ));
        }
    }
    let claimed = claimed_numbers(root, excluding)?;
    let start = claimed.iter().next_back().copied().unwrap_or(0) + 1;
    let batch_ids: BTreeSet<u64> = drafts.iter().map(|d| number(&d.filename)).collect();
    if batch_ids.len() != drafts.len() {
        return Err(diagnostic(
            "batch",
            1,
            "numbering.duplicate",
            "duplicate PRD number",
        ));
    }
    for (offset, draft) in drafts.iter().enumerate() {
        if draft.content.len() > limits.max_bytes_per_prd {
            return Err(diagnostic(
                &draft.filename,
                1,
                "size.max_bytes_per_prd",
                format!(
                    "{} exceeds {}",
                    draft.content.len(),
                    limits.max_bytes_per_prd
                ),
            ));
        }
        let expected = start + offset as u64;
        if number(&draft.filename) != expected {
            return Err(diagnostic(
                &draft.filename,
                1,
                "numbering.next_unused",
                format!("expected PRD-{expected:03}.md"),
            ));
        }
        if claimed.contains(&number(&draft.filename)) {
            return Err(diagnostic(
                &draft.filename,
                1,
                "numbering.unclaimed",
                "PRD number is already claimed",
            ));
        }
        title(&draft.filename, &draft.content)?;
        for section in REQUIRED {
            if heading_line(&draft.content, section).is_none() {
                return Err(diagnostic(
                    &draft.filename,
                    1,
                    "sections.required",
                    format!("missing ## {section}"),
                ));
            }
        }
        parse_expected_files(&draft.content).map_err(|e| {
            let text = e.to_string();
            let line = text
                .split("line ")
                .nth(1)
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| heading_line(&draft.content, "Expected Files").unwrap_or(1));
            diagnostic(&draft.filename, line, "expected_files.grammar", text)
        })?;
    }
    let repository = FilesystemBacklogDiscovery
        .resolve(root)
        .map_err(|e| e.to_string())?;
    let existing = FilesystemBacklogDiscovery
        .discover(&repository)
        .map_err(|e| e.to_string())?;
    validate_graph(&existing)
        .map_err(|e| diagnostic("batch", 1, "dependencies.existing_graph", e))?;
    let existing_ids: BTreeSet<u64> = existing.iter().map(|p| p.number).collect();
    let mut graph = BTreeMap::new();
    let mut summaries = Vec::new();
    for draft in drafts {
        let deps = dependencies(&draft.filename, &draft.content)?;
        let nums = deps
            .iter()
            .map(|d| d[4..].parse::<u64>().unwrap())
            .collect::<Vec<_>>();
        if let Some(unknown) = nums
            .iter()
            .find(|n| !existing_ids.contains(n) && !batch_ids.contains(n))
        {
            return Err(diagnostic(
                &draft.filename,
                dependency_line(&draft.content),
                "dependencies.unknown",
                format!("unknown PRD-{unknown:03}"),
            ));
        }
        graph.insert(number(&draft.filename), nums);
        summaries.push(DraftSummary {
            filename: draft.filename.clone(),
            title: title(&draft.filename, &draft.content)?,
            dependencies: deps,
            expected_files: parse_expected_files(&draft.content).unwrap().len(),
            graph_width: 0,
            achievable_width: 0,
        });
    }
    fn visit(
        n: u64,
        g: &BTreeMap<u64, Vec<u64>>,
        vis: &mut BTreeSet<u64>,
        stack: &mut BTreeSet<u64>,
    ) -> bool {
        if !stack.insert(n) {
            return false;
        }
        if vis.insert(n) {
            for d in g
                .get(&n)
                .into_iter()
                .flatten()
                .filter(|d| g.contains_key(d))
            {
                if !visit(*d, g, vis, stack) {
                    return false;
                }
            }
        }
        stack.remove(&n);
        true
    }
    let mut vis = BTreeSet::new();
    for n in graph.keys() {
        if !visit(*n, &graph, &mut vis, &mut BTreeSet::new()) {
            let draft = drafts
                .iter()
                .find(|draft| number(&draft.filename) == *n)
                .expect("graph keys come from drafts");
            return Err(diagnostic(
                &draft.filename,
                dependency_line(&draft.content),
                "dependencies.cycle",
                "batch dependency cycle",
            ));
        }
    }
    for draft in drafts {
        if let Some(later) = graph[&number(&draft.filename)]
            .iter()
            .find(|n| batch_ids.contains(n) && **n >= number(&draft.filename))
        {
            return Err(diagnostic(
                &draft.filename,
                dependency_line(&draft.content),
                "dependencies.order",
                format!("in-batch dependency PRD-{later:03} must precede its dependent"),
            ));
        }
    }
    let mut remaining = batch_ids.clone();
    let mut placed = BTreeSet::new();
    while !remaining.is_empty() {
        let wave_ids = remaining
            .iter()
            .copied()
            .filter(|id| {
                graph[id]
                    .iter()
                    .all(|dep| !batch_ids.contains(dep) || placed.contains(dep))
            })
            .collect::<Vec<_>>();
        let wave = wave_ids
            .iter()
            .map(|id| {
                let draft = drafts
                    .iter()
                    .find(|draft| number(&draft.filename) == *id)
                    .expect("wave ids come from drafts");
                let metadata = structured_prd_metadata(&draft.content)
                    .map_err(|error| error.to_string())?
                    .unwrap_or_else(|| familiar_ai_core::PrdMetadata {
                        contract_version: Some(1),
                        expected_files: parse_expected_files(&draft.content)
                            .expect("expected-files grammar was validated")
                            .into_iter()
                            .map(|entry| entry.normalized)
                            .collect(),
                        ..Default::default()
                    });
                Ok(DiscoveredPrd {
                    id: PrdId::new(*id),
                    number: *id,
                    path: RepositoryPath::new(format!("docs/prds/{}", draft.filename))
                        .map_err(|error| error.to_string())?,
                    location: PrdLocation::Active,
                    title: title(&draft.filename, &draft.content)?,
                    dependencies: graph[id].iter().copied().map(PrdId::new).collect(),
                    metadata,
                    content_hash: familiar_ai_review::content_hash(draft.content.as_bytes()),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let widths = validate_wave_width(root, &wave, wave.len())?;
        for id in &wave_ids {
            let summary = summaries
                .iter_mut()
                .find(|summary| number(&summary.filename) == *id)
                .expect("wave ids have summaries");
            summary.graph_width = widths.graph_width;
            summary.achievable_width = widths.achievable_width;
            remaining.remove(id);
            placed.insert(*id);
        }
    }
    Ok(summaries)
}

fn batch_id() -> String {
    format!(
        "batch-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    )
}
fn artifact(paths: &familiar_ai_core::AppPaths, id: &str, raw: &[u8]) -> Result<PathBuf, String> {
    let dir = paths.state_dir.join("planner-artifacts");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join(format!("{id}.raw"));
    fs::write(&p, raw).map_err(|e| e.to_string())?;
    Ok(p)
}

pub fn generate(
    root: &Path,
    design_docs: &[PathBuf],
    config: &Config,
    paths: &familiar_ai_core::AppPaths,
    db: &Database,
    agent: &dyn CodingAgent,
) -> Result<(String, Vec<DraftSummary>), String> {
    let limits = config.planner.as_ref().ok_or("[planner] is required")?;
    limits.validate()?;
    if design_docs.is_empty() {
        return Err("at least one design document is required".into());
    }
    let id = batch_id();
    let mut prompt=String::from("Draft a bounded dependency-ordered PRD batch. Output only blocks beginning exactly `=== PRD-NNN.md ===`. Each PRD must have Objective, Scope, Non-goals, Acceptance Criteria, Test Strategy, Expected Files, Definition of Done, and `**Depends on:** ...`. Expected Files bullets must contain one repository-relative inline-code path.\n\nDESIGN DOCUMENTS:\n");
    for path in design_docs {
        let content = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        prompt.push_str(&format!("\n--- {} ---\n{}", path.display(), content));
    }
    let mut raw = Vec::new();
    let timer = Instant::now();
    let started = Utc::now().to_rfc3339();
    let execution_id = format!("planner-{id}");
    let mut unavailable = BTreeMap::from([
        ("input_tokens".into(), "agent_did_not_report".into()),
        ("output_tokens".into(), "agent_did_not_report".into()),
        ("cached_tokens".into(), "agent_did_not_report".into()),
        ("total_tokens".into(), "agent_did_not_report".into()),
    ]);
    ExecutionHistoryRepository::new(db.conn())
        .insert_running(&ExecutionStart {
            execution_id: execution_id.clone(),
            started_at: started,
            repository: root.display().to_string(),
            worktree: root.display().to_string(),
            git_commit: None,
            prd_path: format!("planner:{id}"),
            unavailable_fields: unavailable.clone(),
        })
        .map_err(|e| e.to_string())?;
    let result = agent.execute(
        ExecutionRequest {
            working_directory: root,
            denied_read_path: None,
            prompt: &prompt,
            prompt_cache_key: None,
            filesystem: FilesystemPolicy::ReadOnly,
            model: limits.agent.model.as_deref(),
            timeout_ms: (limits.agent.max_execution_duration_ms > 0)
                .then_some(limits.agent.max_execution_duration_ms),
            budget: ExecutionBudget {
                max_cost_microusd: NonZeroU64::new(limits.agent.max_execution_cost_microusd),
                max_tokens: NonZeroU64::new(limits.agent.max_execution_tokens),
                max_duration_ms: NonZeroU64::new(limits.agent.max_execution_duration_ms),
            },
        },
        &mut raw,
    );
    let (outcome, res) = match &result {
        Ok(r) => ("succeeded", Some(r)),
        Err(e) => ("failed", Some(e.result())),
    };
    let total = res.and_then(|r| {
        r.input_tokens
            .zip(r.output_tokens)
            .map(|(a, b)| a.saturating_add(b))
    });
    if let Some(result) = res {
        for (name, value) in [
            ("input_tokens", result.input_tokens),
            ("output_tokens", result.output_tokens),
            ("cached_tokens", result.cached_tokens),
            ("total_tokens", total),
        ] {
            if value.is_some() {
                unavailable.remove(name);
            }
        }
    }
    ExecutionHistoryRepository::new(db.conn())
        .finalize(
            &execution_id,
            &ExecutionFinalization {
                ended_at: Utc::now().to_rfc3339(),
                duration_ms: timer.elapsed().as_millis() as u64,
                agent_version: res.and_then(|r| r.agent_version.clone()),
                model: res.and_then(|r| r.model.clone()),
                input_tokens: res.and_then(|r| r.input_tokens),
                output_tokens: res.and_then(|r| r.output_tokens),
                cached_tokens: res.and_then(|r| r.cached_tokens),
                total_tokens: total,
                outcome: outcome.into(),
                unavailable_fields: unavailable,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
    let raw_artifact = artifact(paths, &id, &raw)?;
    result.map_err(|e| {
        format!(
            "planner agent failed: {e}; raw artifact={}",
            raw_artifact.display()
        )
    })?;
    let drafts = std::str::from_utf8(&raw)
        .map_err(|e| diagnostic("agent-output", 1, "output.utf8", e))
        .and_then(parse_agent_output)
        .and_then(|d| validate_batch(root, &d, limits, None).map(|s| (d, s)));
    let (drafts, summaries) =
        drafts.map_err(|e| format!("{e}; raw artifact={}", raw_artifact.display()))?;
    let proposed = root.join("docs/prds/proposed");
    fs::create_dir_all(&proposed).map_err(|e| e.to_string())?;
    let staging = proposed.join(format!(".{id}.tmp"));
    let dir = proposed.join(&id);
    fs::create_dir(&staging).map_err(|e| e.to_string())?;
    for d in drafts {
        if let Err(error) = fs::write(staging.join(d.filename), d.content) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.to_string());
        }
    }
    fs::rename(&staging, &dir).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        e.to_string()
    })?;
    Ok((id, summaries))
}

fn human(actor: &str) -> Result<(), String> {
    if matches!(actor.strip_prefix("human:"),Some(v) if !v.trim().is_empty()&&!v.chars().any(char::is_control))
    {
        Ok(())
    } else {
        Err("human authority required: actor must be human:<identity>".into())
    }
}
fn proposal(root: &Path, id: &str) -> Result<PathBuf, String> {
    let suffix = id.strip_prefix("batch-").ok_or_else(|| {
        "batch id must match batch-<digits> and contain no path components".to_owned()
    })?;
    if suffix.is_empty() || suffix.len() > 32 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("batch id must match batch-<digits> and contain no path components".into());
    }
    let parent = root.join("docs/prds/proposed");
    let path = parent.join(id);
    if path.parent() != Some(parent.as_path()) {
        return Err("batch id must resolve to a direct proposal child".into());
    }
    Ok(path)
}
fn read_batch(dir: &Path) -> Result<Vec<Draft>, String> {
    let mut paths = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .map(|e| e.map(|v| v.path()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| {
            let filename = p.file_name().unwrap().to_string_lossy().into_owned();
            let content = fs::read_to_string(&p).map_err(|e| e.to_string())?;
            Ok(Draft { filename, content })
        })
        .collect()
}

pub fn approve(
    root: &Path,
    id: &str,
    actor: &str,
    limits: &PlannerConfig,
    repository: &RepositoryIdentity,
    db: &mut Database,
) -> Result<Vec<DraftSummary>, String> {
    human(actor)?;
    let dir = proposal(root, id)?;
    let drafts = read_batch(&dir)?;
    let summaries = validate_batch(root, &drafts, limits, Some(&dir))?;
    let target = root.join("docs/prds");
    for d in &drafts {
        let to = target.join(&d.filename);
        if to.exists() {
            return Err(diagnostic(
                &d.filename,
                1,
                "approval.unclaimed",
                "number was claimed during review",
            ));
        }
    }
    let hashes = drafts
        .iter()
        .map(|d| {
            (
                d.filename.clone(),
                familiar_ai_review::content_hash(d.content.as_bytes()),
            )
        })
        .collect::<Vec<_>>();
    let transaction = db.conn_mut().transaction().map_err(|e| e.to_string())?;
    PlannerBatchRepository::new(&transaction)
        .record(id, &repository.key, "approved", actor, None, &hashes)
        .map_err(|e| e.to_string())?;
    let mut moved = Vec::new();
    for draft in &drafts {
        let from = dir.join(&draft.filename);
        let to = target.join(&draft.filename);
        if let Err(error) = fs::rename(&from, &to) {
            for (from, to) in moved.iter().rev() {
                let _ = fs::rename(to, from);
            }
            return Err(error.to_string());
        }
        moved.push((from, to));
    }
    if let Err(error) = transaction.commit() {
        for (from, to) in moved.iter().rev() {
            let _ = fs::rename(to, from);
        }
        return Err(error.to_string());
    }
    // The durable approval and complete file set are authoritative. An empty
    // proposal directory is harmless and can be cleaned on a later retry.
    let _ = fs::remove_dir(&dir);
    Ok(summaries)
}
pub fn reject(
    root: &Path,
    id: &str,
    actor: &str,
    reason: &str,
    repository: &RepositoryIdentity,
    db: &mut Database,
) -> Result<(), String> {
    human(actor)?;
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().any(char::is_control) {
        return Err("rejection reason must be non-empty and contain no control characters".into());
    }
    let dir = proposal(root, id)?;
    if !dir.is_dir() {
        return Err(format!("proposal batch not found: {id}"));
    }
    let rejected = dir
        .parent()
        .expect("validated proposal has a parent")
        .join(format!(".{id}.rejected"));
    if rejected.exists() {
        return Err(format!("rejection staging path already exists: {id}"));
    }
    fs::rename(&dir, &rejected).map_err(|e| e.to_string())?;
    let transaction = db.conn_mut().transaction().map_err(|e| {
        let _ = fs::rename(&rejected, &dir);
        e.to_string()
    })?;
    if let Err(error) = PlannerBatchRepository::new(&transaction).record(
        id,
        &repository.key,
        "rejected",
        actor,
        Some(reason),
        &[],
    ) {
        drop(transaction);
        let _ = fs::rename(&rejected, &dir);
        return Err(error.to_string());
    }
    if let Err(error) = transaction.commit() {
        let _ = fs::rename(&rejected, &dir);
        return Err(error.to_string());
    }
    // Once the decision is committed the hidden staging directory is no
    // longer approvable. Cleanup failure is safe and recoverable.
    let _ = fs::remove_dir_all(rejected);
    Ok(())
}

pub fn print_summary(id: &str, s: &[DraftSummary]) {
    println!("Batch {id}");
    for d in s {
        println!(
            "{}\t{}\tdeps={}\texpected-files={}\tgraph-width={}\tachievable-width={}",
            d.filename,
            d.title,
            if d.dependencies.is_empty() {
                "none".into()
            } else {
                d.dependencies.join(",")
            },
            d.expected_files,
            d.graph_width,
            d.achievable_width
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_agent::{AgentExecutionError, ExecutionResult};
    use std::io;
    use std::process::Command;

    struct FixtureAgent(String);
    impl CodingAgent for FixtureAgent {
        fn execute(
            &self,
            _: ExecutionRequest<'_>,
            output: &mut dyn io::Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            output.write_all(self.0.as_bytes()).unwrap();
            Ok(ExecutionResult {
                exit_code: Some(0),
                ..Default::default()
            })
        }
    }

    fn repository() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("docs/prds/proposed")).unwrap();
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success());
        root
    }

    fn limits() -> PlannerConfig {
        PlannerConfig {
            agent: Default::default(),
            max_prds_per_batch: 4,
            max_bytes_per_prd: 9999,
        }
    }

    fn paths(root: &Path) -> familiar_ai_core::AppPaths {
        let app = root.join("app");
        familiar_ai_core::AppPaths {
            config_dir: app.join("config"),
            data_dir: app.join("data"),
            state_dir: app.join("state"),
            runtime_dir: app.join("runtime"),
            log_dir: app.join("logs"),
            socket_path: app.join("runtime/familiar-ai.sock"),
            pid_path: app.join("runtime/familiar-ai.pid"),
        }
    }
    fn valid(n: u64, deps: &str) -> Draft {
        Draft{filename:format!("PRD-{n:03}.md"),content:format!("# PRD-{n:03}: Test\n\n**Depends on:** {deps}\n\n## Objective\nx\n## Scope\nx\n## Non-goals\nx\n## Acceptance Criteria\nx\n## Test Strategy\nx\n## Expected Files\n\n- `src/x.rs`\n\n## Definition of Done\nx\n")}
    }
    #[test]
    fn parses_marked_output() {
        assert_eq!(
            parse_agent_output("=== PRD-001.md ===\nbody\n").unwrap()[0].content,
            "body\n"
        )
    }
    #[test]
    fn human_gate() {
        assert!(human("agent:x").is_err());
        assert!(human("human:a").is_ok())
    }
    #[test]
    fn detects_expected_files_line() {
        let mut d = valid(1, "none");
        d.content = d.content.replace("`src/x.rs`", "`../x`");
        let limits = PlannerConfig {
            agent: Default::default(),
            max_prds_per_batch: 2,
            max_bytes_per_prd: 9999,
        };
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("docs/prds")).unwrap();
        let e = validate_batch(root.path(), &[d], &limits, None).unwrap_err();
        assert!(e.contains("expected_files.grammar"));
        assert!(e.contains(":17:"), "{e}")
    }

    #[test]
    fn authored_wave_rejects_overclaimed_width_and_names_conflicting_pair() {
        let root = repository();
        let error = validate_batch(
            root.path(),
            &[valid(1, "none"), valid(2, "none")],
            &limits(),
            None,
        )
        .unwrap_err();
        assert!(error.contains("claimed width 2 exceeds achievable width 1"));
        assert!(error.contains("PRD-1<->PRD-2"), "{error}");
    }

    #[test]
    fn rejects_noncanonical_dependencies_and_batch_ids() {
        assert!(dependencies("PRD-001.md", "**Depends on:** PRD-1")
            .unwrap_err()
            .contains("dependencies.syntax"));
        let root = repository();
        let victim = root.path().join("victim");
        fs::create_dir(&victim).unwrap();
        let repository = FilesystemBacklogDiscovery.resolve(root.path()).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        assert!(reject(
            root.path(),
            "../../victim",
            "human:test",
            "no",
            &repository,
            &mut db,
        )
        .is_err());
        assert!(victim.is_dir());
    }

    #[test]
    fn validates_bad_numbering_and_cycles() {
        let root = repository();
        let error = validate_batch(root.path(), &[valid(2, "none")], &limits(), None).unwrap_err();
        assert!(error.contains("numbering.next_unused"));
        let error = validate_batch(
            root.path(),
            &[valid(1, "PRD-002"), valid(2, "PRD-001")],
            &limits(),
            None,
        )
        .unwrap_err();
        assert!(error.contains("dependencies.cycle") || error.contains("dependencies.order"));
    }

    #[test]
    fn successful_generation_retains_raw_output_and_is_not_discoverable() {
        let root = repository();
        let config = Config {
            planner: Some(limits()),
            ..Default::default()
        };
        let paths = paths(root.path());
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let design = root.path().join("design.md");
        fs::write(&design, "build it").unwrap();
        let raw = format!("=== PRD-001.md ===\n{}", valid(1, "none").content);
        let (id, summaries) = generate(
            root.path(),
            &[design],
            &config,
            &paths,
            &db,
            &FixtureAgent(raw.clone()),
        )
        .unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(
            fs::read(
                paths
                    .state_dir
                    .join("planner-artifacts")
                    .join(format!("{id}.raw"))
            )
            .unwrap(),
            raw.as_bytes()
        );
        assert!(root
            .path()
            .join("docs/prds/proposed")
            .join(&id)
            .join("PRD-001.md")
            .is_file());
        let repository = FilesystemBacklogDiscovery.resolve(root.path()).unwrap();
        assert!(FilesystemBacklogDiscovery
            .discover(&repository)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn approval_revalidates_edits_and_records_complete_batch() {
        let root = repository();
        let repository = FilesystemBacklogDiscovery.resolve(root.path()).unwrap();
        let dir = root.path().join("docs/prds/proposed/batch-1");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("PRD-999.md"), valid(999, "none").content).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let error = approve(
            root.path(),
            "batch-1",
            "human:test",
            &limits(),
            &repository,
            &mut db,
        )
        .unwrap_err();
        assert!(error.contains("numbering.next_unused"));
        assert!(dir.join("PRD-999.md").is_file());

        fs::remove_file(dir.join("PRD-999.md")).unwrap();
        fs::write(dir.join("PRD-001.md"), valid(1, "none").content).unwrap();
        approve(
            root.path(),
            "batch-1",
            "human:test",
            &limits(),
            &repository,
            &mut db,
        )
        .unwrap();
        assert!(root.path().join("docs/prds/PRD-001.md").is_file());
        let record = PlannerBatchRepository::new(db.conn())
            .get("batch-1")
            .unwrap()
            .unwrap();
        assert_eq!(record.status, "approved");
        assert_eq!(record.file_hashes.len(), 1);
    }

    #[test]
    fn rejection_hides_proposal_before_recording_terminal_state() {
        let root = repository();
        let repository = FilesystemBacklogDiscovery.resolve(root.path()).unwrap();
        let dir = root.path().join("docs/prds/proposed/batch-2");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("PRD-001.md"), valid(1, "none").content).unwrap();
        let mut db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        reject(
            root.path(),
            "batch-2",
            "human:test",
            "superseded",
            &repository,
            &mut db,
        )
        .unwrap();
        assert!(!dir.exists());
        assert_eq!(
            PlannerBatchRepository::new(db.conn())
                .get("batch-2")
                .unwrap()
                .unwrap()
                .status,
            "rejected"
        );
    }
}
