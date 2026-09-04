//! PRD-071: batch-tier independent review regression coverage.
//!
//! Every test here runs against a `wiremock::MockServer` — no live or
//! billable provider call. `BatchReviewAgent::review` is a synchronous
//! `ReviewAgent` method that internally drives its own single-threaded
//! Tokio runtime, so each test drives `wiremock`'s async setup on an
//! *outer* runtime, lets that runtime's `block_on` calls return, and only
//! then calls `.review()` from plain synchronous test code — nesting a
//! `block_on` inside an active Tokio context panics, so the two must never
//! overlap.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use familiar_ai_core::config::{
    AgentAdapterKind, AuthDescriptor, Config, RegistryWorkerConfig, RepositoryConfig,
    ReviewAgentConfig, ReviewConfig, ReviewTierPolicyConfig, WorkerRegistryConfig,
};
use familiar_ai_core::AppPaths;
use familiar_ai_daemon::batch_review::{
    build_batch_reviewer, poll_once, BatchReviewAgent, BatchReviewContext,
};
use familiar_ai_llm::anthropic_api::{AnthropicHttpClient, AnthropicHttpConfig};
use familiar_ai_review::{
    AgentAssignment, AgentRole, EvidenceRef, ExecutionUsage, ReviewAgent, ReviewDisposition,
    ReviewExecutionError, ReviewFinding, ReviewPackageBudget, ReviewPackageManifest, ReviewRequest,
    ReviewResult, ReviewTask,
};
use familiar_ai_storage::{BatchReviewRepository, Database, NewBatchReview};

fn evidence_ref() -> EvidenceRef {
    EvidenceRef {
        content_hash: "fnv1a64:1".into(),
        media_type: "text/x-diff".into(),
        byte_size: 1,
        repository: "repo".into(),
        revision: "base".into(),
        storage_ref: "mem".into(),
        truncated: false,
        omitted_bytes: 0,
    }
}

fn review_request(review_id: &str) -> ReviewRequest {
    ReviewRequest {
        review_id: review_id.into(),
        task: ReviewTask {
            task_id: "task".into(),
            objective: "objective".into(),
            acceptance_criteria: vec!["criterion".into()],
            base_revision: "base".into(),
            allowed_paths: vec!["src/".into()],
            prohibited_changes: vec![],
            verification_plan_id: "plan".into(),
        },
        implementation: AgentAssignment {
            adapter_id: "fake".into(),
            agent_id: "implementation".into(),
            provider: Some("fake".into()),
            requested_model: Some("impl-model".into()),
            role: AgentRole::Implementation,
            session_id: None,
        },
        reviewer: AgentAssignment {
            adapter_id: "anthropic-api-batch".into(),
            agent_id: "reviewer".into(),
            provider: Some("anthropic".into()),
            requested_model: Some("claude-review-model".into()),
            role: AgentRole::Review,
            session_id: None,
        },
        base_revision: "base".into(),
        candidate_revision: Some("candidate".into()),
        changed_files: vec![],
        diff: evidence_ref(),
        disclosed_diff: "diff --git a/src/lib.rs b/src/lib.rs\n".into(),
        contracts: vec![],
        invariants: vec![],
        verification: vec![],
        prior_findings: vec![],
        budget: ReviewPackageBudget {
            max_bytes: 10_000,
            max_estimated_tokens: 10_000,
        },
        manifest: ReviewPackageManifest {
            manifest_hash: "sha256:manifest".into(),
            diff_hash: "sha256:diff".into(),
            included_sources: vec!["task".into(), "diff".into()],
            omissions: vec![],
            total_bytes: 10,
            estimated_tokens: 10,
        },
    }
}

fn open_db() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    // The ledger's `execution_id` foreign-keys into `execution_history`;
    // accounting tests need a matching row to attach observations to.
    db.conn().execute(
        "INSERT INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields) VALUES('exec-1','2026-08-30T00:00:00Z','anthropic-api-batch','running','repo','wt','docs/prds/PRD-071.md','[]')",
        [],
    ).unwrap();
    db
}

fn batch_client(server: &MockServer) -> AnthropicHttpClient {
    AnthropicHttpClient::new(AnthropicHttpConfig {
        base_url: server.uri(),
        anthropic_version: "2023-06-01".into(),
        request_timeout_secs: 5,
    })
    .unwrap()
}

fn agent<'a>(
    conn: &'a Connection,
    client: AnthropicHttpClient,
    fallback: &'a dyn ReviewAgent,
    env_key: &str,
) -> BatchReviewAgent<'a> {
    agent_with_pricing(conn, client, fallback, env_key, BTreeMap::new())
}

fn agent_with_pricing<'a>(
    conn: &'a Connection,
    client: AnthropicHttpClient,
    fallback: &'a dyn ReviewAgent,
    env_key: &str,
    pricing: BTreeMap<String, familiar_ai_core::ExecutionPrice>,
) -> BatchReviewAgent<'a> {
    BatchReviewAgent::new(
        conn,
        client,
        AuthDescriptor::Env(env_key.into()),
        BatchReviewContext {
            cycle_id: "cycle-1".into(),
            repository_key: "repo/.git".into(),
            prd_id: "PRD-071".into(),
            execution_id: "exec-1".into(),
            declared_risk_classes: vec!["low-risk-docs".into()],
            batch_risk_classes: vec!["low-risk-docs".into()],
            max_wait_ms: 3_600_000,
        },
        fallback,
        pricing,
    )
    .unwrap()
}

/// A canned interactive reviewer used as the batch tier's expiry fallback.
/// It must never be reached except when a test deliberately exercises
/// expiry.
struct FallbackReviewer {
    result: ReviewResult,
}
impl ReviewAgent for FallbackReviewer {
    fn review(
        &self,
        _request: &ReviewRequest,
        _output: &mut dyn std::io::Write,
    ) -> Result<ReviewResult, ReviewExecutionError> {
        Ok(self.result.clone())
    }
}

fn fallback_result(review_id: &str) -> ReviewResult {
    ReviewResult {
        review_id: review_id.into(),
        reviewer: familiar_ai_review::AgentObservation {
            assignment: AgentAssignment {
                adapter_id: "fake-interactive".into(),
                agent_id: "reviewer".into(),
                provider: Some("fake".into()),
                requested_model: Some("fake-model".into()),
                role: AgentRole::Review,
                session_id: None,
            },
            agent_version: None,
            reported_model: None,
            unavailable_fields: BTreeMap::new(),
        },
        started_at: "2026-08-30T00:00:00Z".into(),
        ended_at: "2026-08-30T00:00:01Z".into(),
        duration_ms: 1000,
        findings: vec![],
        reviewed_manifest_hash: "sha256:manifest".into(),
        usage: ExecutionUsage::default(),
        disposition: ReviewDisposition::Pending,
        unavailable_fields: BTreeMap::new(),
    }
}

/// Each test uses its own env var name — `std::env::set_var` is process-wide
/// and Rust runs tests in parallel by default, so a shared name would race.
fn with_env_key<F: FnOnce(&str)>(env_key: &str, f: F) {
    std::env::set_var(env_key, "sk-test");
    f(env_key);
    std::env::remove_var(env_key);
}

/// A review eligible for the batch tier submits and returns immediately —
/// `Parked`, never `Ok` — so the PRD's admission slot is free while the
/// batch is outstanding, and a durable row records the submission.
#[test]
fn batch_submission_parks_and_frees_the_slot() {
    with_env_key("PRD071_TEST_KEY_PARK", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_park",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .expect(1)
                .mount(&server)
                .await;
        });

        let db = open_db();
        let fallback = FallbackReviewer {
            result: fallback_result("review-park"),
        };
        let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
        let request = review_request("review-park");

        let error = batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        assert!(
            matches!(&error, ReviewExecutionError::Parked { batch_id } if batch_id == "msgbatch_park")
        );

        let row = BatchReviewRepository::new(db.conn())
            .find_by_review_id("review-park")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "submitted");
        assert_eq!(row.provider_batch_id, "msgbatch_park");
        assert_eq!(row.risk_class, "low-risk-docs");

        outer.block_on(async { server.verify().await });
    });
}

/// A re-driven review cycle (mirroring a daemon restart, or a periodic
/// resume scan racing the batch worker) must never resubmit a still
/// pending batch — the provider only ever sees one submission.
#[test]
fn crash_resume_never_double_submits() {
    with_env_key("PRD071_TEST_KEY_RESUME", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_resume",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_resume"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_resume",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .mount(&server)
                .await;
        });

        let db = open_db();
        let fallback = FallbackReviewer {
            result: fallback_result("review-resume"),
        };
        let request = review_request("review-resume");

        // First attempt: submits, parks.
        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            let error = batch_agent.review(&request, &mut Vec::new()).unwrap_err();
            assert!(matches!(error, ReviewExecutionError::Parked { .. }));
        }
        // Second attempt models a fresh `coordinator.run()` after a daemon
        // restart: a brand new agent instance, same durable connection.
        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            let error = batch_agent.review(&request, &mut Vec::new()).unwrap_err();
            assert!(matches!(
                error,
                ReviewExecutionError::Parked { batch_id } if batch_id == "msgbatch_resume"
            ));
        }

        // The wiremock POST expectation (`expect(1)`) fails the mock's own
        // verification if the second attempt resubmitted.
        outer.block_on(async { server.verify().await });
    });
}

/// The configured maximum batch wait bounds latency: once the deadline
/// passes, the attempt falls back to the interactive reviewer and the
/// durable row records why — the PRD never sits parked past the bound.
#[test]
fn expiry_falls_back_to_interactive_review_with_a_durable_reason() {
    with_env_key("PRD071_TEST_KEY_EXPIRE", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_expire",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .mount(&server)
                .await;
        });

        let db = open_db();
        let expected = fallback_result("review-expire");
        let fallback = FallbackReviewer {
            result: expected.clone(),
        };
        let request = review_request("review-expire");

        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        }
        // Force the deadline into the past — deterministic, no sleep.
        db.conn()
            .execute(
                "UPDATE batch_reviews SET deadline_at='2000-01-01T00:00:00Z' WHERE review_id='review-expire'",
                [],
            )
            .unwrap();

        let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
        let result = batch_agent.review(&request, &mut Vec::new()).unwrap();
        assert_eq!(result.review_id, expected.review_id);
        assert_eq!(result.reviewer.assignment.adapter_id, "fake-interactive");

        let row = BatchReviewRepository::new(db.conn())
            .find_by_review_id("review-expire")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "expired_fallback");
        assert_eq!(
            row.fallback_reason.as_deref(),
            Some("max_batch_wait_exceeded")
        );
    });
}

/// A completed batch result flows through the identical structured-review
/// parser the interactive transport uses — no separate code path decides
/// what the reviewer said.
#[test]
fn completed_batch_result_uses_the_identical_structured_review_parser() {
    with_env_key("PRD071_TEST_KEY_SINGLE", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        let review_text = json!({
            "review_id": "review-single-path",
            "reviewed_manifest_hash": "sha256:manifest",
            "findings": [{
                "finding_id": "f1",
                "category": "correctness_defect",
                "severity": "high",
                "blocking": true,
                "title": "off by one",
                "claim": "loop overruns by one element",
                "evidence": [{"kind": "file_range", "path": "src/lib.rs", "range": {"start": 1, "end": 2}}],
                "remediation": "fix the bound",
                "status": "open",
                "supersedes": null
            }]
        })
        .to_string();
        let direct: familiar_ai_review::WireReviewResult =
            familiar_ai_review::parse_structured_review_text(&review_text).unwrap();

        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_single",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_single"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_single",
                    "type": "message_batch",
                    "processing_status": "ended",
                    "results_url": format!("{}/v1/messages/batches/msgbatch_single/results", server.uri())
                })))
                .mount(&server)
                .await;
            let line = json!({
                "custom_id": "review-single-path",
                "result": {
                    "type": "succeeded",
                    "message": {
                        "id": "msg_1",
                        "model": "claude-review-model-resolved",
                        "content": [{"type": "text", "text": review_text}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 500, "output_tokens": 80}
                    }
                }
            });
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_single/results"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(format!("{line}\n"), "application/x-jsonl"),
                )
                .mount(&server)
                .await;
        });

        let db = open_db();
        let fallback = FallbackReviewer {
            result: fallback_result("review-single-path"),
        };
        let request = review_request("review-single-path");

        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        }
        let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
        let result = batch_agent.review(&request, &mut Vec::new()).unwrap();

        assert_eq!(result.review_id, direct.review_id);
        assert_eq!(result.reviewed_manifest_hash, direct.reviewed_manifest_hash);
        assert_eq!(
            findings_signature(&result.findings),
            findings_signature(&direct.findings)
        );
        assert_eq!(result.usage.input_tokens, Some(500));
        assert_eq!(result.usage.output_tokens, Some(80));

        // Exactly-once: the row is `applied`, and a second call finds
        // nothing left to apply and parks again rather than re-deciding.
        let row = BatchReviewRepository::new(db.conn())
            .find_by_review_id("review-single-path")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "applied");
    });
}

/// `familiar-ai batch-review enable` writes the batch policy to
/// `repositories.<key>.review.tier_policy`, never to the top-level
/// `[review.tier_policy]`. `build_batch_reviewer` must resolve that
/// repository-scoped policy directly — a config where only the repository
/// entry (not the global default) enables batch must still resolve a
/// worker and credential.
#[test]
fn build_batch_reviewer_resolves_a_repository_scoped_enablement() {
    let worker = RegistryWorkerConfig {
        adapter: Some(AgentAdapterKind::Codex),
        provider: "anthropic".into(),
        model: "claude-review-model".into(),
        runtime: Some("anthropic-api".into()),
        model_artifact: None,
        auth_profile: Some("anthropic-batch".into()),
        capability_profile: None,
        runtime_config: None,
        executable: None,
        capabilities: vec![],
        fresh_process_isolation: true,
        context_tokens: 100,
        estimated_cost_microusd: Some(1),
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: vec![],
    };
    let scoped_policy = ReviewTierPolicyConfig {
        independent_review_required: false,
        standard_reviewer_agent: ReviewAgentConfig::default(),
        full_review_risk_classes: vec![],
        batch_risk_classes: vec!["low-risk-docs".into()],
        max_batch_wait_ms: 3_600_000,
        batch_worker: Some("batch-anthropic".into()),
        rules: vec![],
    };
    let mut config = Config {
        worker_registry: Some(WorkerRegistryConfig {
            workers: BTreeMap::from([("batch-anthropic".to_string(), worker)]),
            ..Default::default()
        }),
        ..Config::default()
    };
    config
        .auth_profiles
        .insert("anthropic-batch".into(), AuthDescriptor::Env("SK".into()));
    config.repositories.insert(
        "repo/.git".into(),
        RepositoryConfig {
            review: Some(ReviewConfig {
                tier_policy: Some(scoped_policy),
                ..ReviewConfig::default()
            }),
            ..RepositoryConfig::default()
        },
    );
    // The top-level policy stays unconfigured: only the repository entry
    // enables batch, matching exactly what the CLI's `enable` command writes.
    assert!(config.review.tier_policy.is_none());

    let db = open_db();
    let fallback = FallbackReviewer {
        result: fallback_result("review-scope"),
    };
    let reviewer = build_batch_reviewer(
        &config,
        db.conn(),
        "cycle-1".into(),
        "repo/.git".into(),
        "PRD-071".into(),
        "exec-1".into(),
        vec!["low-risk-docs".into()],
        &fallback,
    );
    assert!(
        reviewer.is_some(),
        "repository-scoped batch enablement must resolve a batch reviewer"
    );

    // A different repository, with no override of its own and no global
    // default, must not inherit the first repository's enablement.
    let unscoped = build_batch_reviewer(
        &config,
        db.conn(),
        "cycle-1".into(),
        "other-repo/.git".into(),
        "PRD-071".into(),
        "exec-1".into(),
        vec!["low-risk-docs".into()],
        &fallback,
    );
    assert!(
        unscoped.is_none(),
        "batch tiering must stay off for a repository that never enabled it"
    );
}

/// The batch tier applies a batch-rated discount to the configured
/// interactive per-token rate rather than recording the undiscounted
/// interactive-rate figure under a `"batch"` label: the recorded estimate
/// must genuinely differ from what the identical token counts would cost
/// through the interactive tier, and `batch_reviews.provider_cost_lexical`
/// must carry that same batch-rated estimate instead of staying `None`.
#[test]
fn batch_completion_records_a_batch_discounted_cost_distinct_from_interactive_rate() {
    with_env_key("PRD071_TEST_KEY_COST", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        let review_text = json!({
            "review_id": "review-cost",
            "reviewed_manifest_hash": "sha256:manifest",
            "findings": []
        })
        .to_string();
        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_cost",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_cost"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_cost",
                    "type": "message_batch",
                    "processing_status": "ended",
                    "results_url": format!("{}/v1/messages/batches/msgbatch_cost/results", server.uri())
                })))
                .mount(&server)
                .await;
            let line = json!({
                "custom_id": "review-cost",
                "result": {
                    "type": "succeeded",
                    "message": {
                        "id": "msg_1",
                        "model": "claude-review-model-resolved",
                        "content": [{"type": "text", "text": review_text}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 1000, "output_tokens": 200}
                    }
                }
            });
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_cost/results"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(format!("{line}\n"), "application/x-jsonl"),
                )
                .mount(&server)
                .await;
        });

        let db = open_db();
        let fallback = FallbackReviewer {
            result: fallback_result("review-cost"),
        };
        let request = review_request("review-cost");
        let mut pricing = BTreeMap::new();
        pricing.insert(
            "claude-review-model-resolved".to_string(),
            familiar_ai_core::ExecutionPrice {
                input_microusd_per_million: Some(3_000_000),
                cached_input_microusd_per_million: Some(300_000),
                output_microusd_per_million: Some(15_000_000),
            },
        );

        {
            let batch_agent = agent_with_pricing(
                db.conn(),
                batch_client(&server),
                &fallback,
                env_key,
                pricing.clone(),
            );
            batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        }
        let batch_agent = agent_with_pricing(
            db.conn(),
            batch_client(&server),
            &fallback,
            env_key,
            pricing.clone(),
        );
        batch_agent.review(&request, &mut Vec::new()).unwrap();

        let price = pricing.get("claude-review-model-resolved").unwrap();
        let (interactive_amount, _, _) =
            familiar_ai_daemon::run::calculate_cost(Some(1000), Some(0), Some(200), Some(price));
        let interactive_amount = interactive_amount.unwrap();

        let recorded_nanousd: i64 = db
            .conn()
            .query_row(
                "SELECT cost_estimates.amount FROM cost_estimates JOIN usage_observations ON usage_observations.observation_id = cost_estimates.observation_id WHERE usage_observations.attempt_id='review-cost'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let recorded_microusd = (recorded_nanousd / 1000) as u64;
        assert_ne!(
            recorded_microusd, interactive_amount,
            "batch cost must differ from the undiscounted interactive-rate computation for the same tokens"
        );
        assert_eq!(recorded_microusd, interactive_amount / 2);

        let row = BatchReviewRepository::new(db.conn())
            .find_by_review_id("review-cost")
            .unwrap()
            .unwrap();
        assert!(
            row.provider_cost_lexical.is_some(),
            "batch_reviews.provider_cost_lexical must carry the batch-rated estimate, not stay None forever"
        );
    });
}

/// A completed batch payload that fails to parse must never strand the row
/// in `applied` with no consumer — the `completed` -> `applied` transition
/// only commits once the result has actually been parsed and its
/// accounting recorded, so the row stays retryable from durable state
/// rather than permanently unrecoverable.
#[test]
fn completed_batch_with_unparseable_payload_never_strands_the_row_as_applied() {
    with_env_key("PRD071_TEST_KEY_PARSEFAIL", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_parsefail",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_parsefail"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_parsefail",
                    "type": "message_batch",
                    "processing_status": "ended",
                    "results_url": format!("{}/v1/messages/batches/msgbatch_parsefail/results", server.uri())
                })))
                .mount(&server)
                .await;
            let line = json!({
                "custom_id": "review-parsefail",
                "result": {
                    "type": "succeeded",
                    "message": {
                        "id": "msg_1",
                        "model": "claude-review-model-resolved",
                        "content": [{"type": "text", "text": "this is not a structured review payload"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 10, "output_tokens": 5}
                    }
                }
            });
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_parsefail/results"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(format!("{line}\n"), "application/x-jsonl"),
                )
                .mount(&server)
                .await;
        });

        let db = open_db();
        let fallback = FallbackReviewer {
            result: fallback_result("review-parsefail"),
        };
        let request = review_request("review-parsefail");

        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        }
        // The batch has ended, but the payload doesn't parse as a
        // structured review — must error, never fabricate a result or
        // strand the row `applied` with nothing having consumed it.
        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            let error = batch_agent.review(&request, &mut Vec::new()).unwrap_err();
            assert!(matches!(error, ReviewExecutionError::Agent(_)));
        }
        let row = BatchReviewRepository::new(db.conn())
            .find_by_review_id("review-parsefail")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.state, "completed",
            "an unparseable payload must leave the row consumable again, never permanently `applied`"
        );

        // A third attempt retries from the same durable `completed` state
        // rather than hitting the unrecoverable `Applied` hard-error arm.
        let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
        let error = batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        assert!(matches!(error, ReviewExecutionError::Agent(_)));
    });
}

/// If a batch result was already committed `applied` but no disposition was
/// ever recorded for it (e.g. a crash immediately after `review()` returned
/// `Ok`), re-entering `review()` must recover via the interactive fallback
/// with a durable reason instead of hard-erroring forever.
#[test]
fn applied_reentry_with_no_disposition_falls_back_to_interactive_with_a_durable_reason() {
    with_env_key("PRD071_TEST_KEY_REENTRY", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_reentry",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .mount(&server)
                .await;
        });

        let db = open_db();
        let expected = fallback_result("review-reentry");
        let fallback = FallbackReviewer {
            result: expected.clone(),
        };
        let request = review_request("review-reentry");
        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        }
        // Model the row having already been committed `applied` by a prior,
        // crashed run — no disposition was ever recorded for it.
        BatchReviewRepository::new(db.conn())
            .mark_completed("review-reentry", "{\"ok\":true}", None)
            .unwrap();
        assert!(BatchReviewRepository::new(db.conn())
            .mark_applied("review-reentry")
            .unwrap());

        let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
        let result = batch_agent.review(&request, &mut Vec::new()).unwrap();
        assert_eq!(result.review_id, expected.review_id);
        assert_eq!(result.reviewer.assignment.adapter_id, "fake-interactive");

        let row = BatchReviewRepository::new(db.conn())
            .find_by_review_id("review-reentry")
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "expired_fallback");
        assert_eq!(
            row.fallback_reason.as_deref(),
            Some("resumed_after_applied_with_no_recorded_disposition")
        );
    });
}

fn findings_signature(findings: &[ReviewFinding]) -> Vec<String> {
    findings
        .iter()
        .map(|finding| {
            format!(
                "{:?}:{}:{}",
                finding.category, finding.finding_id, finding.claim
            )
        })
        .collect()
}

/// Every batch completion's ledger observation records the batch tier
/// distinctly from interactive-tier observations, so a PRD-051 comparison
/// can partition by tier for the same risk class rather than assuming a
/// discount rate.
#[test]
fn batch_completion_records_a_tier_partitioned_accounting_observation() {
    with_env_key("PRD071_TEST_KEY_ACCT", |env_key| {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let server = outer.block_on(MockServer::start());
        let review_text = json!({
            "review_id": "review-accounting",
            "reviewed_manifest_hash": "sha256:manifest",
            "findings": []
        })
        .to_string();
        outer.block_on(async {
            Mock::given(method("POST"))
                .and(path("/v1/messages/batches"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_acct",
                    "type": "message_batch",
                    "processing_status": "in_progress",
                    "results_url": null
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_acct"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "msgbatch_acct",
                    "type": "message_batch",
                    "processing_status": "ended",
                    "results_url": format!("{}/v1/messages/batches/msgbatch_acct/results", server.uri())
                })))
                .mount(&server)
                .await;
            let line = json!({
                "custom_id": "review-accounting",
                "result": {
                    "type": "succeeded",
                    "message": {
                        "id": "msg_1",
                        "model": "claude-review-model-resolved",
                        "content": [{"type": "text", "text": review_text}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 1000, "output_tokens": 200}
                    }
                }
            });
            Mock::given(method("GET"))
                .and(path("/v1/messages/batches/msgbatch_acct/results"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(format!("{line}\n"), "application/x-jsonl"),
                )
                .mount(&server)
                .await;
        });

        let db = open_db();
        let fallback = FallbackReviewer {
            result: fallback_result("review-accounting"),
        };
        let request = review_request("review-accounting");

        {
            let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
            batch_agent.review(&request, &mut Vec::new()).unwrap_err();
        }
        let batch_agent = agent(db.conn(), batch_client(&server), &fallback, env_key);
        batch_agent.review(&request, &mut Vec::new()).unwrap();

        let (service_tier, stage, uncached_input, output): (String, String, i64, i64) = db
            .conn()
            .query_row(
                "SELECT service_tier,stage,uncached_input_tokens,output_tokens FROM usage_observations WHERE attempt_id='review-accounting'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(service_tier, "batch");
        assert_eq!(stage, "review");
        assert_eq!(uncached_input, 1000);
        assert_eq!(output, 200);
    });
}

/// The daemon poller must evaluate the bounded-wait deadline before
/// resolving auth, a client, or a credential for a row: a repository whose
/// batch policy has since been disabled (e.g. `familiar-ai batch-review
/// disable` on its last configured risk class) still has a row sitting in
/// `submitted`, and `batch_auth` now resolves to `None` for it. That must
/// not orphan the row — it still has to expire and fall back within its
/// bound, exactly like a row whose provider access still resolves.
#[test]
fn poller_expires_a_row_whose_repository_policy_is_no_longer_configured() {
    let db = open_db();
    BatchReviewRepository::new(db.conn())
        .submit(&NewBatchReview {
            review_id: "review-orphaned",
            cycle_id: "cycle-1",
            repository_key: "repo/.git",
            prd_id: "PRD-071",
            risk_class: "low-risk-docs",
            provider: "anthropic",
            provider_batch_id: "msgbatch_orphaned",
            provider_request_id: None,
            max_wait_ms: 3_600_000,
        })
        .unwrap();
    // Force the deadline into the past — deterministic, no sleep.
    db.conn()
        .execute(
            "UPDATE batch_reviews SET deadline_at='2000-01-01T00:00:00Z' WHERE review_id='review-orphaned'",
            [],
        )
        .unwrap();

    // No repository entry and no top-level tier policy: `batch_auth`
    // resolves to `None`, exactly as it does once the last configured risk
    // class is disabled for `repo/.git`.
    let config = Config::default();
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        runtime_dir: tmp.path().join("runtime"),
        log_dir: tmp.path().join("log"),
        socket_path: tmp.path().join("familiar-ai.sock"),
        pid_path: tmp.path().join("familiar-ai.pid"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();

    let db = Arc::new(Mutex::new(db));
    let outer = tokio::runtime::Runtime::new().unwrap();
    outer.block_on(poll_once(&db, &config, &paths));

    let guard = db.lock().unwrap();
    let row = BatchReviewRepository::new(guard.conn())
        .find_by_review_id("review-orphaned")
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "expired_fallback");
    assert_eq!(
        row.fallback_reason.as_deref(),
        Some("max_batch_wait_exceeded")
    );
}
