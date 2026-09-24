//! Answers the tray windows' queries and performs their actions.
//!
//! The tray cannot depend on this crate (this crate depends on the tray), so
//! it declares what it needs as [`Query`]/[`Action`] and this implements them
//! over exactly the functions the dashboard's HTTP handlers, the control
//! socket and the CLI use. One source of truth: a window and the other
//! surfaces cannot drift into disagreeing about the same repository.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use familiar_ai_core::control_plane::{
    Authority, CapabilityScope, ClientClass, ExecutionMode, Submission,
};
use familiar_ai_core::operator_ui::{
    ConfigEdit, OperatorAction as Action, OperatorDataSource as DataSource, OperatorQuery as Query,
};
use familiar_ai_core::{
    validate_recovery_attribution, BacklogDiscovery, BacklogRecoveryAction,
    FilesystemBacklogDiscovery,
};
use familiar_ai_llm::InferenceRouter;
use familiar_ai_storage::{Database, SqliteBacklogRepository};

use crate::backlog_reconciler::BacklogReconciler;
use crate::control_plane::ControlPlaneService;
use crate::stewardship;

pub struct DaemonDataSource {
    db: Arc<Mutex<Database>>,
    router: Arc<InferenceRouter>,
    runtime: tokio::runtime::Handle,
    control: ControlPlaneService,
    paths: familiar_ai_core::AppPaths,
    /// The same status the tray menu reads. Configuring inference has to
    /// update it, or the menu keeps offering to configure something that is
    /// now configured.
    status: Arc<Mutex<familiar_ai_core::AppStatus>>,
    shutdown: Option<tokio::sync::watch::Sender<bool>>,
    /// PRD-108: bounded reconcile-on-read fallback, shared with the watcher
    /// and startup reconciliation paths, so backlog and dependency queries
    /// read one reconciled repository view instead of drifting apart.
    reconciler: Arc<BacklogReconciler>,
}

impl DaemonDataSource {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<Mutex<Database>>,
        router: Arc<InferenceRouter>,
        runtime: tokio::runtime::Handle,
        control: ControlPlaneService,
        paths: familiar_ai_core::AppPaths,
        status: Arc<Mutex<familiar_ai_core::AppStatus>>,
        shutdown: Option<tokio::sync::watch::Sender<bool>>,
        reconciler: Arc<BacklogReconciler>,
    ) -> Self {
        Self {
            db,
            router,
            runtime,
            control,
            paths,
            status,
            shutdown,
            reconciler,
        }
    }

    /// Resolves a repository the same way the dashboard's mandatory `repo`
    /// parameter does, so a window and a URL naming the same path address the
    /// same repository.
    fn identity(repo: &str) -> Result<familiar_ai_core::RepositoryIdentity, String> {
        FilesystemBacklogDiscovery
            .resolve(std::path::Path::new(repo))
            .map_err(|e| e.to_string())
    }

    /// The window runs inside the daemon that owns the control plane, so it
    /// holds operator authority directly rather than minting a session for
    /// itself over the socket. Scoped to no particular project because the
    /// window switches between them.
    fn operator_scope() -> CapabilityScope {
        CapabilityScope {
            client_class: ClientClass::Operator,
            project_id: None,
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control, Authority::Observe],
        }
    }

    /// A repository's path is its control-plane project id. The two registries
    /// are keyed differently — the backlog by git directory, the control plane
    /// by project id — and this is the one place that decides the mapping.
    fn project_id(repo: &str) -> &str {
        repo
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.runtime.block_on(future))
        } else {
            self.runtime.block_on(future)
        }
    }

    fn repositories(&self) -> Result<Value, String> {
        let mut value = {
            let db = self
                .db
                .lock()
                .map_err(|_| "database lock poisoned".to_string())?;
            stewardship::list_repositories(&db).map_err(|e| e.to_string())?
        };
        let mut identities = Vec::new();
        for configured in self.reconciler.configured_repositories() {
            let identity = FilesystemBacklogDiscovery
                .resolve(&configured)
                .map_err(|e| format!("configured repository {}: {e}", configured.display()))?;
            identities.push(identity);
        }
        merge_configured_repositories(&mut value, identities)?;
        Ok(value)
    }

    /// Every PRD the repository declares, needed to resolve a supplied path to
    /// a real backlog entry before mutating it.
    /// The repository's configured backlog layout: directories, metadata
    /// policy and risk vocabulary. Every discovery and every lifecycle
    /// derivation goes through this so the tray and the desktop read the
    /// same PRD files the driver does.
    fn layout(&self, repo: &str) -> Result<familiar_ai_core::BacklogLayout, String> {
        let config = crate::cli::shared::effective_repository_config(
            &self.paths,
            std::path::Path::new(repo),
        )?;
        Ok(config
            .repository(std::path::Path::new(repo))
            .map_err(|e| e.to_string())?
            .layout())
    }

    fn discovered(&self, repo: &str) -> Result<Vec<familiar_ai_core::DiscoveredPrd>, String> {
        let repository = Self::identity(repo)?;
        let layout = self.layout(repo)?;
        FilesystemBacklogDiscovery
            .discover_with_layout(&repository, &layout)
            .map_err(|e| e.to_string())
    }

    fn recover(
        &self,
        repo: &str,
        prd_path: &str,
        action: BacklogRecoveryAction,
        actor: &str,
        reason: &str,
    ) -> Result<Value, String> {
        // The same three steps `familiar-ai backlog release|complete` runs, in
        // the same order: attribution is validated before anything is written,
        // so an anonymous or unexplained mutation is refused up front.
        validate_recovery_attribution(action, actor, reason).map_err(|e| e.to_string())?;
        let repository = Self::identity(repo)?;
        let discovered = self.discovered(repo)?;
        let target = familiar_ai_core::resolve_run_prd(
            &repository,
            &discovered,
            std::path::Path::new(prd_path),
        )
        .map_err(|e| e.to_string())?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let result = SqliteBacklogRepository::new(db.conn_mut())
            .recover(&repository, &target, action, actor, reason)
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "prd_id": result.prd.id.to_string(),
            "prd_path": result.prd.path.to_string(),
            "status": format!("{:?}", action),
        }))
    }

    /// What each dropdown may offer, and what this machine can actually use.
    ///
    /// Adapters come from the built-in registry and providers from the
    /// operator's own config, so the lists cannot drift from what the code
    /// accepts. Availability is probed rather than assumed: an adapter whose
    /// executable is not installed, or a provider whose credential does not
    /// resolve, is still listed — hiding it would leave the operator unable to
    /// see why a choice is missing — but it is labelled as unavailable.
    fn config_choices(&self) -> Value {
        let config = familiar_ai_core::Config::load(Some(&self.config_path())).ok();

        // FAM-BUG-094: `agents.<role>.adapter` is the closed
        // `AgentAdapterKind` enum. The form used to offer every runtime
        // factory id here (`openai-api`, `anthropic-api`, ...), which the
        // config rejects at startup. Those ids belong to a worker's
        // `runtime` field and are offered there, as `runtimes`.
        let adapters: Vec<Value> = ADAPTER_KINDS
            .iter()
            .map(|(id, executable, detail)| {
                let found = which_on_path(executable);
                let (models, models_source) = self.adapter_models(id, config.as_ref());
                let is_cli = which_matters(id);
                json!({
                    "value": id,
                    "available": !is_cli || found.is_some(),
                    "detail": match (&found, is_cli) {
                        (_, false) => detail.to_string(),
                        (Some(path), true) => format!("{executable} at {path}"),
                        (None, true) => format!("{executable} not on PATH"),
                    },
                    // The adapter supplies its executable and the models it
                    // can drive; the settings form offers exactly these.
                    "executable": executable,
                    "executable_path": found,
                    "models": models,
                    "models_source": models_source,
                })
            })
            .collect();
        let factories = familiar_ai_agent::builtin_adapter_factories();
        let runtimes: Vec<Value> = factories
            .ids()
            .into_iter()
            .map(|id| {
                let (models, models_source) = self.adapter_models(id.as_str(), config.as_ref());
                json!({
                    "value": id,
                    "available": true,
                    "models": models,
                    "models_source": models_source,
                })
            })
            .collect();

        let mut providers = Vec::new();
        let mut models: Vec<String> = Vec::new();
        if let Some(config) = &config {
            for (name, provider) in &config.providers {
                let auth = crate::config_cli::check_auth(&provider.auth);
                providers.push(json!({
                    "value": name,
                    "available": matches!(auth, Ok(Some(_))),
                    "detail": match &auth {
                        Ok(Some(_)) => "credential resolves".to_string(),
                        Ok(None) => "no credential configured".to_string(),
                        Err(e) => e.clone(),
                    },
                }));
                models.extend(provider.models.iter().cloned());
            }
        }
        models.sort();
        models.dedup();

        json!({
            "adapters": adapters,
            "runtimes": runtimes,
            "permission_modes": ["default", "plan", "acceptEdits", "bypassPermissions"],
            "inference_modes": ["disabled", "local_only", "remote_only", "hybrid"],
            "prd_metadata_policies": ["incremental", "strict"],
            "providers": providers,
            "models": models,
        })
    }

    /// The models an adapter can be pointed at, and where that list came
    /// from. Vendor CLIs take aliases; Codex keeps a model cache on disk;
    /// local runtimes are asked over `/v1/models`; API runtimes list what the
    /// configured providers of that kind declare, with the current family as
    /// a fallback so the dropdown is never empty.
    fn adapter_models(
        &self,
        adapter: &str,
        config: Option<&familiar_ai_core::Config>,
    ) -> (Vec<String>, String) {
        let provider_models = |kind_fragment: &str| -> Vec<String> {
            config
                .map(|config| {
                    config
                        .providers
                        .iter()
                        .filter(|(_, provider)| {
                            format!("{:?}", provider.kind)
                                .to_ascii_lowercase()
                                .contains(kind_fragment)
                        })
                        .flat_map(|(_, provider)| provider.models.iter().cloned())
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut result = match adapter {
            "claude-code" => (
                vec![
                    "opus".to_string(),
                    "sonnet".to_string(),
                    "haiku".to_string(),
                ],
                "Claude Code model aliases".to_string(),
            ),
            "codex" => {
                let cache = std::env::var_os("CODEX_HOME")
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME")
                            .map(|home| std::path::PathBuf::from(home).join(".codex"))
                    })
                    .map(|home| home.join("models_cache.json"));
                let cached: Vec<String> = cache
                    .as_ref()
                    .and_then(|path| std::fs::read(path).ok())
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .and_then(|value| value.get("models").and_then(Value::as_array).cloned())
                    .map(|models| {
                        models
                            .iter()
                            .filter_map(|model| {
                                ["slug", "id", "name"]
                                    .iter()
                                    .find_map(|key| model.get(*key).and_then(Value::as_str))
                            })
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if cached.is_empty() {
                    (
                        vec!["gpt-5-codex".to_string(), "gpt-5".to_string()],
                        "built-in defaults; ~/.codex/models_cache.json not found".to_string(),
                    )
                } else {
                    (
                        cached,
                        cache
                            .map(|path| path.display().to_string())
                            .unwrap_or_default(),
                    )
                }
            }
            "ollama" | "unsloth" => {
                let url = self.builtin_endpoint().map(|(url, _)| url).or_else(|| {
                    (adapter == "ollama" && which_on_path("ollama").is_some())
                        .then(|| "http://127.0.0.1:11434".to_string())
                });
                match url {
                    Some(url) => match self
                        .probe_models(&url, &familiar_ai_core::config::AuthDescriptor::None)
                    {
                        Ok(models) => (models, url),
                        Err(error) => (Vec::new(), format!("{url}: {error}")),
                    },
                    None => (
                        Vec::new(),
                        "no local endpoint configured or found".to_string(),
                    ),
                }
            }
            "anthropic-api" => {
                let mut models = provider_models("anthropic");
                for default in [
                    "claude-fable-5-1",
                    "claude-opus-5",
                    "claude-sonnet-5",
                    "claude-haiku-4-5-20251001",
                ] {
                    models.push(default.to_string());
                }
                (
                    models,
                    "configured providers and the current Claude family".to_string(),
                )
            }
            "openai-api" => {
                let mut models = provider_models("openai");
                for default in ["gpt-5", "gpt-5-mini"] {
                    models.push(default.to_string());
                }
                (
                    models,
                    "configured providers and current defaults".to_string(),
                )
            }
            "raw-agent-loop" => (
                provider_models(""),
                "configured providers; the runtime is chosen per worker".to_string(),
            ),
            _ => (provider_models(""), "configured providers".to_string()),
        };
        result.0.sort();
        result.0.dedup();
        result
    }

    /// Which PRDs are waiting on other PRDs.
    ///
    /// Applies the same rule the runner does — a dependency counts as met only
    /// when its backlog status is `completed` — so the form cannot offer to
    /// start something the backlog would then refuse. A dependency that is not
    /// discovered at all is unmet too, and named, because that is a real state
    /// this backlog reaches when a PRD is moved or deleted.
    fn dependencies(&self, repo: &str) -> Result<Value, String> {
        // PRD-108: repair whatever a watcher gap missed before joining
        // discovery against the ledger, bounded so this cannot become a
        // continuous full-tree scan under repeated polling.
        self.reconciler
            .reconcile_if_stale(std::path::Path::new(repo));
        let discovered = self.discovered(repo)?;
        let identity = Self::identity(repo)?;
        let statuses: std::collections::HashMap<String, (String, String)> = {
            let db = self
                .db
                .lock()
                .map_err(|_| "database lock poisoned".to_string())?;
            let layout = self.layout(repo).ok();
            let backlog =
                stewardship::list_backlog(&db, &identity, layout.as_ref(), None, None, 2000)
                    .map_err(|e| e.to_string())?;
            backlog
                .get("items")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            Some((
                                item.get("prd_path")?.as_str()?.to_string(),
                                (
                                    item.get("status")?.as_str()?.to_string(),
                                    item.get("lifecycle")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                ),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        // FAM-BUG-088: only work that can still run can conflict. Archived
        // PRDs (some predate the Expected Files grammar entirely) and
        // completed ones used to go through the scope loader too, and one
        // legacy file without a heading emptied the conflict map for the
        // whole chart, so three PRDs the scheduler serializes were drawn as
        // one launchable wave.
        let candidates: Vec<familiar_ai_core::DiscoveredPrd> = discovered
            .iter()
            .filter(|prd| prd.location == familiar_ai_core::PrdLocation::Active)
            .filter(|prd| {
                statuses
                    .get(prd.path.as_str())
                    .map(|(status, lifecycle)| lifecycle != "completed" && status != "completed")
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        // A wave is dependency-ready AND scope-disjoint (the owner's
        // definition, EXECUTION-PLAN). The chart used to lay out dependency
        // layers only, so it showed PRDs side by side that the scheduler
        // would serialize; these are the scheduler's own conflict edges.
        // If the scheduler cannot compute overlaps, say so on the payload
        // rather than silently drawing dependency layers as if they were
        // rounds — a chart that degrades without saying it degraded is how
        // three conflicting PRDs get launched as one wave.
        let (conflicts, conflicts_error): (
            std::collections::HashMap<String, Vec<String>>,
            Option<String>,
        ) = match crate::drive::achievable_width(std::path::Path::new(repo), &candidates) {
            Ok(width) => {
                let mut map: std::collections::HashMap<String, Vec<String>> =
                    std::collections::HashMap::new();
                for (a, b, _) in width.conflicts {
                    map.entry(a.to_string()).or_default().push(b.to_string());
                    map.entry(b.to_string()).or_default().push(a.to_string());
                }
                (map, None)
            }
            Err(error) => {
                tracing::warn!(repo, %error, "dependency view: scope conflicts unavailable; waves are dependency layers only");
                (std::collections::HashMap::new(), Some(error))
            }
        };
        let status_of_id: std::collections::HashMap<String, Option<&String>> = discovered
            .iter()
            .map(|prd| {
                (
                    prd.id.to_string(),
                    statuses.get(prd.path.as_str()).map(|(status, _)| status),
                )
            })
            .collect();

        let items: Vec<Value> = discovered
            .iter()
            .map(|prd| {
                let depends_on: Vec<Value> = prd
                    .dependencies
                    .iter()
                    .map(|dep| {
                        let dep_id = dep.to_string();
                        // PRD-108: `status_of_id` holding the key at all
                        // means the dependency's file was discovered; only
                        // its ledger row is missing (reconciliation has not
                        // caught up yet). That is never "not found" — the
                        // file is right there — so it is named for what it
                        // actually is: not yet enrolled in the ledger.
                        let status = match status_of_id.get(&dep_id) {
                            Some(Some(status)) => status.as_str(),
                            Some(None) => "unenrolled",
                            None => "not found",
                        };
                        json!({"prd_id": dep_id, "status": status})
                    })
                    .collect();
                let blocked_by: Vec<Value> = prd
                    .dependencies
                    .iter()
                    .filter_map(|dep| {
                        let dep_id = dep.to_string();
                        match status_of_id.get(&dep_id) {
                            Some(Some(status)) if status.as_str() == "completed" => None,
                            Some(Some(status)) => Some(json!({"prd_id": dep_id, "status": status})),
                            // Discovered on disk, no backlog row yet.
                            Some(None) => Some(json!({"prd_id": dep_id, "status": "unenrolled"})),
                            // Declared but no matching file exists at all.
                            None => Some(json!({"prd_id": dep_id, "status": "not found"})),
                        }
                    })
                    .collect();
                json!({
                    "prd_path": prd.path.to_string(),
                    "prd_id": prd.id.to_string(),
                    "depends_on": depends_on,
                    "blocked_by": blocked_by,
                    "conflicts_with": conflicts.get(&prd.id.to_string()).cloned().unwrap_or_default(),
                    "hold": familiar_ai_core::backlog::front_matter_hold(prd),
                })
            })
            .collect();
        Ok(json!({"items": items, "conflicts_error": conflicts_error}))
    }

    /// What actually stopped each blocked PRD.
    ///
    /// A checkpoint records `phase = "blocked"` and nothing else useful — its
    /// `invalid_reason` is null for every blocked checkpoint in practice. The
    /// reason lives in the transition event's detail, as the scope evaluation
    /// that produced the stop. This reads that back and classifies it with the
    /// same rule the policy uses, so the window says the same thing the runner
    /// decided rather than a second opinion.
    fn blocked_reasons(&self, repo: &str) -> Result<Value, String> {
        use familiar_ai_review::ScopeDecision;

        let identity = Self::identity(repo)?;
        let db = self
            .db
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        let checkpoints = familiar_ai_storage::CheckpointRepository::new(db.conn());
        // Every checkpoint that has not reached the end, not just the ones
        // marked blocked: a stopped attempt can leave a candidate parked at
        // `implemented` or `reviewed`, and the operator still has to decide
        // what happens to it.
        let all = checkpoints
            .resumable(&identity.key)
            .map_err(|e| e.to_string())?;

        let mut items = Vec::new();
        for checkpoint in all.iter() {
            // FAM-BUG-089: a stop the owner released is not a reason the
            // card should still show. Same rule as the gates list and the
            // lifecycle.
            if stewardship::checkpoint_superseded_by_recovery(
                &db,
                &identity.key,
                &checkpoint.checkpoint_id,
                &checkpoint.prd_path,
            )
            .map_err(|e| e.to_string())?
            {
                continue;
            }
            let events = checkpoints
                .events(&checkpoint.checkpoint_id)
                .map_err(|e| e.to_string())?;
            // Events are ordered oldest first; the most recent stop is the one
            // that still holds.
            let detail = events
                .iter()
                .rev()
                .find(|(phase, _)| phase == "blocked" || phase == "invalid_checkpoint")
                .map(|(_, detail)| detail.clone());

            let mut blocking = Vec::new();
            let mut review_findings = 0usize;
            if let Some(detail) = &detail {
                if let Ok(parsed) = serde_json::from_str::<Value>(detail) {
                    review_findings = parsed
                        .get("findings")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    for finding in parsed
                        .get("scope_findings")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                    {
                        let Some(decision) = finding.get("decision") else {
                            continue;
                        };
                        let Ok(decision) =
                            serde_json::from_value::<ScopeDecision>(decision.clone())
                        else {
                            continue;
                        };
                        // Only the decisions the policy treats as stopping.
                        // `AllowedChange` and `JustifiedExpectedFileChange` are
                        // contained changes and would be noise here.
                        if !matches!(
                            decision,
                            ScopeDecision::ProhibitedChange
                                | ScopeDecision::UndeclaredScopeExpansion
                                | ScopeDecision::AmbiguousHumanReview
                        ) {
                            continue;
                        }
                        blocking.push(json!({
                            "path": finding.get("path").and_then(Value::as_str).unwrap_or(""),
                            "decision": finding.get("decision").cloned().unwrap_or(Value::Null),
                            "rule_id": finding.get("rule_id").and_then(Value::as_str).unwrap_or(""),
                            "rule_detail": finding
                                .get("rule_detail")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                        }));
                    }
                }
            }

            // What is genuinely outstanding, as opposed to what the generic
            // recovery template suggests: findings still awaiting a verdict,
            // and whether there is retained work to re-drive.
            let pending_decisions: i64 = db
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM scope_decisions \
                     WHERE repository_key=?1 AND prd_id=?2 AND decision IS NULL",
                    rusqlite::params![identity.key, checkpoint.prd_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            items.push(json!({
                "prd_path": checkpoint.prd_path,
                "prd_id": checkpoint.prd_id,
                "phase": checkpoint.phase,
                "invalid_reason": checkpoint.invalid_reason,
                "review_findings": review_findings,
                "blocking": blocking,
                "pending_decisions": pending_decisions,
                "branch": checkpoint.branch_name,
                "base_revision": checkpoint.base_revision,
            }));
        }
        Ok(json!({"items": items}))
    }

    /// Asks every configured provider what models it serves.
    ///
    /// Uses the OpenAI-compatible `/v1/models`, which Ollama, vLLM, Unsloth and
    /// the OpenAI API all answer — the same endpoint the Unsloth probe in
    /// `config_cli` already relies on. A provider that cannot be reached is
    /// reported rather than dropped: "no models" and "your endpoint is down"
    /// must not look the same in a dropdown.
    ///
    /// Deliberately blocking, with a short timeout, because the caller runs it
    /// on a worker thread.
    fn discover_models(&self) -> Value {
        let mut models: Vec<String> = Vec::new();
        let mut errors: Vec<Value> = Vec::new();

        // The full config validates every repository; a moved worktree must
        // not hide the model list (same rule as `read_inference_config`).
        match familiar_ai_core::Config::load(Some(&self.config_path())) {
            Ok(config) => {
                for (name, provider) in &config.providers {
                    if provider.host.trim().is_empty() {
                        continue;
                    }
                    match self.probe_models(&provider.host, &provider.auth) {
                        Ok(found) => models.extend(found),
                        Err(error) => errors.push(json!({"provider": name, "error": error})),
                    }
                }
            }
            Err(error) => errors.push(json!({
                "provider": "config",
                "error": format!("providers skipped, config could not be loaded: {error}"),
            })),
        }
        // FAM-BUG-091: the panel's own endpoint was never asked, so a host
        // with no `[providers]` table showed "0 discovered model(s)" beside
        // a reachable Ollama.
        if let Some((url, _)) = self.builtin_endpoint() {
            match self.probe_models(&url, &familiar_ai_core::config::AuthDescriptor::None) {
                Ok(found) => models.extend(found),
                Err(error) => errors.push(json!({"provider": "builtin", "error": error})),
            }
        }
        models.sort();
        models.dedup();
        json!({"models": models, "errors": errors})
    }

    /// The saved builtin endpoint and model, when the saved mode uses them.
    fn builtin_endpoint(&self) -> Option<(String, String)> {
        let inference = read_inference_config(&self.config_path()).ok()?;
        let text = &inference.text;
        if !matches!(mode_id(&text.mode), "local_only" | "hybrid")
            || text.builtin_url.trim().is_empty()
        {
            return None;
        }
        Some((text.builtin_url.clone(), text.builtin_model.clone()))
    }

    /// FAM-BUG-091: "connected" meant the endpoint answered, not that it
    /// serves the configured model. A saved `qwen2.5:3b` against an Ollama
    /// serving only 0.5b/1.5b/7b tested Healthy and would have failed on the
    /// first real request. `Ok(None)` means the model is served or the
    /// endpoint could not list models; `Ok(Some(served))` names the gap.
    fn model_not_served(&self, url: &str, model: &str) -> Option<Vec<String>> {
        let served = self
            .probe_models(url, &familiar_ai_core::config::AuthDescriptor::None)
            .ok()?;
        (!served.is_empty() && !served.iter().any(|m| m == model)).then_some(served)
    }

    fn probe_models(
        &self,
        host: &str,
        auth: &familiar_ai_core::config::AuthDescriptor,
    ) -> Result<Vec<String>, String> {
        // A host may already carry a scheme; assume plain HTTP only when it
        // does not, which matches how local runtimes are configured.
        let base = if host.starts_with("http://") || host.starts_with("https://") {
            host.to_string()
        } else {
            format!("http://{host}")
        };
        // The panel's endpoint is the OpenAI-compatible base and already
        // ends in `/v1`; provider hosts do not. Either way the list is at
        // exactly one `/v1/models`.
        let base = base.trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        // The async client through the context-aware helper: this runs on a
        // tokio worker when the desktop asks, and reqwest's blocking client
        // panics there while building its private runtime (FAM-BUG-091).
        let url = format!("{base}/v1/models");
        let bearer = crate::config_cli::check_auth(auth)
            .ok()
            .flatten()
            .map(|credential| credential.expose_for_request().to_string());
        let body: Value = self.block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(4))
                .build()
                .map_err(|e| e.to_string())?;
            let mut request = client.get(url);
            if let Some(token) = bearer {
                request = request.bearer_auth(token);
            }
            let response = request
                .send()
                .await
                .map_err(|e| format!("unreachable ({e})"))?;
            if !response.status().is_success() {
                return Err(format!("HTTP {}", response.status().as_u16()));
            }
            response
                .json::<Value>()
                .await
                .map_err(|e| format!("invalid /v1/models response ({e})"))
        })?;
        Ok(body
            .get("data")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.get("id").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn config_path(&self) -> std::path::PathBuf {
        self.paths.config_dir.join("config.toml")
    }

    /// Applies the operator's edits to the config file.
    ///
    /// Written through `toml_edit` so comments, ordering and formatting
    /// survive — this file is heavily commented with the reasoning behind
    /// several ceilings, and a serialize-from-struct round trip would erase
    /// all of it. Edits are applied to an in-memory document and only written
    /// once every one of them succeeded, so a rejected value cannot leave the
    /// file half-changed.
    /// Writes the inference settings and applies them to the running daemon.
    ///
    /// Two things make this its own path rather than three `ConfigEdit`s.
    /// The generic save can only write a setting the file already has, or one
    /// whose type it can borrow from a global of the same name — and a daemon
    /// being configured for the first time has no `[inference]` table to take
    /// either from. And these settings decide whether a backend exists at
    /// all, so applying them means rebuilding the router, not noting that a
    /// restart is due.
    fn save_inference_config(
        &self,
        mode: &str,
        builtin_url: &str,
        builtin_model: &str,
    ) -> Result<Value, String> {
        if !INFERENCE_MODES.contains(&mode) {
            return Err(format!("unknown inference mode: {mode}"));
        }
        let url = builtin_url.trim();
        let model = builtin_model.trim();
        // A mode that needs the builtin endpoint and does not have one would
        // write a config that cannot load, and the operator would be told
        // "enabled" about a backend that is not there.
        if matches!(mode, "local_only" | "hybrid") {
            if url.is_empty() {
                return Err("an endpoint is required for this mode".into());
            }
            if model.is_empty() {
                return Err("a model is required for this mode".into());
            }
        }

        // FAM-BUG-091: when the endpoint answers and lists models, a model
        // it does not list is a typo, not a preference. An unreachable
        // endpoint still saves — the server may simply be down right now.
        if matches!(mode, "local_only" | "hybrid") {
            if let Some(served) = self.model_not_served(url, model) {
                return Err(format!(
                    "{url} does not serve `{model}`. It serves: {}. Pick one of those or pull the model first.",
                    served.join(", ")
                ));
            }
        }

        let path = self.config_path();
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut document: toml_edit::Document = text.parse().map_err(|e| format!("{e}"))?;
        for (key, value) in [
            ("mode", mode),
            ("builtin_url", url),
            ("builtin_model", model),
        ] {
            let slot = ensure_item(
                document.as_item_mut(),
                &["inference".to_string(), "text".to_string(), key.to_string()],
            )
            .ok_or_else(|| format!("cannot create setting: inference.text.{key}"))?;
            *slot = toml_edit::value(value);
        }

        // Same recoverability rule the generic save follows: keep what was
        // there before overwriting it.
        let backup = path.with_extension(format!(
            "toml.bak-inference-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ));
        std::fs::copy(&path, &backup).map_err(|e| e.to_string())?;
        std::fs::write(&path, document.to_string()).map_err(|e| e.to_string())?;

        // Re-read rather than patching an in-memory copy: the file is the
        // source of truth, and it may have been edited by hand too.
        let inference = read_inference_config(&path)?;
        let router = self.router.clone();
        // FAM-BUG-087: this runs on a tokio worker (the local transport
        // dispatches operator mutations inline), where `Handle::block_on`
        // panics. The helper uses `block_in_place` there.
        let (configured, loaded, load_error) = self.block_on(async move {
            router.reconfigure(&inference).await;
            let configured = router.is_configured().await;
            match router.enable().await {
                Ok(()) => (configured, true, None),
                Err(e) => (configured, false, Some(e.to_string())),
            }
        });

        if let Ok(mut status) = self.status.lock() {
            status.local_llm_configured = configured;
            status.local_llm_enabled = loaded;
        }

        let note = match (configured, loaded, &load_error) {
            (false, _, _) => "Saved. Inference is disabled, so no backend is loaded.".to_string(),
            (true, true, _) => "Saved and loaded. Inference is active now.".to_string(),
            (true, false, Some(e)) => format!("Saved, but the backend did not load: {e}"),
            (true, false, None) => "Saved, but the backend did not load.".to_string(),
        };
        Ok(json!({
            "configured": configured,
            "loaded": loaded,
            "backup": backup.to_string_lossy(),
            "note": note,
        }))
    }

    fn save_config(&self, edits: &[ConfigEdit]) -> Result<Value, String> {
        let path = self.config_path();
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut document: toml_edit::Document = text.parse().map_err(|e| format!("{e}"))?;

        for edit in edits {
            let name = edit.path.join(".");
            if item_at(document.as_item_mut(), &edit.path).is_some() {
                let item = item_at(document.as_item_mut(), &edit.path).expect("just probed");
                let replacement =
                    typed_value(item, &edit.value).map_err(|e| format!("{name}: {e}"))?;
                *item = replacement;
                continue;
            }

            // The setting does not exist yet. That is the normal case for a
            // project overriding something it currently inherits, so take the
            // type from the global setting of the same name and create the
            // override. Anything else is a typo, not an override.
            let global_path = project_override_target(&edit.path)
                .ok_or_else(|| format!("no such setting: {name}"))?;
            let hint = item_at(document.as_item_mut(), global_path)
                .ok_or_else(|| format!("no such setting: {name}"))?;
            let replacement = typed_value(hint, &edit.value).map_err(|e| format!("{name}: {e}"))?;
            let slot = ensure_item(document.as_item_mut(), &edit.path)
                .ok_or_else(|| format!("cannot create setting: {name}"))?;
            *slot = replacement;
        }

        // FAM-BUG-094: the form used to write whatever it held and call it
        // validated. A reviewer adapter the enum rejects took the daemon
        // down on its next start. Load the candidate through the same
        // validation the daemon runs at startup, and refuse before writing.
        validate_candidate_config(&path, &document.to_string())?;

        // Keep the file that was there. The operator can be editing ceilings
        // that govern unattended spending; a bad save must be recoverable.
        let backup = path.with_extension(format!(
            "toml.bak-window-{}",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ));
        std::fs::copy(&path, &backup).map_err(|e| e.to_string())?;
        std::fs::write(&path, document.to_string()).map_err(|e| e.to_string())?;
        Ok(json!({
            "saved": edits.len(),
            "backup": backup.to_string_lossy(),
            "note": "Most settings are read at startup; restart the daemon for them to take effect.",
        }))
    }
}

/// Run the daemon's own startup validation over a candidate file body before
/// it replaces the real one. The candidate sits beside the real file so any
/// relative reference resolves the same way.
fn validate_candidate_config(path: &std::path::Path, candidate: &str) -> Result<(), String> {
    let probe = path.with_extension(format!("toml.candidate-{}", std::process::id()));
    std::fs::write(&probe, candidate).map_err(|e| e.to_string())?;
    let outcome = familiar_ai_core::Config::load(Some(&probe));
    let _ = std::fs::remove_file(&probe);
    outcome
        .map(|_| ())
        .map_err(|error| format!("not saved, the daemon would refuse this configuration: {error}"))
}

fn merge_configured_repositories(
    value: &mut Value,
    configured: impl IntoIterator<Item = familiar_ai_core::RepositoryIdentity>,
) -> Result<(), String> {
    let rows = value
        .get_mut("repositories")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "repository listing returned an invalid response".to_string())?;
    let mut known: std::collections::HashSet<String> = rows
        .iter()
        .filter_map(|row| row.get("path").and_then(Value::as_str).map(str::to_owned))
        .collect();
    for identity in configured {
        let path = identity.worktree.to_string_lossy().into_owned();
        if known.insert(path.clone()) {
            rows.push(json!({"repository_key": identity.key, "path": path}));
        }
    }
    rows.sort_by(|left, right| {
        left.get("path")
            .and_then(Value::as_str)
            .cmp(&right.get("path").and_then(Value::as_str))
    });
    Ok(())
}

impl DataSource for DaemonDataSource {
    fn query(&self, query: Query) -> Result<Value, String> {
        // Inference queries are async. `block_on` is safe here because this is
        // never called from inside the runtime: the tray calls it from the GTK
        // main thread, or from a worker thread it spawned for a slow query.
        match query {
            Query::InferenceStatus => Ok(json!(self.block_on(self.router.health()))),
            Query::TestConnection { target } => {
                let result = self.block_on(self.router.test_connection(&target));
                let mut value = json!(result);
                let mut message = if result.connected {
                    format!(
                        "Connected to {}{}.",
                        result.backend_name.as_deref().unwrap_or("the backend"),
                        result
                            .latency_ms
                            .map(|ms| format!(" in {ms} ms"))
                            .unwrap_or_default()
                    )
                } else {
                    format!(
                        "{}{}",
                        result.status_text,
                        result
                            .last_error
                            .as_deref()
                            .map(|e| format!(": {e}"))
                            .unwrap_or_default()
                    )
                };
                if result.connected && target == "text_primary" {
                    if let Some((url, model)) = self.builtin_endpoint() {
                        if let Some(served) = self.model_not_served(&url, &model) {
                            value["connected"] = json!(false);
                            value["status_text"] = json!("model not served");
                            message = format!(
                                "{url} answers, but it does not serve `{model}`. It serves: {}.",
                                served.join(", ")
                            );
                        }
                    }
                }
                value["message"] = json!(message);
                Ok(value)
            }
            Query::PrdText { repo, prd_path } => {
                // Contained to the repository: a `repo` and a `prd_path` from
                // the backlog are both trusted, but joining them blindly would
                // read anything a crafted path pointed at.
                let root = std::path::Path::new(&repo)
                    .canonicalize()
                    .map_err(|e| e.to_string())?;
                let full = root
                    .join(&prd_path)
                    .canonicalize()
                    .map_err(|e| e.to_string())?;
                if !full.starts_with(&root) {
                    return Err(format!("{prd_path} is outside {}", root.display()));
                }
                let text = std::fs::read_to_string(&full).map_err(|e| e.to_string())?;
                Ok(json!({"path": prd_path, "text": text}))
            }
            Query::Executions { repo, limit } => {
                let rows = self
                    .control
                    .executions(&Self::operator_scope(), Self::project_id(&repo), limit)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"items": rows}))
            }
            Query::Rounds { repo, limit } => {
                let db = self
                    .db
                    .lock()
                    .map_err(|_| "database lock poisoned".to_string())?;
                stewardship::list_rounds(&db, &Self::identity(&repo)?, limit)
                    .map_err(|e| e.to_string())
            }
            Query::Checkpoints { repo } => {
                let db = self
                    .db
                    .lock()
                    .map_err(|_| "database lock poisoned".to_string())?;
                stewardship::list_checkpoints(&db, &Self::identity(&repo)?, None, 200)
                    .map_err(|e| e.to_string())
            }
            Query::Dependencies { repo } => self.dependencies(&repo),
            Query::BlockedReasons { repo } => self.blocked_reasons(&repo),
            Query::Backlog { repo, limit } => {
                // PRD-108: same bounded reconcile-on-read fallback as
                // `dependencies`, so the two share one reconciled view
                // rather than reading different epochs. Must run before the
                // database lock below is taken — `reconcile_if_stale` takes
                // it itself, and the lock is not reentrant.
                self.reconciler
                    .reconcile_if_stale(std::path::Path::new(&repo));
                let db = self
                    .db
                    .lock()
                    .map_err(|_| "database lock poisoned".to_string())?;
                let layout = self.layout(&repo).ok();
                stewardship::list_backlog(
                    &db,
                    &Self::identity(&repo)?,
                    layout.as_ref(),
                    None,
                    None,
                    limit,
                )
                .map_err(|e| e.to_string())
            }
            Query::ConfigChoices => Ok(self.config_choices()),
            Query::DiscoverModels => Ok(self.discover_models()),
            Query::InferenceSettings => {
                let inference = read_inference_config(&self.config_path())?;
                let text = &inference.text;
                Ok(json!({
                    "mode": mode_id(&text.mode),
                    "builtin_url": text.builtin_url,
                    "builtin_model": text.builtin_model,
                }))
            }
            Query::ConfigDocument => {
                let path = self.config_path();
                let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                let parsed: toml::Value = toml::from_str(&text).map_err(|e| e.to_string())?;
                Ok(json!({
                    "path": path.to_string_lossy(),
                    "document": serde_json::to_value(parsed).map_err(|e| e.to_string())?,
                }))
            }
            Query::ProjectState { repo } => {
                let state = self
                    .control
                    .project_state(&Self::operator_scope(), Self::project_id(&repo))
                    .map_err(|e| e.to_string())?;
                Ok(json!({"state": state}))
            }
            Query::Repositories => self.repositories(),
            other => {
                let db = self
                    .db
                    .lock()
                    .map_err(|_| "database lock poisoned".to_string())?;
                let value = match other {
                    Query::Gates { repo } => {
                        stewardship::list_pending_human_gates(&db, &Self::identity(&repo)?, 100)
                    }
                    Query::Sessions { repo, limit } => {
                        stewardship::list_sessions(&db, &Self::identity(&repo)?, None, limit)
                    }
                    Query::Attempts { repo, session_id } => stewardship::list_attempts(
                        &db,
                        &Self::identity(&repo)?,
                        &session_id,
                        None,
                        50,
                    ),
                    Query::Review { repo, session_id } => {
                        stewardship::list_review_findings(&db, &Self::identity(&repo)?, &session_id)
                    }
                    Query::Budget { repo, session_id } => {
                        stewardship::get_budget(&db, &Self::identity(&repo)?, &session_id)
                    }
                    // Handled above; these never reach here.
                    Query::InferenceStatus
                    | Query::TestConnection { .. }
                    | Query::PrdText { .. }
                    | Query::Executions { .. }
                    | Query::ProjectState { .. }
                    | Query::ConfigDocument
                    | Query::Rounds { .. }
                    | Query::InferenceSettings
                    | Query::ConfigChoices
                    | Query::Checkpoints { .. }
                    | Query::Dependencies { .. }
                    | Query::BlockedReasons { .. }
                    | Query::Backlog { .. }
                    | Query::DiscoverModels => unreachable!(),
                    Query::Repositories => unreachable!(),
                };
                value.map_err(|e| e.to_string())
            }
        }
    }

    fn act(&self, action: Action) -> Result<Value, String> {
        let scope = Self::operator_scope();
        match action {
            Action::StartPrd { repo, prd_path } => {
                let project = Self::project_id(&repo).to_string();
                // Registration is idempotent and leaves an existing project's
                // state alone, so this cannot silently unpause a paused
                // project on the way to submitting.
                self.control
                    .register_project(&project, &repo, 0, None)
                    .map_err(|e| e.to_string())?;
                let stamp = chrono::Utc::now().timestamp_micros();
                let submission = Submission {
                    execution_id: format!("exec-{stamp}-{}", std::process::id()),
                    project_id: project.clone(),
                    // Unique per click: the operator asking twice means they
                    // want it twice, and the control plane dedupes identical
                    // keys.
                    idempotency_key: format!("tray:{project}:{prd_path}:{stamp}"),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: json!({
                        "argv": ["familiar-ai", "run", prd_path],
                        "timeout_ms": null,
                    })
                    .to_string(),
                };
                let ack = self
                    .control
                    .submit(&scope, &submission)
                    .map_err(|e| e.to_string())?;
                Ok(json!({
                    "execution_id": ack.execution_id,
                    "duplicate": ack.duplicate,
                }))
            }
            Action::ResumePrd { repo, prd_id } => {
                let project = Self::project_id(&repo).to_string();
                self.control
                    .register_project(&project, &repo, 0, None)
                    .map_err(|e| e.to_string())?;
                let stamp = chrono::Utc::now().timestamp_micros();
                let submission = Submission {
                    execution_id: format!("exec-{stamp}-{}", std::process::id()),
                    project_id: project.clone(),
                    idempotency_key: format!("tray-resume:{project}:{prd_id}:{stamp}"),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: json!({
                        "argv": ["familiar-ai", "resume", prd_id],
                        "timeout_ms": null,
                    })
                    .to_string(),
                };
                let ack = self
                    .control
                    .submit(&scope, &submission)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"execution_id": ack.execution_id, "duplicate": ack.duplicate}))
            }
            Action::CancelExecution { repo, execution_id } => {
                let stopped = self
                    .control
                    .cancel(&scope, &execution_id, Self::project_id(&repo))
                    .map_err(|e| e.to_string())?;
                Ok(json!({"stopped": stopped}))
            }
            Action::SetProjectPaused { repo, paused } => {
                let state = if paused { "paused" } else { "active" };
                let changed = self
                    .control
                    .set_project_state(&scope, Self::project_id(&repo), state)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"state": state, "changed": changed}))
            }
            Action::ReleasePrd {
                repo,
                prd_path,
                actor,
                reason,
            } => self.recover(
                &repo,
                &prd_path,
                BacklogRecoveryAction::Release,
                &actor,
                &reason,
            ),
            Action::CompletePrd {
                repo,
                prd_path,
                actor,
                reason,
            } => self.recover(
                &repo,
                &prd_path,
                BacklogRecoveryAction::ManualCompleteOverride,
                &actor,
                &reason,
            ),
            Action::SaveConfig { edits } => self.save_config(&edits),
            Action::SaveInferenceConfig {
                mode,
                builtin_url,
                builtin_model,
            } => self.save_inference_config(&mode, &builtin_url, &builtin_model),
            Action::StopDaemon => {
                self.shutdown
                    .as_ref()
                    .ok_or_else(|| "daemon shutdown is not available from this client".to_string())?
                    .send(true)
                    .map_err(|_| "daemon is already stopping".to_string())?;
                Ok(json!({"stopping": true}))
            }
        }
    }
}

/// The effective inference settings: the file's `[inference]` table where it
/// has one, the schema's defaults where it does not.
///
/// Deliberately narrower than `Config::load`, which validates the whole file —
/// including canonicalizing every configured repository's worktree. Setting up
/// inference must not fail because some unrelated repository has since been
/// moved or deleted.
fn read_inference_config(
    path: &std::path::Path,
) -> Result<familiar_ai_core::config::InferenceConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let document: toml::Value = toml::from_str(&text).map_err(|e| e.to_string())?;
    match document.get("inference") {
        Some(table) => table.clone().try_into().map_err(|e| format!("{e}")),
        // Every field carries a serde default, so an absent table is the
        // default configuration rather than an error.
        None => Ok(familiar_ai_core::config::InferenceConfig::default()),
    }
}

/// The inference modes the config file accepts, spelled as serde writes them.
const INFERENCE_MODES: [&str; 4] = ["disabled", "local_only", "remote_only", "hybrid"];

/// The mode's config-file spelling, which is what the form's dropdown uses.
fn mode_id(mode: &familiar_ai_core::config::InferenceMode) -> &'static str {
    use familiar_ai_core::config::InferenceMode;
    match mode {
        InferenceMode::Disabled => "disabled",
        InferenceMode::LocalOnly => "local_only",
        InferenceMode::RemoteOnly => "remote_only",
        InferenceMode::Hybrid => "hybrid",
    }
}

/// Walks a path through the document to the item it names.
///
/// Numeric segments index an array of tables — the shape
/// `[[repositories.x.verification.checks]]` produces — so a setting nested
/// inside one is addressable like any other.
fn item_at<'a>(item: &'a mut toml_edit::Item, path: &[String]) -> Option<&'a mut toml_edit::Item> {
    let Some((segment, rest)) = path.split_first() else {
        return Some(item);
    };
    if item.as_array_of_tables_mut().is_some() {
        let index: usize = segment.parse().ok()?;
        let array = item.as_array_of_tables_mut()?;
        let table = array.get_mut(index)?;
        let (key, tail) = rest.split_first()?;
        let child = table.get_mut(key)?;
        return item_at(child, tail);
    }
    let table = item.as_table_like_mut()?;
    let child = table.get_mut(segment)?;
    item_at(child, rest)
}

/// Parses the operator's text against the type already at that path.
///
/// The type in the file is the contract — a ceiling that is an integer must
/// stay an integer — so a value that does not parse is refused by name rather
/// than silently rewriting the setting as a string.
fn typed_value(existing: &toml_edit::Item, text: &str) -> Result<toml_edit::Item, String> {
    use toml_edit::value;
    let text = text.trim();
    let Some(current) = existing.as_value() else {
        return Err("not an editable value".into());
    };
    Ok(match current {
        toml_edit::Value::Boolean(_) => value(
            text.parse::<bool>()
                .map_err(|_| format!("expected true or false, got {text:?}"))?,
        ),
        toml_edit::Value::Integer(_) => value(
            text.parse::<i64>()
                .map_err(|_| format!("expected a whole number, got {text:?}"))?,
        ),
        toml_edit::Value::Float(_) => value(
            text.parse::<f64>()
                .map_err(|_| format!("expected a number, got {text:?}"))?,
        ),
        toml_edit::Value::String(_) => value(text),
        toml_edit::Value::Array(existing_array) => {
            // Element type follows what is already in the list, so a list of
            // numbers cannot become a list of strings by being retyped.
            let strings = existing_array
                .iter()
                .all(|e| matches!(e, toml_edit::Value::String(_)));
            let mut array = toml_edit::Array::new();
            for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                if strings {
                    array.push(part);
                } else if let Ok(n) = part.parse::<i64>() {
                    array.push(n);
                } else if let Ok(f) = part.parse::<f64>() {
                    array.push(f);
                } else {
                    return Err(format!("expected numbers, got {part:?}"));
                }
            }
            value(array)
        }
        other => return Err(format!("cannot edit a {} here", other.type_name())),
    })
}

/// For a path under `repositories.<repo>`, the global setting it shadows.
///
/// A project override has no type of its own until it exists, and the type it
/// must take is the one the global setting already has.
fn project_override_target(path: &[String]) -> Option<&[String]> {
    match path {
        [first, _repo, rest @ ..] if first == "repositories" && !rest.is_empty() => Some(rest),
        _ => None,
    }
}

/// Like [`item_at`], but creates the intermediate tables it walks through.
///
/// Intermediates are marked implicit so a new override renders as
/// `[repositories."/path".review]` rather than also emitting an empty
/// `[repositories."/path"]` header above it.
fn ensure_item<'a>(
    item: &'a mut toml_edit::Item,
    path: &[String],
) -> Option<&'a mut toml_edit::Item> {
    let Some((segment, rest)) = path.split_first() else {
        return Some(item);
    };
    if item.as_array_of_tables_mut().is_some() {
        // Never fabricate array elements: an index that is not there is a bug
        // in the caller, not a setting to create.
        return item_at(item, path);
    }
    let table = item.as_table_like_mut()?;
    if table.get(segment).is_none() {
        let mut created = toml_edit::Table::new();
        if !rest.is_empty() {
            created.set_implicit(true);
        }
        table.insert(segment, toml_edit::Item::Table(created));
    }
    let child = table.get_mut(segment)?;
    ensure_item(child, rest)
}

/// The executable an adapter drives when the config does not say otherwise.
/// The closed set `agents.<role>.adapter` accepts, mirroring
/// `AgentAdapterKind` and its `default_executable`: id, executable, and what
/// to say about a kind that is not a vendor CLI.
const ADAPTER_KINDS: [(&str, &str, &str); 4] = [
    ("claude-code", "claude", ""),
    ("codex", "codex", ""),
    ("ollama", "codex", "runs through Familiar's own agent loop"),
    (
        "raw-agent-loop",
        "raw-agent-loop",
        "Familiar's own agent loop over an API runtime; no executable",
    ),
];

/// Whether an adapter kind needs its executable on PATH to be usable.
fn which_matters(adapter: &str) -> bool {
    matches!(adapter, "claude-code" | "codex")
}

/// Whether a program is runnable, resolved the way a shell would.
fn which_on_path(program: &str) -> Option<String> {
    if program.is_empty() {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::merge_configured_repositories;
    use familiar_ai_core::RepositoryIdentity;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn configured_repository_remains_visible_without_durable_rows() {
        let mut value = json!({"repositories": []});
        merge_configured_repositories(
            &mut value,
            [RepositoryIdentity {
                worktree: PathBuf::from("/work/familiar"),
                key: "/work/familiar/.git".into(),
            }],
        )
        .unwrap();

        assert_eq!(
            value,
            json!({"repositories": [{
                "repository_key": "/work/familiar/.git",
                "path": "/work/familiar"
            }]})
        );
    }

    #[test]
    fn configured_repository_does_not_duplicate_a_durable_row() {
        let mut value = json!({"repositories": [{
            "repository_key": "/work/familiar/.git",
            "path": "/work/familiar"
        }]});
        merge_configured_repositories(
            &mut value,
            [RepositoryIdentity {
                worktree: PathBuf::from("/work/familiar"),
                key: "/work/familiar/.git".into(),
            }],
        )
        .unwrap();

        assert_eq!(value["repositories"].as_array().unwrap().len(), 1);
    }
}
