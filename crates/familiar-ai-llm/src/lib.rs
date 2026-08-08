//! Local LLM manager + pluggable backends.
//!
//! The manager owns lifecycle and health state. Backends are dumb adapters.
//! v1 ships two backends: `StubBackend` (no network) and `OpenAiHttpBackend`
//! (any OpenAI-compatible HTTP server — Ollama, LM Studio, llama.cpp, vLLM,
//! OpenRouter, etc.). mistral.rs and true in-process inference are out of
//! scope for this PRD.

pub mod backend;
pub mod backends;
pub mod error;
pub mod factory;
pub mod heuristics;
pub mod manager;
pub mod router;
pub mod types;

pub use backend::LlmBackend;
pub use error::LlmError;
pub use factory::BackendParams;
pub use manager::LlmManager;
pub use router::InferenceRouter;
pub use types::{HealthStatus, LlmHealthState};
