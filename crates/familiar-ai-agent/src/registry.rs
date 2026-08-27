//! Adapter-neutral worker capabilities and deterministic policy routing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::{ClaudeCodeAgent, ClaudeCodeSettings, CodexAgent, CodingAgent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkerStage {
    Planning,
    Implementation,
    Review,
    Remediation,
    NarrowTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkerCapability {
    Planning,
    Implementation,
    Review,
    Remediation,
    NarrowTask,
}

impl From<WorkerStage> for WorkerCapability {
    fn from(value: WorkerStage) -> Self {
        match value {
            WorkerStage::Planning => Self::Planning,
            WorkerStage::Implementation => Self::Implementation,
            WorkerStage::Review => Self::Review,
            WorkerStage::Remediation => Self::Remediation,
            WorkerStage::NarrowTask => Self::NarrowTask,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDescriptor {
    pub id: String,
    pub adapter_id: String,
    pub provider: String,
    pub model: String,
    pub executable: String,
    pub capabilities: BTreeSet<WorkerCapability>,
    pub fresh_process_isolation: bool,
    pub context_tokens: u64,
    pub estimated_cost_microusd: u64,
    pub available: bool,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub extra_args: Vec<String>,
}

/// Contract implemented by built-in and future Fable/OpenCode adapters.
/// Orchestration deals only in descriptors and this constructor boundary.
pub trait AdapterFactory: Send + Sync {
    fn adapter_id(&self) -> &str;
    fn build(&self, worker: &WorkerDescriptor) -> Result<Box<dyn CodingAgent>, String>;
}

/// Adapter construction registry. Adding an adapter means registering another
/// factory here; execution orchestration remains unchanged.
#[derive(Default)]
pub struct AdapterFactories {
    factories: BTreeMap<String, Box<dyn AdapterFactory>>,
}

impl AdapterFactories {
    pub fn register(&mut self, factory: Box<dyn AdapterFactory>) -> Result<(), String> {
        let id = factory.adapter_id().to_owned();
        if id.trim().is_empty() {
            return Err("adapter factory id must be non-empty".into());
        }
        if self.factories.insert(id.clone(), factory).is_some() {
            return Err(format!("duplicate adapter factory {id:?}"));
        }
        Ok(())
    }

    pub fn build(&self, worker: &WorkerDescriptor) -> Result<Box<dyn CodingAgent>, String> {
        self.factories
            .get(&worker.adapter_id)
            .ok_or_else(|| format!("no adapter factory registered for {:?}", worker.adapter_id))?
            .build(worker)
    }
}

struct CodexFactory {
    id: &'static str,
}

impl AdapterFactory for CodexFactory {
    fn adapter_id(&self) -> &str {
        self.id
    }

    fn build(&self, worker: &WorkerDescriptor) -> Result<Box<dyn CodingAgent>, String> {
        Ok(Box::new(CodexAgent::new(worker.executable.clone())))
    }
}

struct ClaudeCodeFactory;

impl AdapterFactory for ClaudeCodeFactory {
    fn adapter_id(&self) -> &str {
        "claude-code"
    }

    fn build(&self, worker: &WorkerDescriptor) -> Result<Box<dyn CodingAgent>, String> {
        Ok(Box::new(ClaudeCodeAgent::new(ClaudeCodeSettings {
            executable: worker.executable.clone(),
            model: (!worker.model.is_empty()).then(|| worker.model.clone()),
            effort: worker.effort.clone(),
            permission_mode: worker.permission_mode.clone(),
            max_budget_microusd: None,
            extra_args: worker.extra_args.clone(),
        })))
    }
}

pub fn builtin_adapter_factories() -> AdapterFactories {
    let mut factories = AdapterFactories::default();
    factories
        .register(Box::new(CodexFactory { id: "codex" }))
        .unwrap();
    factories
        .register(Box::new(CodexFactory { id: "ollama" }))
        .unwrap();
    factories.register(Box::new(ClaudeCodeFactory)).unwrap();
    factories
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRequest {
    pub stage: WorkerStage,
    pub pinned_worker: Option<String>,
    pub max_cost_microusd: u64,
    pub required_context_tokens: u64,
    pub require_isolation: bool,
    pub independent_from: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectionReason {
    Unavailable,
    OverBudget,
    Incapable,
    InsufficientContext,
    NoIsolation,
    NotIndependent,
    NotPinned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateEvaluation {
    pub worker_id: String,
    pub rejected: Vec<RejectionReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionRecord {
    pub stage: WorkerStage,
    pub rule: String,
    pub selected_worker: String,
    pub candidates: Vec<CandidateEvaluation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    UnknownPinnedWorker(String),
    PinnedWorkerRefused {
        worker: String,
        reasons: Vec<RejectionReason>,
    },
    NoEligibleWorker(WorkerStage),
    NoIndependentReviewer,
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPinnedWorker(id) => write!(f, "pinned worker {id:?} is not registered"),
            Self::PinnedWorkerRefused { worker, reasons } => {
                write!(f, "pinned worker {worker:?} was refused: {reasons:?}")
            }
            Self::NoEligibleWorker(stage) => write!(f, "no eligible worker for {stage:?}"),
            Self::NoIndependentReviewer => f.write_str("NoIndependentReviewer"),
        }
    }
}
impl std::error::Error for RouteError {}

#[derive(Debug, Clone, Default)]
pub struct WorkerRegistry {
    workers: BTreeMap<String, WorkerDescriptor>,
}

impl WorkerRegistry {
    pub fn register(&mut self, worker: WorkerDescriptor) -> Result<(), String> {
        if worker.id.trim().is_empty()
            || worker.adapter_id.trim().is_empty()
            || worker.provider.trim().is_empty()
            || worker.model.trim().is_empty()
            || worker.executable.trim().is_empty()
        {
            return Err(
                "worker identity, adapter, provider, model, and executable must be non-empty"
                    .into(),
            );
        }
        if self.workers.insert(worker.id.clone(), worker).is_some() {
            return Err("duplicate worker id".into());
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&WorkerDescriptor> {
        self.workers.get(id)
    }

    pub fn select(&self, request: &RouteRequest) -> Result<SelectionRecord, RouteError> {
        if let Some(pin) = &request.pinned_worker {
            if !self.workers.contains_key(pin) {
                return Err(RouteError::UnknownPinnedWorker(pin.clone()));
            }
        }
        let mut candidates = Vec::new();
        for worker in self.workers.values() {
            let mut rejected = Vec::new();
            if request
                .pinned_worker
                .as_ref()
                .is_some_and(|pin| pin != &worker.id)
            {
                rejected.push(RejectionReason::NotPinned);
            }
            if !worker.available {
                rejected.push(RejectionReason::Unavailable);
            }
            if !worker.capabilities.contains(&request.stage.into()) {
                rejected.push(RejectionReason::Incapable);
            }
            if request.max_cost_microusd > 0
                && worker.estimated_cost_microusd > request.max_cost_microusd
            {
                rejected.push(RejectionReason::OverBudget);
            }
            if worker.context_tokens < request.required_context_tokens {
                rejected.push(RejectionReason::InsufficientContext);
            }
            if request.require_isolation && !worker.fresh_process_isolation {
                rejected.push(RejectionReason::NoIsolation);
            }
            if request.independent_from.as_ref().is_some_and(|identity| {
                identity == &(worker.provider.clone(), worker.model.clone())
            }) {
                rejected.push(RejectionReason::NotIndependent);
            }
            candidates.push(CandidateEvaluation {
                worker_id: worker.id.clone(),
                rejected,
            });
        }
        let selected = candidates
            .iter()
            .filter(|c| c.rejected.is_empty())
            .map(|c| self.workers.get(&c.worker_id).unwrap())
            .min_by_key(|w| (w.estimated_cost_microusd, w.id.as_str()));
        let Some(selected) = selected else {
            if let Some(pin) = &request.pinned_worker {
                let reasons = candidates
                    .iter()
                    .find(|c| &c.worker_id == pin)
                    .unwrap()
                    .rejected
                    .clone();
                return Err(RouteError::PinnedWorkerRefused {
                    worker: pin.clone(),
                    reasons,
                });
            }
            if request.stage == WorkerStage::Review && request.independent_from.is_some() {
                return Err(RouteError::NoIndependentReviewer);
            }
            return Err(RouteError::NoEligibleWorker(request.stage));
        };
        Ok(SelectionRecord {
            stage: request.stage,
            rule: if request.pinned_worker.is_some() {
                "user-pin"
            } else {
                "lowest-cost-then-id"
            }
            .into(),
            selected_worker: selected.id.clone(),
            candidates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CustomFactory;
    impl AdapterFactory for CustomFactory {
        fn adapter_id(&self) -> &str {
            "custom"
        }
        fn build(&self, worker: &WorkerDescriptor) -> Result<Box<dyn CodingAgent>, String> {
            Ok(Box::new(CodexAgent::new(worker.executable.clone())))
        }
    }
    fn worker(id: &str, cost: u64) -> WorkerDescriptor {
        WorkerDescriptor {
            id: id.into(),
            adapter_id: "test".into(),
            provider: "p".into(),
            model: id.into(),
            executable: "test".into(),
            capabilities: [WorkerCapability::Implementation, WorkerCapability::Review].into(),
            fresh_process_isolation: true,
            context_tokens: 100,
            estimated_cost_microusd: cost,
            available: true,
            effort: None,
            permission_mode: None,
            extra_args: Vec::new(),
        }
    }
    #[test]
    fn selection_is_cost_then_id_deterministic() {
        let mut r = WorkerRegistry::default();
        r.register(worker("z", 1)).unwrap();
        r.register(worker("a", 1)).unwrap();
        let q = RouteRequest {
            stage: WorkerStage::Implementation,
            pinned_worker: None,
            max_cost_microusd: 0,
            required_context_tokens: 0,
            require_isolation: false,
            independent_from: None,
        };
        assert_eq!(r.select(&q).unwrap().selected_worker, "a");
    }
    #[test]
    fn exact_pin_is_used_or_refused() {
        let mut r = WorkerRegistry::default();
        let mut w = worker("pinned", 1);
        w.available = false;
        r.register(w).unwrap();
        let q = RouteRequest {
            stage: WorkerStage::Implementation,
            pinned_worker: Some("pinned".into()),
            max_cost_microusd: 0,
            required_context_tokens: 0,
            require_isolation: false,
            independent_from: None,
        };
        assert!(matches!(
            r.select(&q),
            Err(RouteError::PinnedWorkerRefused { .. })
        ));
    }
    #[test]
    fn reviewer_must_be_independent() {
        let mut r = WorkerRegistry::default();
        r.register(worker("same", 1)).unwrap();
        let q = RouteRequest {
            stage: WorkerStage::Review,
            pinned_worker: None,
            max_cost_microusd: 0,
            required_context_tokens: 0,
            require_isolation: true,
            independent_from: Some(("p".into(), "same".into())),
        };
        assert_eq!(r.select(&q), Err(RouteError::NoIndependentReviewer));
    }

    #[test]
    fn conforming_factory_is_added_without_orchestration_changes() {
        let mut factories = AdapterFactories::default();
        factories.register(Box::new(CustomFactory)).unwrap();
        let mut descriptor = worker("custom-worker", 1);
        descriptor.adapter_id = "custom".into();
        assert!(factories.build(&descriptor).is_ok());
    }
}
