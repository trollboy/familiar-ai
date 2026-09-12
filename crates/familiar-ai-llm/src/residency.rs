//! PRD-073 residency vocabulary and resident-endpoint routing.
//!
//! A local serving process that outlives one execution turns two scarce
//! resources — load time and the prefix/KV cache — into assets that can be
//! reused. This module owns the honest language for that: whether a call
//! was served by an already-loaded process (`ResidencyState`), and what the
//! serving runtime actually reported about its prefix cache
//! (`CacheEvidence`).
//!
//! The two are deliberately independent. A warm process does not imply a
//! cache hit, and a runtime that reports no cache accounting leaves the
//! evidence `Unknown` with a stated reason rather than letting residency
//! stand in for a measurement nobody took
//! (`docs/contracts/local-worker-runtime.md`).
//!
//! Routing lives here too, and is a no-op by construction when residency is
//! off: [`ResidencyDirectory::resolve`] against an empty directory returns
//! the caller's own configured endpoint unchanged, so a workspace with no
//! residency configured reaches exactly the endpoint PRD-063 reaches today.

use std::collections::BTreeMap;

use crate::attempt::UsageCategories;

/// Whether the serving process behind one call was already loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyState {
    /// No resident process served this call: weights were loaded for it, or
    /// residency is off entirely.
    Cold,
    /// An already-running resident process served this call.
    Warm,
}

impl ResidencyState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "cold" => Self::Cold,
            "warm" => Self::Warm,
            _ => return None,
        })
    }
}

/// The reason a runtime's prefix-cache behavior could not be observed.
/// Stated, never implied.
pub const NO_CACHE_ACCOUNTING_REASON: &str =
    "serving runtime reported no prefix-cache accounting for this call";

/// What is actually known about prefix-cache reuse for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheEvidence {
    /// The process was loaded for this call, so there was no prior cache to
    /// hit. This is a fact about residency, not a cache measurement.
    ColdLoad,
    /// A resident process served the call and reported reading nothing from
    /// its cache.
    WarmMiss,
    /// A resident process served the call and reported reading cached
    /// prefix tokens.
    WarmHit,
    /// A resident process served the call and the runtime reported nothing
    /// either way.
    Unknown { reason: String },
}

impl CacheEvidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ColdLoad => "cold-load",
            Self::WarmMiss => "warm-miss",
            Self::WarmHit => "warm-hit",
            Self::Unknown { .. } => "unknown",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Unknown { reason } => Some(reason.as_str()),
            _ => None,
        }
    }

    /// Classifies one call from its residency state and whatever usage the
    /// runtime reported.
    ///
    /// A cold call is `ColdLoad` regardless of reported usage: there was no
    /// resident cache to hit, so any cache figure describes something other
    /// than residency reuse. A warm call is classified only from a reported
    /// `cache_read_tokens`; absent reporting is `Unknown`, never a
    /// fabricated miss. This is the distinction the second acceptance
    /// criterion requires the record to preserve.
    pub fn classify(state: ResidencyState, usage: &UsageCategories) -> Self {
        match state {
            ResidencyState::Cold => Self::ColdLoad,
            ResidencyState::Warm => match usage.cache_read_tokens {
                Some(tokens) if tokens > 0 => Self::WarmHit,
                Some(_) => Self::WarmMiss,
                None => Self::Unknown {
                    reason: NO_CACHE_ACCOUNTING_REASON.to_string(),
                },
            },
        }
    }
}

/// One running resident server, as routing sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentEndpoint {
    /// Identity of this server instance. A restart mints a new one: the
    /// process that served the previous call is not this process.
    pub server_identity: String,
    pub base_url: String,
}

/// Where one call should actually be sent, and what that implies for the
/// record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub base_url: String,
    pub residency_state: ResidencyState,
    /// `Some` exactly when a resident server took the call.
    pub resident_server_identity: Option<String>,
}

/// The resident servers currently held, keyed by the `worker_registry`
/// worker identity they serve.
///
/// An empty directory is the off-by-default case and is not a special path:
/// [`Self::resolve`] hands back the caller's configured endpoint verbatim,
/// `Cold`, with no server identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResidencyDirectory {
    residents: BTreeMap<String, ResidentEndpoint>,
}

impl ResidencyDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, worker_identity: &str, endpoint: ResidentEndpoint) {
        self.residents.insert(worker_identity.to_string(), endpoint);
    }

    pub fn remove(&mut self, worker_identity: &str) -> Option<ResidentEndpoint> {
        self.residents.remove(worker_identity)
    }

    pub fn get(&self, worker_identity: &str) -> Option<&ResidentEndpoint> {
        self.residents.get(worker_identity)
    }

    pub fn is_empty(&self) -> bool {
        self.residents.is_empty()
    }

    pub fn len(&self) -> usize {
        self.residents.len()
    }

    /// Routes one call. `configured_base_url` is the worker's own PRD-063
    /// endpoint — what is used whenever no resident serves this worker.
    pub fn resolve(&self, worker_identity: &str, configured_base_url: &str) -> ResolvedEndpoint {
        match self.residents.get(worker_identity) {
            Some(resident) => ResolvedEndpoint {
                base_url: resident.base_url.clone(),
                residency_state: ResidencyState::Warm,
                resident_server_identity: Some(resident.server_identity.clone()),
            },
            None => ResolvedEndpoint {
                base_url: configured_base_url.to_string(),
                residency_state: ResidencyState::Cold,
                resident_server_identity: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(cache_read: Option<u64>) -> UsageCategories {
        UsageCategories {
            uncached_input_tokens: Some(100),
            cache_read_tokens: cache_read,
            cache_write_tokens: None,
            output_tokens: Some(20),
            reasoning_output_tokens: None,
        }
    }

    #[test]
    fn cold_warm_miss_and_warm_hit_are_three_distinct_records() {
        assert_eq!(
            CacheEvidence::classify(ResidencyState::Cold, &usage(None)),
            CacheEvidence::ColdLoad
        );
        assert_eq!(
            CacheEvidence::classify(ResidencyState::Warm, &usage(Some(0))),
            CacheEvidence::WarmMiss
        );
        assert_eq!(
            CacheEvidence::classify(ResidencyState::Warm, &usage(Some(512))),
            CacheEvidence::WarmHit
        );
    }

    #[test]
    fn a_runtime_that_reports_no_cache_accounting_is_unknown_with_a_reason() {
        let evidence = CacheEvidence::classify(ResidencyState::Warm, &usage(None));
        assert_eq!(evidence.as_str(), "unknown");
        assert_eq!(evidence.reason(), Some(NO_CACHE_ACCOUNTING_REASON));
    }

    #[test]
    fn a_cold_call_is_never_reported_as_a_cache_hit_even_if_usage_claims_reads() {
        // Cache figures on a cold call describe something other than
        // residency reuse; residency never launders them into a hit.
        assert_eq!(
            CacheEvidence::classify(ResidencyState::Cold, &usage(Some(4096))),
            CacheEvidence::ColdLoad
        );
    }

    #[test]
    fn an_empty_directory_routes_to_the_configured_endpoint_unchanged() {
        let directory = ResidencyDirectory::new();
        let resolved = directory.resolve("llama3-ollama", "http://127.0.0.1:11434");
        assert_eq!(
            resolved,
            ResolvedEndpoint {
                base_url: "http://127.0.0.1:11434".into(),
                residency_state: ResidencyState::Cold,
                resident_server_identity: None,
            }
        );
    }

    #[test]
    fn a_resident_takes_the_call_and_names_itself() {
        let mut directory = ResidencyDirectory::new();
        directory.insert(
            "llama3-ollama",
            ResidentEndpoint {
                server_identity: "resident-1".into(),
                base_url: "http://127.0.0.1:21434".into(),
            },
        );
        let resolved = directory.resolve("llama3-ollama", "http://127.0.0.1:11434");
        assert_eq!(resolved.base_url, "http://127.0.0.1:21434");
        assert_eq!(resolved.residency_state, ResidencyState::Warm);
        assert_eq!(
            resolved.resident_server_identity.as_deref(),
            Some("resident-1")
        );
        // An unrelated worker is untouched by another worker's resident.
        let other = directory.resolve("qwen-unsloth", "http://127.0.0.1:11435");
        assert_eq!(other.residency_state, ResidencyState::Cold);
        assert_eq!(other.base_url, "http://127.0.0.1:11435");
    }
}
