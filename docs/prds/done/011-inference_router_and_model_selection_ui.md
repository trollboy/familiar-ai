# PRD-011: Inference Router + Model Selection UI

## Overview

Add an intelligent inference routing layer and a minimal settings UI for choosing local vs remote models.

The goal is not to build a generic LLM management product. The goal is to let Familiar decide when to use:

- no model at all
- a built-in local model
- a remote model endpoint

This routing layer should reduce token cost, improve privacy, avoid unnecessary remote calls, and keep Familiar responsive.

A small settings modal in the tray should expose only the minimum configuration needed for normal users.

## Depends On

- PRD-004: System Tray
- PRD-008: Local Model Manager
- PRD-009: Context Packer

## Goals

- Support intelligent local vs remote routing
- Support separate text-generation and embedding backends
- Allow local-only, remote-only, hybrid, or disabled operation
- Keep routing rules deterministic and explainable
- Keep the tray settings UI extremely simple
- Provide connection testing and status reporting

## Non-Goals

- No chat UI
- No prompt playground
- No quantization selector
- No VRAM charts
- No tokens/sec charts
- No dropdown for every possible model backend
- No advanced prompt template editing
- No full dashboard for model management

## High-Level Behavior

The router decides:

1. Does this task need a model at all?
2. If yes, should it use local or remote?
3. If local fails, should it fall back to remote?
4. If remote fails, should it fall back to local?
5. Is the content sensitive enough that it should stay local?
6. Is the task large enough that preprocessing should happen locally before escalation?

Example decisions:

- Tiny rename or metadata update → no model
- Small README update → local summary only
- Large code diff → local preprocess + remote summarize
- Sensitive project notes → local only
- Local model unavailable → remote fallback
- Remote endpoint unreachable → local fallback

## Configuration

Extend config with an inference section:

```toml
[inference.text]
mode = "hybrid"          # disabled | local_only | remote_only | hybrid
provider = "builtin"     # builtin | remote
builtin_model = "qwen2.5:3b"
remote_url = "http://localhost:11434/v1"
remote_api_key = ""
fallback_enabled = true
prefer_privacy = false
prefer_cost_savings = true

[inference.embedding]
provider = "builtin"     # builtin | remote
builtin_model = "nomic-embed-text"
remote_url = "http://localhost:11434/v1/embeddings"
remote_api_key = ""
fallback_enabled = true
```

## Core Types

```rust
pub enum InferenceMode {
    Disabled,
    LocalOnly,
    RemoteOnly,
    Hybrid,
}

pub enum ProviderKind {
    Builtin,
    Remote,
}

pub struct TextInferenceConfig {
    pub mode: InferenceMode,
    pub provider: ProviderKind,
    pub builtin_model: String,
    pub remote_url: Option<String>,
    pub remote_api_key: Option<String>,
    pub fallback_enabled: bool,
    pub prefer_privacy: bool,
    pub prefer_cost_savings: bool,
}

pub struct EmbeddingInferenceConfig {
    pub provider: ProviderKind,
    pub builtin_model: String,
    pub remote_url: Option<String>,
    pub remote_api_key: Option<String>,
    pub fallback_enabled: bool,
}
```

## Router Layer

Create a new router component:

```rust
pub struct InferenceRouter {
    text_config: TextInferenceConfig,
    embedding_config: EmbeddingInferenceConfig,
    local_manager: Arc<LlmManager>,
    remote_client: Arc<RemoteInferenceClient>,
}
```

Responsibilities:

- Decide whether to use a model
- Choose local vs remote
- Route summarize/classify/embed requests
- Apply fallback logic
- Return status and error details
- Expose simple routing decisions for logging/debugging

## Routing Heuristics

Initial heuristics should stay simple and deterministic.

### No-Model Cases

Do not use any model when:

- file change is trivial
- only whitespace changed
- only comments changed
- filename change only
- token estimate below minimum threshold
- task can be handled by keyword logic alone

### Local-Preferred Cases

Prefer local model when:

- content is small
- content contains sensitive/private data
- user selected local-only
- remote unavailable
- low latency desired
- cheap summarization/classification is sufficient

### Remote-Preferred Cases

Prefer remote model when:

- task is large
- task is complex
- context pack exceeds local threshold
- confidence from local classification is weak
- user selected remote-only

### Hybrid Behavior

In hybrid mode:

- local model performs first-pass summarization/classification
- local model may compress or pre-filter large inputs
- remote model used only when complexity threshold exceeded
- local model can generate routing hints for pack_for_task profile selection

## Suggested Built-In Models

Default text model:

- qwen2.5:3b

Optional stronger text model:

- qwen2.5:7b

Default embedding model:

- nomic-embed-text

## New Router APIs

```rust
async fn summarize(&self, input: &str) -> Result<String, InferenceError>;

async fn classify(
    &self,
    input: &str,
    labels: &[String],
) -> Result<String, InferenceError>;

async fn embed(&self, input: &str) -> Result<Vec<f32>, InferenceError>;

async fn choose_packer_profile(
    &self,
    task: &str,
) -> Result<BudgetProfile, InferenceError>;

async fn score_importance(
    &self,
    input: &str,
) -> Result<ImportanceScore, InferenceError>;
```

## Tray Settings UI

Add a minimal settings modal under tray Settings.

### Text Model Section

- Radio button: Built-in model
- Radio button: Remote endpoint
- Remote URL field
- Current backend label
- Status label
- Last error label
- Test connection button
- Optional load/unload button for local model

### Embedding Model Section

- Radio button: Built-in embedding model
- Radio button: Remote endpoint
- Remote URL field
- Current backend label
- Status label
- Last error label
- Test connection button

### Status Values

- Connected
- Unreachable
- Loading
- Disabled
- Authentication Failed
- Endpoint Not Found
- Model Not Loaded

## Connection Testing

Add lightweight connection checks:

- Built-in text model → local manager health_check()
- Remote text model → OpenAI-compatible /models endpoint
- Built-in embedding model → embedding backend health check
- Remote embedding model → remote embedding endpoint health check

Returned status should include:

```rust
pub struct BackendStatus {
    pub connected: bool,
    pub status_text: String,
    pub last_error: Option<String>,
    pub backend_name: Option<String>,
}
```

## Logging

Log routing decisions in debug mode.

Example:

```text
router_decision task=summarize_text route=local reason=small_input
router_decision task=pack_for_task route=remote reason=high_complexity
router_decision task=embed route=local reason=privacy_preferred
```

## Tests

- Disabled mode returns heuristic-only behavior
- Local-only never calls remote
- Remote-only never calls local
- Hybrid falls back correctly
- Sensitive input prefers local
- Remote unavailable triggers fallback
- Connection test success/failure
- Settings UI reflects backend status
- Invalid remote URL handled cleanly
- Router chooses expected path for simple vs complex tasks

## Acceptance Criteria

1. Familiar can run in disabled, local-only, remote-only, and hybrid modes
2. Text and embedding backends can be configured independently
3. Tray settings modal allows switching between local and remote providers
4. Connection test button reports success/failure
5. Last error is visible in settings modal
6. Router can choose local vs remote for summarize/classify/em