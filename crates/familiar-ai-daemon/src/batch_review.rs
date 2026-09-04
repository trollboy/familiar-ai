//! PRD-071: daemon-owned batch-tier independent review.
//!
//! [`BatchReviewAgent`] is the `ReviewAgent` the coordinator calls when a
//! review attempt's declared risk class is configured for the batch tier.
//! It never blocks waiting on the provider: a submission durably records
//! the batch in `batch_reviews` (via [`familiar_ai_storage::BatchReviewRepository`])
//! and returns `ReviewExecutionError::Parked` immediately, freeing the
//! calling PRD's admission slot. A re-driven review cycle checks the same
//! durable row before ever resubmitting — this makes the whole flow
//! idempotent across daemon restarts. Completed results flow through
//! `parse_structured_review_text` — the identical parser the interactive
//! transport uses — and then through the ordinary coordinator disposition
//! machinery; no separate code path decides outcomes.
//!
//! [`run`] is the daemon-owned background poller (mirroring
//! `summary_worker`'s shape): it periodically checks every still-pending
//! batch across every repository, applies completions or bounded-wait
//! expiry fallback as soon as they're known — rather than waiting for an
//! unrelated resume to stumble across them — and resumes cleanly from
//! durable state after a crash because it reads the same table.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use rusqlite::Connection;

use familiar_ai_core::config::AuthDescriptor;
use familiar_ai_core::{AppPaths, Config};
use familiar_ai_llm::anthropic_api::{
    AnthropicHttpClient, AnthropicHttpConfig, CredentialResolver, EnvCredentialResolver,
    WireBatchResult, WireContentBlock, WireMessage, WireRequestBody,
};
use familiar_ai_review::{
    render_review_prompt, AdapterIsolationEvidence, AgentAssignment, AgentObservation,
    ExecutionUsage, ReviewAgent, ReviewDisposition, ReviewExecutionError, ReviewRequest,
    ReviewResult,
};
use familiar_ai_storage::{
    AccountingRepository, BatchReviewRepository, BatchReviewRow, BatchReviewState, Database,
    NewBatchReview, UsageObservation,
};

/// The provider identity recorded on every batch-tier ledger observation
/// and durable row (PRD-059's first, and currently only, batch-capable
/// adapter).
pub const BATCH_PROVIDER: &str = "anthropic-api";
const BATCH_MAX_OUTPUT_TOKENS: u64 = 16_384;
/// Anthropic's Message Batches API is documented to bill at half the
/// interactive per-token rate. The batch endpoint never itself reports a
/// dollar cost, so this fixed, published discount factor is what makes a
/// batch observation's recorded cost an actual batch-rated estimate rather
/// than the undiscounted interactive-rate figure it would otherwise share
/// with an interactive execution of identical token counts.
const BATCH_PRICE_DISCOUNT_DENOMINATOR: u64 = 2;

/// Computes the batch-rated cost estimate for a completed batch member:
/// the configured interactive per-token rate for `model`, discounted by
/// [`BATCH_PRICE_DISCOUNT_DENOMINATOR`]. Returns `None` when the model is
/// unknown or unconfigured, matching `calculate_cost`'s "pricing not
/// configured" behavior — no estimate is fabricated without a rate.
/// Returns `(amount_microusd, lexical_usd, rates_json)`; `rates_json` tags
/// its provenance as a local batch-rate estimate, distinct from an
/// interactive-tier configured-rate estimate for the same tokens.
fn batch_cost_estimate(
    pricing: &BTreeMap<String, familiar_ai_core::ExecutionPrice>,
    model: Option<&str>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Option<(u64, String, String)> {
    let model = model?;
    let price = pricing.get(model)?;
    let (interactive_cost, _, _) =
        crate::run::calculate_cost(input_tokens, Some(0), output_tokens, Some(price));
    let interactive_amount = interactive_cost?;
    let batch_amount = interactive_amount / BATCH_PRICE_DISCOUNT_DENOMINATOR;
    let lexical = format!(
        "{}.{:06}",
        batch_amount / 1_000_000,
        batch_amount % 1_000_000
    );
    let rates = serde_json::json!({
        "input": price.input_microusd_per_million,
        "cached_input": price.cached_input_microusd_per_million,
        "output": price.output_microusd_per_million,
        "service_tier": "batch",
        "batch_discount_denominator": BATCH_PRICE_DISCOUNT_DENOMINATOR,
        "provenance": "batch-rate-estimate",
    })
    .to_string();
    Some((batch_amount, lexical, rates))
}

/// Everything a `BatchReviewAgent` needs about the PRD execution it is
/// reviewing, beyond what travels inside the model-visible `ReviewRequest`.
/// Kept separate from `ReviewRequest` deliberately: tiering and scheduling
/// metadata are operational facts, never part of the disclosed review
/// package.
#[derive(Debug, Clone)]
pub struct BatchReviewContext {
    pub cycle_id: String,
    pub repository_key: String,
    /// The PRD's own identity (e.g. `PRD-071`) — keys `batch_reviews` and
    /// the durable checkpoint the background poller resumes from.
    pub prd_id: String,
    /// The daemon's generated execution identity — distinct from
    /// `prd_id`, and what the PRD-051 ledger's `execution_id` column
    /// (and its `execution_history` foreign key) actually references.
    pub execution_id: String,
    pub declared_risk_classes: Vec<String>,
    pub batch_risk_classes: Vec<String>,
    pub max_wait_ms: u64,
}

impl BatchReviewContext {
    fn matched_risk_class(&self) -> &str {
        self.declared_risk_classes
            .iter()
            .find(|class| self.batch_risk_classes.contains(class))
            .map(String::as_str)
            .unwrap_or("unknown")
    }
}

pub struct BatchReviewAgent<'a> {
    conn: &'a Connection,
    client: AnthropicHttpClient,
    runtime: tokio::runtime::Runtime,
    credential_resolver: Box<dyn CredentialResolver>,
    auth: AuthDescriptor,
    context: BatchReviewContext,
    fallback: &'a dyn ReviewAgent,
    /// Configured `[execution_history.pricing]` rates, keyed by model —
    /// looked up by the model the provider actually reports at completion,
    /// since batch submissions use the interactive reviewer's configured
    /// model, not the batch worker's transport-only identity.
    pricing: BTreeMap<String, familiar_ai_core::ExecutionPrice>,
}

impl<'a> BatchReviewAgent<'a> {
    /// `client` is caller-constructed so tests can point it at a
    /// `wiremock::MockServer` instead of the real Anthropic API — this
    /// type never makes a live or billable call from a test.
    pub fn new(
        conn: &'a Connection,
        client: AnthropicHttpClient,
        auth: AuthDescriptor,
        context: BatchReviewContext,
        fallback: &'a dyn ReviewAgent,
        pricing: BTreeMap<String, familiar_ai_core::ExecutionPrice>,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("batch review runtime construction failed: {error}"))?;
        Ok(Self {
            conn,
            client,
            runtime,
            credential_resolver: Box::new(EnvCredentialResolver),
            auth,
            context,
            fallback,
            pricing,
        })
    }

    fn credential(&self) -> Result<String, ReviewExecutionError> {
        self.credential_resolver
            .resolve(&self.auth)
            .map_err(ReviewExecutionError::Agent)
    }

    fn submit(
        &self,
        repository: &BatchReviewRepository<'_>,
        request: &ReviewRequest,
    ) -> Result<ReviewResult, ReviewExecutionError> {
        let model = request.reviewer.requested_model.as_deref().ok_or_else(|| {
            ReviewExecutionError::Agent("batch review requires an explicit reviewer model".into())
        })?;
        let prompt_bytes = render_review_prompt(request)
            .map_err(|error| ReviewExecutionError::Agent(error.to_string()))?;
        let prompt = String::from_utf8(prompt_bytes)
            .map_err(|error| ReviewExecutionError::Agent(error.to_string()))?;
        let body = WireRequestBody {
            model: model.to_owned(),
            max_tokens: BATCH_MAX_OUTPUT_TOKENS,
            stream: false,
            system: None,
            messages: vec![WireMessage {
                role: "user",
                content: vec![WireContentBlock::Text {
                    text: prompt,
                    cache_control: None,
                }],
            }],
            tools: None,
            thinking: None,
            output_config: None,
        };
        let api_key = self.credential()?;
        let batch = self
            .runtime
            .block_on(
                self.client
                    .submit_message_batch(&api_key, &request.review_id, &body),
            )
            .map_err(|error| {
                ReviewExecutionError::Agent(format!("batch submission failed: {error}"))
            })?;
        repository
            .submit(&NewBatchReview {
                review_id: &request.review_id,
                cycle_id: &self.context.cycle_id,
                repository_key: &self.context.repository_key,
                prd_id: &self.context.prd_id,
                risk_class: self.context.matched_risk_class(),
                provider: BATCH_PROVIDER,
                provider_batch_id: &batch.id,
                provider_request_id: None,
                max_wait_ms: self.context.max_wait_ms,
            })
            .map_err(db_err)?;
        Err(ReviewExecutionError::Parked { batch_id: batch.id })
    }

    fn poll_or_expire(
        &self,
        repository: &BatchReviewRepository<'_>,
        row: BatchReviewRow,
        request: &ReviewRequest,
        output: &mut dyn Write,
    ) -> Result<ReviewResult, ReviewExecutionError> {
        let deadline = chrono::DateTime::parse_from_rfc3339(&row.deadline_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        if Utc::now() >= deadline {
            repository
                .mark_expired_fallback(&request.review_id, "max_batch_wait_exceeded")
                .map_err(db_err)?;
            let _ = writeln!(
                output,
                "review: batch {} exceeded max wait; falling back to interactive review",
                row.provider_batch_id
            );
            return self.fallback.review(request, output);
        }
        let api_key = self.credential()?;
        let batch = self
            .runtime
            .block_on(
                self.client
                    .retrieve_message_batch(&api_key, &row.provider_batch_id),
            )
            .map_err(|error| ReviewExecutionError::Agent(format!("batch poll failed: {error}")))?;
        repository.mark_polled(&request.review_id).map_err(db_err)?;
        if !batch.ended() {
            return Err(ReviewExecutionError::Parked {
                batch_id: row.provider_batch_id,
            });
        }
        let Some(results_url) = batch.results_url else {
            return Err(ReviewExecutionError::Parked {
                batch_id: row.provider_batch_id,
            });
        };
        let lines = self
            .runtime
            .block_on(
                self.client
                    .fetch_message_batch_results(&api_key, &results_url),
            )
            .map_err(|error| {
                ReviewExecutionError::Agent(format!("batch results fetch failed: {error}"))
            })?;
        let Some(line) = lines
            .into_iter()
            .find(|line| line.custom_id == request.review_id)
        else {
            return Err(ReviewExecutionError::Agent(format!(
                "batch results are missing member '{}'",
                request.review_id
            )));
        };
        match line.result {
            WireBatchResult::Succeeded { message } => {
                let payload = BatchMessagePayload {
                    text: message.text(),
                    model: message.model.clone(),
                    input_tokens: message.usage.input_tokens,
                    output_tokens: message.usage.output_tokens,
                    submitted_at: row.submitted_at.clone(),
                    completed_at: Utc::now().to_rfc3339(),
                };
                let payload_json = serde_json::to_string(&payload).map_err(json_err)?;
                let lexical_cost = batch_cost_estimate(
                    &self.pricing,
                    payload.model.as_deref(),
                    payload.input_tokens,
                    payload.output_tokens,
                )
                .map(|(_, lexical, _)| lexical);
                repository
                    .mark_completed(&request.review_id, &payload_json, lexical_cost.as_deref())
                    .map_err(db_err)?;
                self.consume_completed(repository, request)
            }
            WireBatchResult::Errored { error } => {
                repository
                    .mark_expired_fallback(
                        &request.review_id,
                        &format!("batch_member_errored: {error}"),
                    )
                    .map_err(db_err)?;
                let _ = writeln!(
                    output,
                    "review: batch member {} errored; falling back to interactive review",
                    request.review_id
                );
                self.fallback.review(request, output)
            }
            WireBatchResult::Canceled {} | WireBatchResult::Expired {} => {
                repository
                    .mark_expired_fallback(&request.review_id, "batch_member_canceled_or_expired")
                    .map_err(db_err)?;
                self.fallback.review(request, output)
            }
        }
    }

    /// Consumes a completed-but-unapplied result: parses it through the
    /// identical structured-review parser the interactive transport uses
    /// and records its accounting observation (idempotent, safe to repeat)
    /// *before* committing the `completed` -> `applied` transition — so a
    /// failure in either step never strands the row applied with no
    /// consumer, and a daemon crash before the transition simply leaves the
    /// row `completed` for the next attempt to retry from scratch.
    fn consume_completed(
        &self,
        repository: &BatchReviewRepository<'_>,
        request: &ReviewRequest,
    ) -> Result<ReviewResult, ReviewExecutionError> {
        let Some(payload_json) = repository
            .peek_completed(&request.review_id)
            .map_err(db_err)?
        else {
            // Lost the race to a concurrent applier (the background poller
            // completed and consumed it between our completion write and
            // this read) — park again rather than fabricate a result.
            return self.park(repository, &request.review_id);
        };
        self.record_accounting(request, &payload_json)?;
        let result = parse_completed_result(request, &payload_json)?;
        if !repository
            .mark_applied(&request.review_id)
            .map_err(db_err)?
        {
            // Lost the race to a concurrent applier that already committed
            // disposition for this exact result — never apply it twice.
            return self.park(repository, &request.review_id);
        }
        Ok(result)
    }

    fn park<T>(
        &self,
        repository: &BatchReviewRepository<'_>,
        review_id: &str,
    ) -> Result<T, ReviewExecutionError> {
        let batch_id = repository
            .find_by_review_id(review_id)
            .map_err(db_err)?
            .map(|row| row.provider_batch_id)
            .unwrap_or_default();
        Err(ReviewExecutionError::Parked { batch_id })
    }

    fn record_accounting(
        &self,
        request: &ReviewRequest,
        payload_json: &str,
    ) -> Result<(), ReviewExecutionError> {
        let payload: BatchMessagePayload = serde_json::from_str(payload_json).map_err(json_err)?;
        let total = payload
            .input_tokens
            .zip(payload.output_tokens)
            .and_then(|(a, b)| a.checked_add(b));
        let worker_identity = format!(
            "{BATCH_PROVIDER}/{}",
            payload.model.as_deref().unwrap_or("unknown")
        );
        let estimate = batch_cost_estimate(
            &self.pricing,
            payload.model.as_deref(),
            payload.input_tokens,
            payload.output_tokens,
        );
        let accounting = AccountingRepository::new(self.conn);
        let observation_id = accounting
            .append_observation(&UsageObservation {
                execution_id: &self.context.execution_id,
                attempt_id: &request.review_id,
                stage: "review",
                session_id: None,
                worker_identity: &worker_identity,
                adapter: BATCH_PROVIDER,
                cli_version: None,
                model_identity: payload.model.as_deref(),
                service_tier: Some("batch"),
                provider_request_id: None,
                uncached_input_tokens: payload.input_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
                output_tokens: payload.output_tokens,
                reasoning_output_tokens: None,
                unknown_reason: (total.is_none()).then_some("usage_incomplete"),
                period_start: &payload.submitted_at,
                period_end: &payload.completed_at,
                terminal_status: "completed",
                source_event_hash: &format!("batch:{}:{}", BATCH_PROVIDER, request.review_id),
                // The batch endpoint never itself reports a dollar cost —
                // only `batch_cost_estimate`'s locally computed, discounted
                // figure is available, recorded below via the configured-rate
                // estimate path rather than fabricated as a provider fact.
                provider_cost_lexical: None,
                project_resolution_evidence: None,
                // Batch review performs no tool-driven edits and applies no
                // result truncation, so PRD-072's attribution fields are
                // "none" here — recorded explicitly rather than defaulted,
                // so the ledger can tell "not applicable" from "unmeasured".
                edit_form_id: "none",
                edit_form_version: "none",
                truncation_config_id: "none",
                truncation_config_version: "none",
                output_register_id: "none",
                output_register_version: "none",
                input_compression_id: "none",
                input_compression_version: "none",
                compression_experiment: None,
                compression_lane: None,
            })
            .map_err(db_err)?;
        if let (Some(observation_id), Some(model), Some((amount, _, rates))) =
            (observation_id, payload.model.as_deref(), estimate.as_ref())
        {
            accounting
                .append_legacy_configured_estimate(&observation_id, model, *amount, rates)
                .map_err(db_err)?;
        }
        Ok(())
    }
}

impl ReviewAgent for BatchReviewAgent<'_> {
    fn isolation_evidence(&self) -> Option<AdapterIsolationEvidence> {
        Some(AdapterIsolationEvidence {
            adapter_id: format!("{BATCH_PROVIDER}-batch"),
            fresh_process_per_execution: true,
            detail: "provider batch interface: one isolated asynchronous request per submission, no resumed session".into(),
        })
    }

    fn review(
        &self,
        request: &ReviewRequest,
        output: &mut dyn Write,
    ) -> Result<ReviewResult, ReviewExecutionError> {
        let repository = BatchReviewRepository::new(self.conn);
        let existing = repository
            .find_by_review_id(&request.review_id)
            .map_err(db_err)?;
        let Some(row) = existing else {
            return self.submit(&repository, request);
        };
        match row.state().map_err(db_err)? {
            BatchReviewState::Completed => self.consume_completed(&repository, request),
            BatchReviewState::Applied => {
                // Reached only when a prior `consume_completed` committed the
                // `applied` transition but the caller never recorded a
                // disposition for it (e.g. a crash immediately after this
                // agent returned `Ok`) — recover instead of hard-erroring
                // forever, with a durable reason for the audit trail.
                repository
                    .mark_applied_reentry_fallback(
                        &request.review_id,
                        "resumed_after_applied_with_no_recorded_disposition",
                    )
                    .map_err(db_err)?;
                let _ = writeln!(
                    output,
                    "review: batch {} was already applied with no recorded disposition; falling back to interactive review",
                    request.review_id
                );
                self.fallback.review(request, output)
            }
            BatchReviewState::ExpiredFallback => self.fallback.review(request, output),
            BatchReviewState::Submitted => self.poll_or_expire(&repository, row, request, output),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BatchMessagePayload {
    text: String,
    model: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    submitted_at: String,
    completed_at: String,
}

fn parse_completed_result(
    request: &ReviewRequest,
    payload_json: &str,
) -> Result<ReviewResult, ReviewExecutionError> {
    let payload: BatchMessagePayload = serde_json::from_str(payload_json).map_err(json_err)?;
    let wire = familiar_ai_review::parse_structured_review_text(&payload.text)?;
    let total = payload
        .input_tokens
        .zip(payload.output_tokens)
        .and_then(|(a, b)| a.checked_add(b));
    let mut unavailable = BTreeMap::new();
    if payload.model.is_none() {
        unavailable.insert("model".into(), "model_not_reported".into());
    }
    let duration_ms = chrono::DateTime::parse_from_rfc3339(&payload.submitted_at)
        .ok()
        .zip(chrono::DateTime::parse_from_rfc3339(&payload.completed_at).ok())
        .and_then(|(started, ended)| {
            u64::try_from((ended - started).num_milliseconds().max(0)).ok()
        })
        .unwrap_or(0);
    Ok(ReviewResult {
        review_id: wire.review_id,
        reviewer: AgentObservation {
            assignment: AgentAssignment {
                adapter_id: format!("{BATCH_PROVIDER}-batch"),
                agent_id: BATCH_PROVIDER.into(),
                provider: Some(BATCH_PROVIDER.into()),
                requested_model: request.reviewer.requested_model.clone(),
                role: request.reviewer.role,
                session_id: None,
            },
            agent_version: Some(format!("{BATCH_PROVIDER}-batch")),
            reported_model: payload.model.clone(),
            unavailable_fields: unavailable,
        },
        started_at: payload.submitted_at,
        ended_at: payload.completed_at,
        duration_ms,
        findings: wire.findings,
        reviewed_manifest_hash: wire.reviewed_manifest_hash,
        usage: ExecutionUsage {
            input_tokens: payload.input_tokens,
            output_tokens: payload.output_tokens,
            cached_tokens: None,
            total_tokens: total,
            estimated_cost_microusd: None,
            pricing_provenance: None,
            unavailable_fields: BTreeMap::new(),
        },
        disposition: ReviewDisposition::Pending,
        unavailable_fields: Default::default(),
    })
}

fn db_err(error: familiar_ai_core::FamiliarError) -> ReviewExecutionError {
    ReviewExecutionError::Agent(error.to_string())
}

fn json_err(error: serde_json::Error) -> ReviewExecutionError {
    ReviewExecutionError::Agent(format!("batch review payload did not parse: {error}"))
}

/// Resolves the `[review.tier_policy]` that governs `repository_key`: the
/// repository-scoped override `familiar-ai batch-review enable` writes to
/// `repositories.<key>.review.tier_policy` when one exists, the top-level
/// `[review.tier_policy]` otherwise. `repository_key` is the same Git
/// common-directory identity string carried on every `BatchReviewRow` and
/// `BatchReviewContext`, and it is exactly the map key the CLI writes under
/// `repositories.<key>` — a direct lookup, no filesystem access.
pub(crate) fn resolved_tier_policy<'a>(
    config: &'a Config,
    repository_key: &str,
) -> Option<&'a familiar_ai_core::config::ReviewTierPolicyConfig> {
    config
        .repositories
        .get(repository_key)
        .and_then(|repository| repository.review.as_ref())
        .and_then(|review| review.tier_policy.as_ref())
        .or(config.review.tier_policy.as_ref())
}

/// Resolves the configured `[worker_registry.workers.<id>]` entry named by
/// `repository_key`'s effective `tier_policy.batch_worker` into an
/// `AuthDescriptor`. Returns `None` when batch tiering is not configured
/// for this repository — the ordinary, default-off case — never when it is
/// configured but resolution fails (that is a configuration error, caught
/// at `Config::validate`).
pub fn batch_auth(config: &Config, repository_key: &str) -> Option<AuthDescriptor> {
    let policy = resolved_tier_policy(config, repository_key)?;
    if policy.batch_risk_classes.is_empty() {
        return None;
    }
    let worker_id = policy.batch_worker.as_deref()?;
    let registry = config.worker_registry.as_ref()?;
    let worker = registry.workers.get(worker_id)?;
    let profile = worker.auth_profile.as_deref()?;
    config.auth_profiles.get(profile).cloned()
}

/// Constructs the batch-tier `ReviewAgent` this execution's coordinator
/// should use, or `None` when batch tiering is not configured (the
/// default) or its configured worker/credential cannot be resolved —
/// either way, `ReviewCoordinator` safely falls back to `fallback`
/// (`reviewer`) for a `Batch`-tier selection rather than stalling.
#[allow(clippy::too_many_arguments)]
pub fn build_batch_reviewer<'a>(
    config: &Config,
    conn: &'a Connection,
    cycle_id: String,
    repository_key: String,
    prd_id: String,
    execution_id: String,
    declared_risk_classes: Vec<String>,
    fallback: &'a dyn ReviewAgent,
) -> Option<BatchReviewAgent<'a>> {
    let policy = resolved_tier_policy(config, &repository_key)?;
    if policy.batch_risk_classes.is_empty() {
        return None;
    }
    let auth = batch_auth(config, &repository_key)?;
    let context = BatchReviewContext {
        cycle_id,
        repository_key,
        prd_id,
        execution_id,
        declared_risk_classes,
        batch_risk_classes: policy.batch_risk_classes.clone(),
        max_wait_ms: policy.max_batch_wait_ms,
    };
    let client = match AnthropicHttpClient::new(AnthropicHttpConfig::default()) {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(error = %error, "anthropic-api batch client construction failed; using interactive reviewer");
            return None;
        }
    };
    match BatchReviewAgent::new(
        conn,
        client,
        auth,
        context,
        fallback,
        config.execution_history.pricing.clone(),
    ) {
        Ok(agent) => Some(agent),
        Err(error) => {
            tracing::warn!(error = %error, "batch review agent construction failed; using interactive reviewer");
            None
        }
    }
}

/// True when at least one repository — the top-level default or a
/// repository-scoped override — declares a non-empty `batch_risk_classes`.
/// The poller only needs a reason to keep running at all; per-row auth is
/// resolved separately by [`batch_auth`] against each row's own
/// `repository_key`, since different repositories may name different
/// batch workers.
fn any_batch_configured(config: &Config) -> bool {
    let enables_batch = |policy: &familiar_ai_core::config::ReviewTierPolicyConfig| {
        !policy.batch_risk_classes.is_empty()
    };
    config
        .review
        .tier_policy
        .as_ref()
        .is_some_and(enables_batch)
        || config.repositories.values().any(|repository| {
            repository
                .review
                .as_ref()
                .and_then(|review| review.tier_policy.as_ref())
                .is_some_and(enables_batch)
        })
}

/// Background poller owning the batch-review lifecycle across daemon
/// restarts: on every tick (and once immediately at startup) it checks
/// every still-`submitted` row across every repository, applies
/// completions or bounded-wait expiry as soon as they're known, and
/// re-drives the PRD's review disposition immediately rather than waiting
/// for an unrelated resume to notice.
pub async fn run(
    db: Arc<Mutex<Database>>,
    config: Config,
    paths: AppPaths,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if !any_batch_configured(&config) {
        tracing::info!("batch review disabled; no tier_policy.batch_worker configured");
        return;
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(30));
    poll_once(&db, &config, &paths).await;
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                tracing::info!("batch review worker shutting down");
                return;
            }
            _ = ticker.tick() => {
                poll_once(&db, &config, &paths).await;
            }
        }
    }
}

/// Runs one poll pass over every still-`submitted` row: applies completions
/// or bounded-wait expiry as soon as they're known. Exposed (not just
/// called from [`run`]'s loop) so integration tests can drive a single pass
/// deterministically rather than racing the 30s ticker.
pub async fn poll_once(db: &Arc<Mutex<Database>>, config: &Config, paths: &AppPaths) {
    let pending = {
        let guard = db.lock().unwrap();
        BatchReviewRepository::new(guard.conn()).submitted()
    };
    let pending = match pending {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, "batch review poll: failed to list pending rows");
            return;
        }
    };
    for row in pending {
        // The deadline is evaluated before any auth, client, or credential
        // resolution: expiry needs no provider access, so a repository whose
        // batch policy, worker, or credential has since become unresolvable
        // must still fall back to interactive review within its bound
        // instead of being silently orphaned in `submitted` forever.
        let deadline = chrono::DateTime::parse_from_rfc3339(&row.deadline_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        if Utc::now() >= deadline {
            let outcome = {
                let guard = db.lock().unwrap();
                BatchReviewRepository::new(guard.conn())
                    .mark_expired_fallback(&row.review_id, "max_batch_wait_exceeded")
                    .map(|_| true)
            };
            match outcome {
                Ok(true) => resume_now(config, paths, &row),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(review_id = %row.review_id, error = %error, "batch review poll: durable update failed");
                }
            }
            continue;
        }
        let Some(auth) = batch_auth(config, &row.repository_key) else {
            tracing::warn!(
                review_id = %row.review_id,
                repository_key = %row.repository_key,
                "batch review poll: repository batch policy is no longer configured"
            );
            continue;
        };
        let client = match AnthropicHttpClient::new(AnthropicHttpConfig::default()) {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(error = %error, "batch review poll: client construction failed");
                continue;
            }
        };
        let api_key = match EnvCredentialResolver.resolve(&auth) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(error = %error, "batch review poll: credential resolution failed");
                continue;
            }
        };
        let outcome = {
            match client
                .retrieve_message_batch(&api_key, &row.provider_batch_id)
                .await
            {
                Ok(batch) if batch.ended() => {
                    if let Some(results_url) = batch.results_url {
                        match client
                            .fetch_message_batch_results(&api_key, &results_url)
                            .await
                        {
                            Ok(lines) => {
                                let matched = lines
                                    .into_iter()
                                    .find(|line| line.custom_id == row.review_id);
                                let guard = db.lock().unwrap();
                                let repository = BatchReviewRepository::new(guard.conn());
                                match matched {
                                    Some(line) => apply_batch_result(
                                        &repository,
                                        &row,
                                        line,
                                        &config.execution_history.pricing,
                                    ),
                                    None => Ok(false),
                                }
                            }
                            Err(error) => {
                                tracing::warn!(review_id = %row.review_id, error = %error, "batch review poll: results fetch failed");
                                Ok(false)
                            }
                        }
                    } else {
                        Ok(false)
                    }
                }
                Ok(_) => {
                    let guard = db.lock().unwrap();
                    BatchReviewRepository::new(guard.conn())
                        .mark_polled(&row.review_id)
                        .map(|_| false)
                }
                Err(error) => {
                    tracing::warn!(review_id = %row.review_id, error = %error, "batch review poll: retrieval failed");
                    Ok(false)
                }
            }
        };
        match outcome {
            Ok(true) => resume_now(config, paths, &row),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(review_id = %row.review_id, error = %error, "batch review poll: durable update failed");
            }
        }
    }
}

fn apply_batch_result(
    repository: &BatchReviewRepository<'_>,
    row: &BatchReviewRow,
    line: familiar_ai_llm::anthropic_api::WireBatchResultLine,
    pricing: &BTreeMap<String, familiar_ai_core::ExecutionPrice>,
) -> familiar_ai_core::Result<bool> {
    match line.result {
        WireBatchResult::Succeeded { message } => {
            let payload = BatchMessagePayload {
                text: message.text(),
                model: message.model.clone(),
                input_tokens: message.usage.input_tokens,
                output_tokens: message.usage.output_tokens,
                submitted_at: row.submitted_at.clone(),
                completed_at: Utc::now().to_rfc3339(),
            };
            let payload_json = serde_json::to_string(&payload)
                .map_err(|error| familiar_ai_core::FamiliarError::Database(error.to_string()))?;
            let lexical_cost = batch_cost_estimate(
                pricing,
                payload.model.as_deref(),
                payload.input_tokens,
                payload.output_tokens,
            )
            .map(|(_, lexical, _)| lexical);
            repository.mark_completed(&row.review_id, &payload_json, lexical_cost.as_deref())?;
            Ok(true)
        }
        WireBatchResult::Errored { .. } => {
            repository.mark_expired_fallback(&row.review_id, "batch_member_errored")?;
            Ok(true)
        }
        WireBatchResult::Canceled {} | WireBatchResult::Expired {} => {
            repository.mark_expired_fallback(&row.review_id, "batch_member_canceled_or_expired")?;
            Ok(true)
        }
    }
}

/// Re-drives review disposition for a PRD whose batch just resolved
/// (completed or fell back), reusing the ordinary implemented-checkpoint
/// resume path rather than a second implementation of disposition.
/// Failures are logged, not fatal: the next poll (or an operator-triggered
/// resume) tries again from the same durable state.
fn resume_now(config: &Config, paths: &AppPaths, row: &BatchReviewRow) {
    let config = config.clone();
    let paths = paths.clone();
    let repository_key = row.repository_key.clone();
    let prd_id = row.prd_id.clone();
    tokio::task::spawn_blocking(move || {
        let database_path = config.database.resolve_path(&paths.data_dir);
        let db = match Database::open(&database_path) {
            Ok(db) => db,
            Err(error) => {
                tracing::warn!(prd_id, error = %error, "batch review resume: cannot open database");
                return;
            }
        };
        let checkpoint = match familiar_ai_storage::CheckpointRepository::new(db.conn())
            .get(&repository_key, &prd_id)
        {
            Ok(Some(checkpoint)) => checkpoint,
            Ok(None) => {
                tracing::warn!(prd_id, "batch review resume: no durable checkpoint");
                return;
            }
            Err(error) => {
                tracing::warn!(prd_id, error = %error, "batch review resume: checkpoint lookup failed");
                return;
            }
        };
        let worktree = Path::new(&checkpoint.worktree_path);
        let (implementation_entry, reviewer_entry) = match crate::run::resolved_agent_entries(
            &config,
        ) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(prd_id, error = %error, "batch review resume: agent resolution failed");
                return;
            }
        };
        let implementation = crate::run::build_agent(&implementation_entry);
        let reviewer = crate::run::build_agent(&reviewer_entry);
        let remediation_entry = match crate::run::resolved_remediation_entry(&config) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(prd_id, error = %error, "batch review resume: remediation agent resolution failed");
                return;
            }
        };
        let remediation = crate::run::build_agent(&remediation_entry);
        let agents = crate::run::AgentSet {
            implementation: implementation.as_ref(),
            reviewer: reviewer.as_ref(),
            remediation: remediation.as_ref(),
        };
        if let Err(error) =
            crate::run::resume_implemented_checkpoint(worktree, &prd_id, &agents, &config, &paths)
        {
            tracing::info!(prd_id, error = %error, "batch review resume: disposition not yet clean (expected for human-review-required outcomes)");
        }
    });
}
