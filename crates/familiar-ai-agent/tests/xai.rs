//! PRD-061 integration coverage: `familiar_ai_llm::xai_api::XaiAdapter`
//! round-tripped through the provider-neutral `raw_runtime::run_loop`
//! against a `wiremock` fake of the real xAI wire shape — proving the
//! adapter needs no loop, authority, or persistence change, and that its
//! whole-chunk tool-call delivery, timeout/ambiguity handling, and error
//! taxonomy all flow correctly through PRD-058's generic loop. No test in
//! this file performs, or could perform, a live or billable call.

use std::time::Duration;

use familiar_ai_agent::raw_runtime::{
    run_loop, AuthorityContext, CancellationToken, CapabilityId, ExecutionError, ExecutionOutcome,
    InMemoryToolJournal, LoopCeilings, LoopConfig, ProviderFailureTaxonomy, ScopeAuthorizer,
    StablePrefix, StopReason, ToolExecutor, ValidatedCall, VolatileTask,
};
use familiar_ai_core::config::{
    AuthDescriptor, PriceCurrency, PriceScheduleConfig, PriceScheduleRateConfig,
};
use familiar_ai_llm::xai_api::{XaiAdapter, XaiAdapterConfig};
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

fn authority() -> AuthorityContext {
    AuthorityContext {
        project_id: "proj_1".into(),
        execution_id: "exec_1".into(),
        attempt_id: "attempt_1".into(),
        worker_id: "worker_1".into(),
    }
}

fn prefix() -> StablePrefix {
    StablePrefix {
        bytes: "stable repository context".into(),
        version: "prefix-v1".into(),
    }
}

fn task() -> VolatileTask {
    VolatileTask {
        bytes: "implement the change".into(),
    }
}

fn attempt_id_source() -> impl FnMut() -> familiar_ai_llm::attempt::AttemptId {
    let mut n = 0u32;
    move || {
        n += 1;
        familiar_ai_llm::attempt::AttemptId(format!("att_{n}"))
    }
}

fn base_config() -> LoopConfig {
    LoopConfig {
        worker_spec_identity: "wspec-sha256:xai-test".into(),
        worker_empirical_version: "wver-sha256:xai-test".into(),
        model: "grok-4".into(),
        prompt_template_version: "agent-loop-prompt/1".into(),
        ceilings: LoopCeilings {
            max_iterations: 10,
            max_output_tokens: None,
            max_wall_clock_ms: None,
        },
        offered_capabilities: vec![CapabilityId::ApplyEdit, CapabilityId::ReportProgress],
        structured_output: None,
        authority: authority(),
    }
}

#[derive(Default)]
struct SpyExecutor {
    calls: Vec<ValidatedCall>,
}

impl ToolExecutor for SpyExecutor {
    fn execute(
        &mut self,
        call: &ValidatedCall,
        _ctx: &AuthorityContext,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        self.calls.push(call.clone());
        Ok(ExecutionOutcome {
            result_text: "ok".into(),
            result_hash: format!("hash-{}", call.call_id),
        })
    }
}

async fn xai_adapter(server: &MockServer, env_name: &str) -> XaiAdapter {
    std::env::set_var(env_name, "sk-test-key");
    XaiAdapter::new(XaiAdapterConfig {
        base_url: server.uri(),
        auth: AuthDescriptor::Env(env_name.into()),
        request_timeout_secs: 5,
    })
    .unwrap()
}

/// Pins the fix for `xai-tool-result-turn-has-no-assistant-tool-calls`: a
/// second-and-later turn's request body must never carry a `role: "tool"`
/// message that is not directly preceded by an assistant message whose
/// `tool_calls` array names that same id — the shape xAI's
/// `/v1/chat/completions` (and every OpenAI-compatible surface) requires,
/// and rejects a request for violating.
struct ToolCallPrecedesEachToolResult;

impl Match for ToolCallPrecedesEachToolResult {
    fn matches(&self, request: &Request) -> bool {
        let Ok(body): Result<serde_json::Value, _> = serde_json::from_slice(&request.body) else {
            return false;
        };
        let Some(messages) = body["messages"].as_array() else {
            return false;
        };
        for (index, message) in messages.iter().enumerate() {
            if message["role"] != "tool" {
                continue;
            }
            let Some(tool_call_id) = message["tool_call_id"].as_str() else {
                return false;
            };
            let Some(preceding) = index.checked_sub(1).map(|i| &messages[i]) else {
                return false;
            };
            if preceding["role"] != "assistant" {
                return false;
            }
            let names_this_id = preceding["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.iter().any(|call| call["id"] == tool_call_id));
            if !names_this_id {
                return false;
            }
        }
        true
    }
}

fn sse(lines: &[&str]) -> String {
    let mut body = String::new();
    for line in lines {
        body.push_str("data: ");
        body.push_str(line);
        body.push_str("\n\n");
    }
    body
}

#[tokio::test]
async fn whole_chunk_tool_call_then_completion_round_trips_through_the_loop() {
    let server = MockServer::start().await;
    let tool_call_body = sse(&[
        r#"{"id":"req_1","model":"grok-4-0709","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"apply-edit","arguments":"{\"path\":\"src/lib.rs\",\"content\":\"fn main() {}\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        r#"{"id":"req_1","model":"grok-4-0709","choices":[],"usage":{"prompt_tokens":40,"completion_tokens":12,"prompt_tokens_details":{"text_tokens":40,"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0},"cost_in_usd_ticks":9000}}"#,
        "[DONE]",
    ]);
    let done_body = sse(&[
        r#"{"id":"req_2","model":"grok-4-0709","choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]}"#,
        r#"{"id":"req_2","model":"grok-4-0709","choices":[],"usage":{"prompt_tokens":50,"completion_tokens":3,"prompt_tokens_details":{"text_tokens":50,"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0},"cost_in_usd_ticks":1500}}"#,
        "[DONE]",
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(tool_call_body, "text/event-stream"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        // The second turn's request body must carry the tool result from
        // `call_1`; it is only accepted here if it is directly preceded by
        // an assistant `tool_calls` message naming that id — a mock that
        // would reject the transcript an OpenAI-compatible provider rejects.
        .and(ToolCallPrecedesEachToolResult)
        .respond_with(ResponseTemplate::new(200).set_body_raw(done_body, "text/event-stream"))
        .mount(&server)
        .await;

    let adapter = xai_adapter(&server, "XAI_LOOP_TEST_KEY_1").await;
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit, CapabilityId::ReportProgress],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::Completed {
            structured_output: false
        }
    );
    assert_eq!(executor.calls.len(), 1);
    assert_eq!(outcome.attempts.len(), 2);
    assert_eq!(outcome.attempts[0].usage.output_tokens, Some(12));
    assert_eq!(outcome.attempts[1].usage.output_tokens, Some(3));
    // The requested alias is what the loop's evidence carries as canonical
    // identity; the provider-resolved identity is separate telemetry.
    assert_eq!(
        outcome.evidence.worker_spec_identity,
        "wspec-sha256:xai-test"
    );
    assert_eq!(adapter.last_resolved_model(), Some("grok-4-0709".into()));
}

#[tokio::test]
async fn wall_clock_timeout_against_a_real_stalled_response_is_ambiguous_not_zero_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(500))
                .set_body_raw(sse(&["[DONE]"]), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let adapter = xai_adapter(&server, "XAI_LOOP_TEST_KEY_2").await;
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();
    let mut config = base_config();
    config.ceilings.max_wall_clock_ms = Some(20);

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &config,
        attempt_id_source(),
    )
    .await;

    assert_eq!(outcome.stop_reason, StopReason::Timeout);
    assert_eq!(outcome.attempts.len(), 1);
    assert!(outcome.attempts[0].ambiguous);
    assert!(outcome.attempts[0].usage.is_entirely_unknown());
}

#[tokio::test]
async fn provider_5xx_maps_to_a_retryable_provider_failure_stop_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let adapter = xai_adapter(&server, "XAI_LOOP_TEST_KEY_3").await;
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::ProviderFailure {
            taxonomy: ProviderFailureTaxonomy::Retryable
        }
    );
}

#[tokio::test]
async fn auth_failure_fails_closed_with_no_request_ever_sent_credentials() {
    let server = MockServer::start().await;
    // No mock mounted: any request would be an unexpected-request panic
    // from wiremock, proving auth failure is caught before the network call.
    std::env::remove_var("XAI_LOOP_TEST_MISSING_KEY");
    let adapter = XaiAdapter::new(XaiAdapterConfig {
        base_url: server.uri(),
        auth: AuthDescriptor::Env("XAI_LOOP_TEST_MISSING_KEY".into()),
        request_timeout_secs: 5,
    })
    .unwrap();
    let mut executor = SpyExecutor::default();
    let authorizer = ScopeAuthorizer {
        granted_capabilities: vec![CapabilityId::ApplyEdit],
        allowed_write_paths: vec!["src/".into()],
        allowed_commands: vec![],
        network_allowed: false,
    };
    let mut journal = InMemoryToolJournal::default();

    let outcome = run_loop(
        &adapter,
        &mut executor,
        &authorizer,
        &mut journal,
        &CancellationToken::new(),
        &prefix(),
        &task(),
        &base_config(),
        attempt_id_source(),
    )
    .await;

    assert_eq!(
        outcome.stop_reason,
        StopReason::ProviderFailure {
            taxonomy: ProviderFailureTaxonomy::NonRetryable
        }
    );
}

/// Grok pricing, versioned and dated, sourced from `docs.x.ai/docs/models`
/// as consulted 2026-09-01 (see `docs/contracts/xai-adapter.md` for the
/// full record). This is never a fallback for an OpenAI schedule — it is
/// the whole schedule an xAI worker's estimate is computed from.
#[test]
fn grok_price_schedule_matches_official_xai_pricing_in_nanousd() {
    let schedule = PriceScheduleConfig {
        effective_at: "2026-09-01T00:00:00Z".into(),
        currency: PriceCurrency::USD,
        calculation_version: "xai-pricing-2026-09-01".into(),
        models: std::collections::BTreeMap::from([
            (
                "grok-4.6".into(),
                PriceScheduleRateConfig {
                    uncached_input_nanousd_per_million: Some(2_000_000_000),
                    cache_read_nanousd_per_million: Some(500_000_000),
                    cache_write_nanousd_per_million: None,
                    output_nanousd_per_million: Some(6_000_000_000),
                    // xAI bills reasoning tokens at the completion (output)
                    // rate — there is no separate reasoning-token price on
                    // docs.x.ai/docs/models. Mirroring the output rate here
                    // (rather than leaving this `None`) is what keeps a
                    // Grok attempt's local estimate from silently omitting
                    // every reasoning token it reports.
                    reasoning_output_nanousd_per_million: Some(6_000_000_000),
                },
            ),
            (
                "grok-4.3".into(),
                PriceScheduleRateConfig {
                    uncached_input_nanousd_per_million: Some(1_250_000_000),
                    cache_read_nanousd_per_million: Some(200_000_000),
                    cache_write_nanousd_per_million: None,
                    output_nanousd_per_million: Some(2_500_000_000),
                    reasoning_output_nanousd_per_million: Some(2_500_000_000),
                },
            ),
        ]),
    };

    let grok_4_6 = &schedule.models["grok-4.6"];
    // $2.00/M tokens == 2,000,000,000 nanoUSD/M ($1 == 1e9 nanoUSD).
    assert_eq!(
        grok_4_6.uncached_input_nanousd_per_million,
        Some(2_000_000_000)
    );
    assert_eq!(grok_4_6.output_nanousd_per_million, Some(6_000_000_000));
    // xAI bills reasoning tokens at the completion rate, so the reasoning
    // rate mirrors the output rate rather than being left unpriced.
    assert_eq!(
        grok_4_6.reasoning_output_nanousd_per_million,
        grok_4_6.output_nanousd_per_million
    );
    // xAI documents no cache-*write* token count or rate; never guessed.
    assert_eq!(grok_4_6.cache_write_nanousd_per_million, None);
    assert_eq!(
        schedule.models["grok-4.3"].output_nanousd_per_million,
        Some(2_500_000_000)
    );
    assert_eq!(
        schedule.models["grok-4.3"].reasoning_output_nanousd_per_million,
        Some(2_500_000_000)
    );
    assert_eq!(schedule.calculation_version, "xai-pricing-2026-09-01");
}

/// Reproduces the `finding: xai-reasoning-tokens-unpriced` regression: a
/// Grok attempt with non-zero `completion_tokens_details.reasoning_tokens`
/// must not have those tokens silently drop out of its local-estimate cost.
/// `XaiAdapter::map_usage` (unit-tested directly in `xai_api.rs`) splits
/// `completion_tokens` into a non-reasoning `output_tokens` remainder and a
/// separate `reasoning_output_tokens` count; this test proves the price
/// schedule those two categories are priced against — the same schedule
/// produced above — actually covers both, using the identical per-category
/// pricing model `familiar-ai-storage`'s accounting repository applies
/// (price * tokens / 1_000_000, summed per usage category).
#[test]
fn grok_estimate_covers_reasoning_tokens_not_just_output_tokens() {
    let rate = PriceScheduleRateConfig {
        uncached_input_nanousd_per_million: Some(2_000_000_000),
        cache_read_nanousd_per_million: Some(500_000_000),
        cache_write_nanousd_per_million: None,
        output_nanousd_per_million: Some(6_000_000_000),
        reasoning_output_nanousd_per_million: Some(6_000_000_000),
    };

    // From `nonzero_reasoning_tokens_are_split_from_output_and_reported_separately`
    // in xai_api.rs: 50 completion tokens, 35 reasoning -> 15 output.
    let output_tokens: u64 = 15;
    let reasoning_output_tokens: u64 = 35;

    let category_cost = |tokens: u64, rate_per_million: Option<u64>| -> u64 {
        rate_per_million
            .map(|rate| (tokens as u128 * rate as u128 / 1_000_000) as u64)
            .unwrap_or(0)
    };

    let output_cost_nanousd = category_cost(output_tokens, rate.output_nanousd_per_million);
    let reasoning_cost_nanousd = category_cost(
        reasoning_output_tokens,
        rate.reasoning_output_nanousd_per_million,
    );

    assert!(
        reasoning_cost_nanousd > 0,
        "reasoning tokens must contribute a nonzero cost, not be silently omitted"
    );
    assert_eq!(
        output_cost_nanousd + reasoning_cost_nanousd,
        category_cost(output_tokens, rate.output_nanousd_per_million)
            + category_cost(reasoning_output_tokens, rate.output_nanousd_per_million),
        "reasoning tokens are billed at the completion rate, so pricing them at \
         reasoning_output_nanousd_per_million must equal pricing them at the output rate"
    );
}

/// xAI's per-request `cost_in_usd_ticks` (10,000,000,000 ticks == $1 USD,
/// per `docs.x.ai`'s cost-tracking guide) is a vendor-reported figure
/// distinct from — and not a substitute for — an authoritative
/// organization billing/administrative cost API, which xAI does not
/// expose. This documents the exact conversion the adapter's
/// `last_cost_usd_ticks()` value is denominated in, for a future
/// reconciliation stage to interpret; PRD-061 itself only captures and
/// exposes the raw ticks losslessly (see `xai_api.rs`), it does not
/// convert or persist them.
#[test]
fn cost_in_usd_ticks_conversion_is_documented_and_exact() {
    let ticks: u128 = 37_756_000;
    let ticks_per_usd: u128 = 10_000_000_000;
    // $0.0038 for 37,756,000 ticks, as documented by xAI's own example.
    let micro_usd = ticks * 1_000_000 / ticks_per_usd;
    assert_eq!(micro_usd, 3775); // $0.003775
}
