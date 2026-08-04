use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use familiar_ai_agent::{CodingAgent, ExecutionRequest, ExecutionResult};

use crate::*;

pub struct StructuredReviewAdapter<'a> {
    agent: &'a dyn CodingAgent,
    repository: PathBuf,
    assignment: AgentAssignment,
    timeout_ms: u64,
}
impl<'a> StructuredReviewAdapter<'a> {
    pub fn new(
        agent: &'a dyn CodingAgent,
        repository: PathBuf,
        assignment: AgentAssignment,
        timeout_ms: u64,
    ) -> Self {
        Self {
            agent,
            repository,
            assignment,
            timeout_ms,
        }
    }
}
impl ReviewAgent for StructuredReviewAdapter<'_> {
    fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
        (self.agent.isolation_capability()
            == familiar_ai_agent::IsolationCapability::FreshProcessPerExecution)
            .then(|| AdapterIsolationEvidence {
                adapter_id: self.assignment.adapter_id.clone(),
                fresh_process_per_execution: true,
                detail:
                    "adapter guarantees a fresh OS process with no resumed implementation session; each invocation uses a newly created temporary working directory containing only the bounded request and disclosed diff, and removes it when execution returns"
                        .into(),
            })
    }
    fn review(
        &self,
        request: &ReviewRequest,
        output: &mut dyn Write,
    ) -> Result<ReviewResult, ReviewExecutionError> {
        let prompt_bytes = render_review_prompt(request)
            .map_err(|error| ReviewExecutionError::Agent(error.to_string()))?;
        let prompt = String::from_utf8(prompt_bytes)
            .map_err(|error| ReviewExecutionError::Agent(error.to_string()))?;
        let serialized_request = serde_json::to_vec(request)
            .map_err(|error| ReviewExecutionError::Agent(error.to_string()))?;
        let workspace = tempfile::Builder::new()
            .prefix("familiar-ai-review-")
            .tempdir()
            .map_err(|error| {
                ReviewExecutionError::Agent(format!(
                    "cannot create isolated review workspace: {error}"
                ))
            })?;
        std::fs::write(
            workspace.path().join("review-request.json"),
            &serialized_request,
        )
        .and_then(|()| {
            std::fs::write(
                workspace.path().join("disclosed.diff"),
                request.disclosed_diff.as_bytes(),
            )
        })
        .map_err(|error| {
            ReviewExecutionError::Agent(format!(
                "cannot populate isolated review workspace: {error}"
            ))
        })?;
        let mut captured = Vec::new();
        let observed = self
            .agent
            .execute(
                ExecutionRequest {
                    working_directory: workspace.path(),
                    denied_read_path: Some(&self.repository),
                    prompt: &prompt,
                    filesystem: familiar_ai_agent::FilesystemPolicy::ReadOnly,
                    model: self.assignment.requested_model.as_deref(),
                    timeout_ms: Some(self.timeout_ms),
                },
                &mut captured,
            )
            .map_err(|e| ReviewExecutionError::Agent(e.to_string()))?;
        output
            .write_all(&captured)
            .map_err(|e| ReviewExecutionError::Agent(e.to_string()))?;
        let text =
            String::from_utf8(captured).map_err(|e| ReviewExecutionError::Agent(e.to_string()))?;
        let mut result: ReviewResult = serde_json::from_str(text.trim()).map_err(|e| {
            ReviewExecutionError::Agent(format!("malformed structured review: {e}"))
        })?;
        result.reviewer = observation(self.assignment.clone(), &observed);
        result.usage = usage(&observed);
        Ok(result)
    }
}

pub struct CodingRemediationAdapter<'a> {
    agent: &'a dyn CodingAgent,
    worktree: PathBuf,
    assignment: AgentAssignment,
}
impl<'a> CodingRemediationAdapter<'a> {
    pub fn new(agent: &'a dyn CodingAgent, worktree: PathBuf, assignment: AgentAssignment) -> Self {
        Self {
            agent,
            worktree,
            assignment,
        }
    }
}
impl RemediationAgent for CodingRemediationAdapter<'_> {
    fn remediate(
        &self,
        request: &RemediationRequest,
        output: &mut dyn Write,
    ) -> Result<RemediationResult, RemediationExecutionError> {
        let value = serde_json::to_string(request)
            .map_err(|e| RemediationExecutionError::Agent(e.to_string()))?;
        let prompt=format!("Remediate only the supplied blocking findings within the unchanged allowed paths. Do not commit, merge, push, deploy, change architecture, or broaden scope. Configured verification commands are evidence, not permission to invent commands.\nREMEDIATION_REQUEST_JSON:\n{value}");
        let started = Utc::now();
        let timer = Instant::now();
        let result = self
            .agent
            .execute(
                ExecutionRequest {
                    working_directory: &self.worktree,
                    denied_read_path: None,
                    prompt: &prompt,
                    filesystem: familiar_ai_agent::FilesystemPolicy::WorkspaceWrite,
                    model: self.assignment.requested_model.as_deref(),
                    timeout_ms: Some(request.budget.max_duration_ms),
                },
                output,
            )
            .map_err(|e| RemediationExecutionError::Agent(e.to_string()))?;
        let ended = Utc::now();
        Ok(RemediationResult {
            remediation_id: request.remediation_id.clone(),
            implementation: observation(self.assignment.clone(), &result),
            started_at: started.to_rfc3339(),
            ended_at: ended.to_rfc3339(),
            duration_ms: u64::try_from(timer.elapsed().as_millis()).unwrap_or(u64::MAX),
            execution: ExecutionObservation {
                exit_code: result.exit_code,
                signal: result.signal,
                outcome: if result.exit_code == Some(0) {
                    "completed"
                } else {
                    "failed"
                }
                .into(),
            },
            addressed_findings: request
                .blocking_findings
                .iter()
                .map(|f| FindingResolution {
                    finding_id: f.finding_id.clone(),
                    claimed_outcome: "implementation agent attempted remediation".into(),
                    evidence: vec![],
                    reviewer_status: None,
                })
                .collect(),
            changed_files: vec![],
            resulting_diff: empty_ref(&self.worktree, &request.base_revision),
            scope_check: ScopeCheckResult {
                added: vec![],
                modified: vec![],
                deleted: vec![],
                renamed: vec![],
                disposition: ScopeDisposition::Contained,
                findings: vec![],
                policy_snapshot_hash: String::new(),
                phase: String::new(),
            },
            usage: usage(&result),
            unavailable_fields: BTreeMap::new(),
        })
    }
}
fn observation(assignment: AgentAssignment, result: &ExecutionResult) -> AgentObservation {
    let mut missing = BTreeMap::new();
    if result.agent_version.is_none() {
        missing.insert("agent_version".into(), "version_not_reported".into());
    }
    if result.model.is_none() {
        missing.insert("model".into(), "model_not_reported".into());
    }
    AgentObservation {
        assignment,
        agent_version: result.agent_version.clone(),
        reported_model: result.model.clone(),
        unavailable_fields: missing,
    }
}
fn usage(r: &ExecutionResult) -> ExecutionUsage {
    let total = r
        .input_tokens
        .zip(r.output_tokens)
        .and_then(|(a, b)| a.checked_add(b));
    let mut missing = BTreeMap::new();
    for (name, value) in [
        ("input_tokens", r.input_tokens),
        ("output_tokens", r.output_tokens),
        ("cached_tokens", r.cached_tokens),
        ("total_tokens", total),
    ] {
        if value.is_none() {
            missing.insert(name.into(), "agent_not_reported".into());
        }
    }
    missing.insert(
        "estimated_cost_microusd".into(),
        "pricing_not_available_to_adapter".into(),
    );
    ExecutionUsage {
        input_tokens: r.input_tokens,
        output_tokens: r.output_tokens,
        cached_tokens: r.cached_tokens,
        total_tokens: total,
        estimated_cost_microusd: None,
        pricing_provenance: None,
        unavailable_fields: missing,
    }
}
fn empty_ref(path: &Path, revision: &str) -> EvidenceRef {
    EvidenceRef {
        content_hash: "fnv1a64:cbf29ce484222325".into(),
        media_type: "text/x-diff".into(),
        byte_size: 0,
        repository: path.to_string_lossy().into(),
        revision: revision.into(),
        storage_ref: String::new(),
        truncated: false,
        omitted_bytes: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use familiar_ai_agent::{AgentExecutionError, FilesystemPolicy, IsolationCapability};

    use super::*;

    struct InspectingAgent {
        workspaces: Mutex<Vec<PathBuf>>,
        fail: bool,
    }

    impl CodingAgent for InspectingAgent {
        fn isolation_capability(&self) -> IsolationCapability {
            IsolationCapability::FreshProcessPerExecution
        }

        fn execute(
            &self,
            request: ExecutionRequest<'_>,
            output: &mut dyn Write,
        ) -> Result<ExecutionResult, AgentExecutionError> {
            assert_eq!(request.filesystem, FilesystemPolicy::ReadOnly);
            assert!(request.denied_read_path.is_some());
            let mut names = std::fs::read_dir(request.working_directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, ["disclosed.diff", "review-request.json"]);
            let bounded: ReviewRequest = serde_json::from_slice(
                &std::fs::read(request.working_directory.join("review-request.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(bounded.disclosed_diff, "bounded diff\n");
            assert_eq!(
                std::fs::read_to_string(request.working_directory.join("disclosed.diff")).unwrap(),
                "bounded diff\n"
            );
            assert!(std::fs::read(request.working_directory.join("unrelated.txt")).is_err());
            self.workspaces
                .lock()
                .unwrap()
                .push(request.working_directory.to_owned());
            if self.fail {
                return Err(AgentExecutionError::Timeout {
                    result: Box::default(),
                });
            }
            let result = ReviewResult {
                review_id: bounded.review_id,
                reviewer: observation(bounded.reviewer, &ExecutionResult::default()),
                started_at: "2026-08-03T00:00:00Z".into(),
                ended_at: "2026-08-03T00:00:00Z".into(),
                duration_ms: 1,
                findings: vec![],
                reviewed_manifest_hash: bounded.manifest.manifest_hash,
                usage: ExecutionUsage::default(),
                disposition: ReviewDisposition::Pending,
                unavailable_fields: BTreeMap::new(),
            };
            output
                .write_all(serde_json::to_string(&result).unwrap().as_bytes())
                .unwrap();
            Ok(ExecutionResult::default())
        }
    }

    fn assignment() -> AgentAssignment {
        AgentAssignment {
            adapter_id: "fake".into(),
            agent_id: "reviewer".into(),
            provider: Some("fake".into()),
            requested_model: Some("review-model".into()),
            role: AgentRole::Review,
            session_id: None,
        }
    }

    fn request(repository: &Path) -> ReviewRequest {
        let evidence = EvidenceRef {
            content_hash: content_hash(b"bounded diff\n"),
            media_type: "text/x-diff".into(),
            byte_size: 13,
            repository: repository.display().to_string(),
            revision: "candidate".into(),
            storage_ref: "artifact:diff".into(),
            truncated: false,
            omitted_bytes: 0,
        };
        ReviewRequest {
            review_id: "review-1".into(),
            task: ReviewTask {
                task_id: "task-1".into(),
                objective: "review bounded content".into(),
                acceptance_criteria: vec!["bounded".into()],
                base_revision: "base".into(),
                allowed_paths: vec!["src/lib.rs".into()],
                prohibited_changes: vec![],
                verification_plan_id: "verify".into(),
            },
            implementation: AgentAssignment {
                role: AgentRole::Implementation,
                ..assignment()
            },
            reviewer: assignment(),
            base_revision: "base".into(),
            candidate_revision: Some("candidate".into()),
            changed_files: vec![],
            diff: evidence,
            disclosed_diff: "bounded diff\n".into(),
            contracts: vec![],
            invariants: vec![],
            verification: vec![],
            prior_findings: vec![],
            budget: ReviewPackageBudget {
                max_bytes: 10_000,
                max_estimated_tokens: 10_000,
            },
            manifest: ReviewPackageManifest {
                manifest_hash: "manifest".into(),
                diff_hash: content_hash(b"bounded diff\n"),
                included_sources: vec!["diff".into()],
                omissions: vec![],
                total_bytes: 13,
                estimated_tokens: 4,
            },
        }
    }

    #[test]
    fn isolated_workspace_contains_only_bounded_package_and_is_cleaned_after_success() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::write(repository.path().join("unrelated.txt"), "private").unwrap();
        let agent = InspectingAgent {
            workspaces: Mutex::new(vec![]),
            fail: false,
        };
        let adapter =
            StructuredReviewAdapter::new(&agent, repository.path().to_owned(), assignment(), 1_000);
        adapter
            .review(&request(repository.path()), &mut Vec::new())
            .unwrap();
        let workspace = agent.workspaces.lock().unwrap()[0].clone();
        assert!(!workspace.exists());
        assert!(repository.path().join("unrelated.txt").exists());
    }

    #[test]
    fn isolated_workspace_is_cleaned_after_agent_failure() {
        let repository = tempfile::tempdir().unwrap();
        let agent = InspectingAgent {
            workspaces: Mutex::new(vec![]),
            fail: true,
        };
        let adapter =
            StructuredReviewAdapter::new(&agent, repository.path().to_owned(), assignment(), 1_000);
        assert!(adapter
            .review(&request(repository.path()), &mut Vec::new())
            .is_err());
        let workspace = agent.workspaces.lock().unwrap()[0].clone();
        assert!(!workspace.exists());
    }
}
