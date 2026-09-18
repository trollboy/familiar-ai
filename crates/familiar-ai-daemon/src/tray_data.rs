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
use familiar_ai_core::{
    validate_recovery_attribution, BacklogDiscovery, BacklogRecoveryAction,
    FilesystemBacklogDiscovery,
};
use familiar_ai_llm::InferenceRouter;
use familiar_ai_storage::{Database, SqliteBacklogRepository};
use familiar_ai_tray::data::{Action, ConfigEdit, DataSource, Query};

use crate::control_plane::ControlPlaneService;
use crate::stewardship;

pub struct DaemonDataSource {
    db: Arc<Mutex<Database>>,
    router: Arc<InferenceRouter>,
    runtime: Arc<tokio::runtime::Runtime>,
    control: ControlPlaneService,
    paths: familiar_ai_core::AppPaths,
}

impl DaemonDataSource {
    pub fn new(
        db: Arc<Mutex<Database>>,
        router: Arc<InferenceRouter>,
        runtime: Arc<tokio::runtime::Runtime>,
        control: ControlPlaneService,
        paths: familiar_ai_core::AppPaths,
    ) -> Self {
        Self {
            db,
            router,
            runtime,
            control,
            paths,
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

    /// Every PRD the repository declares, needed to resolve a supplied path to
    /// a real backlog entry before mutating it.
    fn discovered(&self, repo: &str) -> Result<Vec<familiar_ai_core::DiscoveredPrd>, String> {
        let repository = Self::identity(repo)?;
        let config = crate::cli::shared::effective_repository_config(
            &self.paths,
            std::path::Path::new(repo),
        )?;
        let layout = config
            .repository(std::path::Path::new(repo))
            .map_err(|e| e.to_string())?
            .layout();
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

        let factories = familiar_ai_agent::builtin_adapter_factories();
        let adapters: Vec<Value> = factories
            .ids()
            .into_iter()
            .map(|id| {
                let executable = default_executable(id.as_str());
                let found = which_on_path(executable);
                json!({
                    "value": id,
                    "available": found.is_some(),
                    "detail": match &found {
                        Some(path) => format!("{executable} at {path}"),
                        None => format!("{executable} not on PATH"),
                    },
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
            "permission_modes": ["default", "plan", "acceptEdits", "bypassPermissions"],
            "inference_modes": ["disabled", "local_only", "remote_only", "hybrid"],
            "prd_metadata_policies": ["incremental", "strict"],
            "providers": providers,
            "models": models,
        })
    }

    /// Which PRDs are waiting on other PRDs.
    ///
    /// Applies the same rule the runner does — a dependency counts as met only
    /// when its backlog status is `completed` — so the form cannot offer to
    /// start something the backlog would then refuse. A dependency that is not
    /// discovered at all is unmet too, and named, because that is a real state
    /// this backlog reaches when a PRD is moved or deleted.
    fn dependencies(&self, repo: &str) -> Result<Value, String> {
        let discovered = self.discovered(repo)?;
        let identity = Self::identity(repo)?;
        let statuses: std::collections::HashMap<String, String> = {
            let db = self
                .db
                .lock()
                .map_err(|_| "database lock poisoned".to_string())?;
            let backlog = stewardship::list_backlog(&db, &identity, None, None, 2000)
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
                                item.get("status")?.as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        let status_of_id: std::collections::HashMap<String, Option<&String>> = discovered
            .iter()
            .map(|prd| (prd.id.to_string(), statuses.get(&prd.path.to_string())))
            .collect();

        let items: Vec<Value> = discovered
            .iter()
            .map(|prd| {
                let blocked_by: Vec<Value> = prd
                    .dependencies
                    .iter()
                    .filter_map(|dep| {
                        let dep_id = dep.to_string();
                        match status_of_id.get(&dep_id) {
                            Some(Some(status)) if status.as_str() == "completed" => None,
                            Some(Some(status)) => Some(json!({"prd_id": dep_id, "status": status})),
                            // Declared but not discovered, or discovered with
                            // no backlog row yet.
                            _ => Some(json!({"prd_id": dep_id, "status": "not found"})),
                        }
                    })
                    .collect();
                json!({
                    "prd_path": prd.path.to_string(),
                    "prd_id": prd.id.to_string(),
                    "blocked_by": blocked_by,
                })
            })
            .collect();
        Ok(json!({"items": items}))
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
        let Some(config) = familiar_ai_core::Config::load(Some(&self.config_path())).ok() else {
            return json!({"models": [], "errors": ["config could not be read"]});
        };
        let mut models: Vec<String> = Vec::new();
        let mut errors: Vec<Value> = Vec::new();

        for (name, provider) in &config.providers {
            if provider.host.trim().is_empty() {
                continue;
            }
            match self.probe_models(&provider.host, &provider.auth) {
                Ok(found) => models.extend(found),
                Err(error) => errors.push(json!({"provider": name, "error": error})),
            }
        }
        models.sort();
        models.dedup();
        json!({"models": models, "errors": errors})
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
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(4))
            .build()
            .map_err(|e| e.to_string())?;
        let mut request = client.get(format!("{base}/v1/models"));
        if let Ok(Some(credential)) = crate::config_cli::check_auth(auth) {
            request = request.bearer_auth(credential.expose_for_request());
        }
        let response = request.send().map_err(|e| format!("unreachable ({e})"))?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status().as_u16()));
        }
        let body: Value = response
            .json()
            .map_err(|e| format!("invalid /v1/models response ({e})"))?;
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

impl DataSource for DaemonDataSource {
    fn query(&self, query: Query) -> Result<Value, String> {
        // Inference queries are async. `block_on` is safe here because this is
        // never called from inside the runtime: the tray calls it from the GTK
        // main thread, or from a worker thread it spawned for a slow query.
        match query {
            Query::InferenceStatus => Ok(json!(self.runtime.block_on(self.router.health()))),
            Query::TestConnection { target } => Ok(json!(self
                .runtime
                .block_on(self.router.test_connection(&target)))),
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
            Query::ConfigChoices => Ok(self.config_choices()),
            Query::DiscoverModels => Ok(self.discover_models()),
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
            other => {
                let db = self
                    .db
                    .lock()
                    .map_err(|_| "database lock poisoned".to_string())?;
                let value = match other {
                    Query::Repositories => stewardship::list_repositories(&db),
                    Query::Gates { repo } => {
                        stewardship::list_pending_human_gates(&db, &Self::identity(&repo)?, 100)
                    }
                    Query::Backlog { repo, limit } => {
                        stewardship::list_backlog(&db, &Self::identity(&repo)?, None, None, limit)
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
                    | Query::ConfigChoices
                    | Query::Checkpoints { .. }
                    | Query::Dependencies { .. }
                    | Query::BlockedReasons { .. }
                    | Query::DiscoverModels => unreachable!(),
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
        }
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
fn default_executable(adapter: &str) -> &'static str {
    match adapter {
        "claude-code" => "claude",
        "codex" => "codex",
        "ollama" => "ollama",
        _ => "",
    }
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
