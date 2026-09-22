//! Bounded, versioned dispatch for desktop operator requests.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use familiar_ai_core::operator_ui::{
    OperatorDataSource, OperatorError, OperatorEvent, OperatorMutation, OperatorPayload,
    OperatorQuery, OperatorReply, OPERATOR_PROTOCOL_VERSION,
};

const MAX_DEDUPLICATION_ENTRIES: usize = 1_024;

pub struct OperatorDispatcher {
    source: Arc<dyn OperatorDataSource>,
    daemon_generation: u64,
    revision: AtomicU64,
    completed: Mutex<(HashMap<String, OperatorReply>, VecDeque<String>)>,
    events: Mutex<VecDeque<OperatorEvent>>,
}

impl OperatorDispatcher {
    pub fn new(source: Arc<dyn OperatorDataSource>, daemon_generation: u64) -> Self {
        Self {
            source,
            daemon_generation,
            revision: AtomicU64::new(0),
            completed: Mutex::new((HashMap::new(), VecDeque::new())),
            events: Mutex::new(VecDeque::new()),
        }
    }

    pub fn query(&self, query: OperatorQuery) -> Result<OperatorReply, OperatorError> {
        query.validate()?;
        let kind = query.kind();
        let data = self.source.query(query).map_err(sanitize_source_error)?;
        Ok(self.reply(
            OperatorPayload::Query { query: kind, data },
            false,
            self.revision.load(Ordering::SeqCst),
        ))
    }

    pub fn mutate(&self, mutation: OperatorMutation) -> Result<OperatorReply, OperatorError> {
        mutation.validate()?;
        let mut completed = self
            .completed
            .lock()
            .map_err(|_| OperatorError::unavailable("operator request state is unavailable"))?;
        if let Some(prior) = completed.0.get(&mutation.idempotency_key) {
            let mut duplicate = prior.clone();
            duplicate.duplicate = true;
            return Ok(duplicate);
        }

        // The lock intentionally spans the mutation. A retry with the same key
        // cannot race the original and perform a destructive action twice.
        let topic = mutation.action.name().to_string();
        let kind = mutation.action.kind();
        let data = self
            .source
            .act(mutation.action)
            .map_err(sanitize_source_error)?;
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let reply = self.reply(
            OperatorPayload::Mutation { action: kind, data },
            false,
            revision,
        );
        completed
            .0
            .insert(mutation.idempotency_key.clone(), reply.clone());
        completed.1.push_back(mutation.idempotency_key);
        while completed.1.len() > MAX_DEDUPLICATION_ENTRIES {
            if let Some(expired) = completed.1.pop_front() {
                completed.0.remove(&expired);
            }
        }
        let mut events = self
            .events
            .lock()
            .map_err(|_| OperatorError::unavailable("operator event state is unavailable"))?;
        events.push_back(OperatorEvent {
            protocol_version: OPERATOR_PROTOCOL_VERSION,
            daemon_generation: self.daemon_generation,
            sequence: revision,
            topic,
        });
        while events.len() > MAX_DEDUPLICATION_ENTRIES {
            events.pop_front();
        }
        Ok(reply)
    }

    /// Publish one ordered operator change event that was not caused by a
    /// client mutation — a backlog reconciliation committed by the daemon's
    /// own watcher/startup/read-fallback paths (PRD-108), rather than
    /// something a Tauri or GTK client asked for through [`Self::mutate`].
    /// Bumps the revision the same way `mutate` does, so `observe` callers
    /// cannot distinguish the two — a connected client refreshes either way.
    pub fn record_event(&self, topic: impl Into<String>) -> Result<u64, OperatorError> {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let mut events = self
            .events
            .lock()
            .map_err(|_| OperatorError::unavailable("operator event state is unavailable"))?;
        events.push_back(OperatorEvent {
            protocol_version: OPERATOR_PROTOCOL_VERSION,
            daemon_generation: self.daemon_generation,
            sequence: revision,
            topic: topic.into(),
        });
        while events.len() > MAX_DEDUPLICATION_ENTRIES {
            events.pop_front();
        }
        Ok(revision)
    }

    pub fn observe(&self, after: u64, limit: usize) -> Result<Vec<OperatorEvent>, OperatorError> {
        if limit == 0 || limit > 200 {
            return Err(OperatorError::invalid(
                "event limit must be between 1 and 200",
            ));
        }
        let events = self
            .events
            .lock()
            .map_err(|_| OperatorError::unavailable("operator event state is unavailable"))?;
        if let Some(first) = events.front() {
            if after > 0 && after + 1 < first.sequence {
                return Err(OperatorError {
                    code: "event_gap".into(),
                    message: "operator event history has advanced; refresh the snapshot".into(),
                    retryable: true,
                });
            }
        }
        Ok(events
            .iter()
            .filter(|event| event.sequence > after)
            .take(limit)
            .cloned()
            .collect())
    }

    fn reply(&self, payload: OperatorPayload, duplicate: bool, revision: u64) -> OperatorReply {
        OperatorReply {
            protocol_version: OPERATOR_PROTOCOL_VERSION,
            daemon_generation: self.daemon_generation,
            revision,
            payload,
            duplicate,
        }
    }
}

fn sanitize_source_error(message: String) -> OperatorError {
    // Source errors are already intended for the operator, but never reflect
    // them through Debug formatting where headers or nested request values can
    // accidentally appear.
    OperatorError {
        code: "operation_failed".into(),
        message,
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::operator_ui::OperatorAction;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake(AtomicUsize);
    impl OperatorDataSource for Fake {
        fn query(&self, _: OperatorQuery) -> Result<serde_json::Value, String> {
            Ok(json!({"ok": true}))
        }
        fn act(&self, _: OperatorAction) -> Result<serde_json::Value, String> {
            Ok(json!({"count": self.0.fetch_add(1, Ordering::SeqCst) + 1}))
        }
    }

    #[test]
    fn mutation_replay_is_not_performed_twice() {
        let source = Arc::new(Fake(AtomicUsize::new(0)));
        let dispatch = OperatorDispatcher::new(source.clone(), 7);
        let mutation = OperatorMutation {
            request_id: "request-1".into(),
            idempotency_key: "click-1".into(),
            action: OperatorAction::SetProjectPaused {
                repo: "/r".into(),
                paused: true,
            },
        };
        let first = dispatch.mutate(mutation.clone()).unwrap();
        let second = dispatch.mutate(mutation).unwrap();
        assert!(!first.duplicate);
        assert!(second.duplicate);
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
        assert_eq!(first.daemon_generation, 7);
    }
}
