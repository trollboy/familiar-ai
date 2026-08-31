use std::sync::{Arc, Mutex};

use familiar_ai_core::control_plane::{
    AgentCapabilityView, Authority, CapabilityGrant, CapabilityScope, ControlEvent,
    ExecutionRecord, ExecutionState, SchedulingPolicy, Submission, SubmissionAck,
};
use familiar_ai_core::{FamiliarError, Result};
use familiar_ai_storage::{ControlPlaneRepository, Database};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};

/// Transport-free application boundary. CLI, socket, dashboard and MCP are
/// adapters; authorization and mutation are centralized here.
#[derive(Clone)]
pub struct ControlPlaneService {
    db: Arc<Mutex<Database>>,
    policy: SchedulingPolicy,
    owner_generation: u64,
}

impl ControlPlaneService {
    pub fn new(db: Arc<Mutex<Database>>, policy: SchedulingPolicy, owner_generation: u64) -> Self {
        Self {
            db,
            policy,
            owner_generation,
        }
    }

    pub fn register_project(
        &self,
        project_id: &str,
        root: &str,
        priority: i64,
        ceiling: Option<usize>,
    ) -> Result<()> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).register_project(
            project_id,
            root,
            priority,
            ceiling.unwrap_or(self.policy.default_project_ceiling),
        )
    }

    pub fn set_project_state(
        &self,
        scope: &CapabilityScope,
        project: &str,
        state: &str,
    ) -> Result<bool> {
        require(scope, Authority::Control, project, None)?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).set_project_state(project, state)
    }

    pub fn submit(&self, scope: &CapabilityScope, request: &Submission) -> Result<SubmissionAck> {
        require(scope, Authority::Control, &request.project_id, None)?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).submit(request)
    }

    pub fn observe(
        &self,
        scope: &CapabilityScope,
        execution_id: &str,
        project_id: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<ControlEvent>> {
        require(scope, Authority::Observe, project_id, Some(execution_id))?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).events_after(
            execution_id,
            after,
            limit.min(1000),
        )
    }

    pub fn claim_next(&self) -> Result<Option<String>> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut())
            .claim_next(self.policy.global_ceiling, self.owner_generation)
    }

    pub fn recover(&self, verified_live_workers: &[String]) -> Result<usize> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).recover_running(verified_live_workers)
    }

    pub fn reconcile_filesystem(&self) -> Result<usize> {
        let locations = {
            let mut db = self.db.lock().map_err(|_| {
                FamiliarError::Database("control-plane database lock poisoned".into())
            })?;
            ControlPlaneRepository::new(db.conn_mut()).project_locations()?
        };
        let mut changed = 0;
        for (project, root) in locations {
            if !std::path::Path::new(&root).exists() {
                let mut db = self.db.lock().map_err(|_| {
                    FamiliarError::Database("control-plane database lock poisoned".into())
                })?;
                changed += usize::from(
                    ControlPlaneRepository::new(db.conn_mut()).record_divergence(
                        &project,
                        None,
                        "missing_project_root",
                        &format!("control-plane root is unavailable: {root}"),
                    )?,
                );
            }
        }
        Ok(changed)
    }

    pub fn execution(
        &self,
        scope: &CapabilityScope,
        id: &str,
        project: &str,
    ) -> Result<Option<ExecutionRecord>> {
        require(scope, Authority::Observe, project, Some(id))?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        let result = ControlPlaneRepository::new(db.conn_mut()).execution(id)?;
        if result.as_ref().is_some_and(|r| r.project_id != project) {
            return Err(FamiliarError::Config(
                "authority denied: unrelated project".into(),
            ));
        }
        Ok(result)
    }

    pub fn execution_internal(&self, id: &str) -> Result<Option<ExecutionRecord>> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).execution(id)
    }

    pub fn finish(&self, id: &str, state: ExecutionState, reason: &str) -> Result<bool> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).finish(id, state, reason)
    }

    pub fn project_root(&self, project: &str) -> Result<Option<String>> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).project_root(project)
    }

    pub fn bind_worker(
        &self,
        execution: &str,
        identity: &str,
        pid: u32,
        start: &str,
        token_hash: &str,
    ) -> Result<()> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).bind_worker(
            execution,
            identity,
            pid,
            start,
            token_hash,
            self.owner_generation,
        )
    }

    pub fn mint_worker_session(&self, execution: &str, worker: &str) -> Result<CapabilityGrant> {
        let record = self.execution_internal(execution)?.ok_or_else(|| {
            FamiliarError::Config("cannot mint a session for an unknown execution".into())
        })?;
        let internal = CapabilityScope {
            client_class: familiar_ai_core::control_plane::ClientClass::Internal,
            project_id: None,
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control],
        };
        self.mint_session(
            &internal,
            CapabilityScope {
                client_class: familiar_ai_core::control_plane::ClientClass::Mcp,
                project_id: Some(record.project_id),
                execution_id: Some(record.execution_id),
                attempt: Some(record.attempt),
                worker_id: Some(worker.into()),
                authorities: vec![
                    Authority::Observe,
                    Authority::ReadWarrant,
                    Authority::ReadAccountingLabels,
                    Authority::ReportProgress,
                    Authority::SubmitEvidence,
                    Authority::RequestEscalation,
                ],
            },
            24 * 60 * 60,
        )
    }

    pub fn cancel(&self, scope: &CapabilityScope, execution: &str, project: &str) -> Result<bool> {
        require(scope, Authority::Control, project, Some(execution))?;
        let pid = {
            let mut db = self.db.lock().map_err(|_| {
                FamiliarError::Database("control-plane database lock poisoned".into())
            })?;
            ControlPlaneRepository::new(db.conn_mut()).worker_pid(execution)?
        };
        #[cfg(unix)]
        if let Some(pid) = pid {
            if pid <= i32::MAX as u32 {
                unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
            }
        }
        self.finish(execution, ExecutionState::Cancelled, "operator_cancelled")
    }
    pub fn end_foreground(&self, execution: &str) -> Result<bool> {
        let record = self.execution_internal(execution)?;
        if !record.is_some_and(|r| {
            r.mode == familiar_ai_core::control_plane::ExecutionMode::ForegroundOnly
        }) {
            return Ok(false);
        }
        let pid = {
            let mut db = self.db.lock().map_err(|_| {
                FamiliarError::Database("control-plane database lock poisoned".into())
            })?;
            ControlPlaneRepository::new(db.conn_mut()).worker_pid(execution)?
        };
        #[cfg(unix)]
        if let Some(pid) = pid {
            if pid <= i32::MAX as u32 {
                unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
            }
        }
        self.finish(
            execution,
            ExecutionState::ForegroundEnded,
            "foreground_session_ended",
        )
    }

    pub fn live_worker_candidates(&self) -> Result<Vec<(String, u32, String)>> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).worker_candidates()
    }

    pub fn record_late_worker_outcome(
        &self,
        execution: &str,
        worker: &str,
        payload: &str,
    ) -> Result<bool> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).record_late_outcome(execution, worker, payload)
    }

    /// Minting is host-only. The raw credential is returned exactly once and
    /// only its SHA-256 digest is persisted.
    pub fn mint_session(
        &self,
        issuer: &CapabilityScope,
        scope: CapabilityScope,
        ttl_seconds: i64,
    ) -> Result<CapabilityGrant> {
        if issuer.client_class != familiar_ai_core::control_plane::ClientClass::Internal
            && !issuer.authorities.contains(&Authority::Control)
        {
            return Err(FamiliarError::Config(
                "authority denied: session minting requires host control authority".into(),
            ));
        }
        if issuer
            .project_id
            .as_ref()
            .zip(scope.project_id.as_ref())
            .is_some_and(|(a, b)| a != b)
        {
            return Err(FamiliarError::Config(
                "authority denied: cannot mint an unrelated-project session".into(),
            ));
        }
        let mut raw = [0_u8; 32];
        SystemRandom::new().fill(&mut raw).map_err(|_| {
            FamiliarError::Config("secure session credential generation failed".into())
        })?;
        let credential = raw.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let hash = credential_hash(&credential);
        let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds.max(1)))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).store_session(&hash, &scope, &expires_at)?;
        Ok(CapabilityGrant {
            credential,
            scope,
            expires_at,
        })
    }

    pub fn authenticate(&self, credential: &str) -> Result<CapabilityScope> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut())
            .session(&credential_hash(credential))?
            .ok_or_else(|| {
                FamiliarError::Config("authority denied: a valid minted session is required".into())
            })
    }

    pub fn revoke_session(&self, credential: &str) -> Result<bool> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).revoke_session(&credential_hash(credential))
    }

    pub fn request_escalation(&self, credential: &str, request_json: &str) -> Result<String> {
        let scope = self.authenticate(credential)?;
        require(
            &scope,
            Authority::RequestEscalation,
            scope.project_id.as_deref().unwrap_or(""),
            scope.execution_id.as_deref(),
        )?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).pending_gate(
            &credential_hash(credential),
            &scope,
            request_json,
        )
    }

    pub fn agent_view(&self, credential: &str) -> Result<AgentCapabilityView> {
        let scope = self.authenticate(credential)?;
        for authority in [Authority::Observe, Authority::ReadWarrant] {
            if !scope.authorities.contains(&authority) {
                return Err(FamiliarError::Config(format!(
                    "authority denied: {authority:?}"
                )));
            }
        }
        let project = scope
            .project_id
            .clone()
            .ok_or_else(|| FamiliarError::Config("agent session is not project scoped".into()))?;
        let execution = scope
            .execution_id
            .clone()
            .ok_or_else(|| FamiliarError::Config("agent session is not execution scoped".into()))?;
        let attempt = scope
            .attempt
            .ok_or_else(|| FamiliarError::Config("agent session is not attempt scoped".into()))?;
        let worker = scope
            .worker_id
            .clone()
            .ok_or_else(|| FamiliarError::Config("agent session is not worker scoped".into()))?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        let repo = ControlPlaneRepository::new(db.conn_mut());
        let record = repo
            .execution(&execution)?
            .ok_or_else(|| FamiliarError::Config("scoped execution no longer exists".into()))?;
        if record.project_id != project
            || record.attempt != attempt
            || record.worker_identity.as_deref() != Some(&worker)
        {
            return Err(FamiliarError::Config(
                "agent session assignment no longer matches its execution attempt and worker"
                    .into(),
            ));
        }
        Ok(AgentCapabilityView {
            project_id: project,
            execution_id: execution.clone(),
            attempt,
            worker_id: worker,
            state: record.state,
            warrant_json: repo.warrant_view(&execution)?,
            remaining_reservations_json: repo.reservation_view(&execution)?,
        })
    }

    pub fn report_agent_event(
        &self,
        credential: &str,
        authority: Authority,
        kind: &str,
        payload: &str,
    ) -> Result<i64> {
        let scope = self.authenticate(credential)?;
        if !scope.authorities.contains(&authority) {
            return Err(FamiliarError::Config(format!(
                "authority denied: {authority:?}"
            )));
        }
        let execution = scope
            .execution_id
            .as_deref()
            .ok_or_else(|| FamiliarError::Config("agent session is not execution scoped".into()))?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| FamiliarError::Database("control-plane database lock poisoned".into()))?;
        ControlPlaneRepository::new(db.conn_mut()).append_agent_event(execution, kind, payload)
    }
}

fn credential_hash(raw: &str) -> String {
    digest::digest(&digest::SHA256, raw.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn require(
    scope: &CapabilityScope,
    authority: Authority,
    project: &str,
    execution: Option<&str>,
) -> Result<()> {
    if !scope.authorities.contains(&authority) {
        return Err(FamiliarError::Config(format!(
            "authority denied: {authority:?}"
        )));
    }
    if scope.project_id.as_deref().is_some_and(|p| p != project) {
        return Err(FamiliarError::Config(
            "authority denied: unrelated project".into(),
        ));
    }
    if let Some(execution) = execution {
        if scope
            .execution_id
            .as_deref()
            .is_some_and(|e| e != execution)
        {
            return Err(FamiliarError::Config(
                "authority denied: unrelated execution".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::control_plane::{ClientClass, ExecutionMode};
    fn scope(project: &str, authorities: Vec<Authority>) -> CapabilityScope {
        CapabilityScope {
            client_class: ClientClass::Operator,
            project_id: Some(project.into()),
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities,
        }
    }
    #[test]
    fn durable_idempotent_submission_and_cursor_replay() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(Mutex::new(db));
        let svc = ControlPlaneService::new(db, SchedulingPolicy::default(), 1);
        svc.register_project("p", "/p", 0, None).unwrap();
        let req = Submission {
            execution_id: "e1".into(),
            project_id: "p".into(),
            idempotency_key: "k".into(),
            mode: ExecutionMode::Detached,
            priority: 0,
            command_json: String::new(),
        };
        let s = scope("p", vec![Authority::Control, Authority::Observe]);
        let a = svc.submit(&s, &req).unwrap();
        let b = svc
            .submit(
                &s,
                &Submission {
                    execution_id: "e2".into(),
                    ..req
                },
            )
            .unwrap();
        assert!(!a.duplicate);
        assert!(b.duplicate);
        assert_eq!(a.execution_id, b.execution_id);
        let events = svc.observe(&s, "e1", "p", 0, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert!(svc.observe(&s, "e1", "other", 0, 10).is_err());
    }

    #[test]
    fn failed_submission_transaction_leaves_no_queue_or_acknowledgement() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(Mutex::new(db));
        let svc = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 1);
        svc.register_project("p", "/p", 0, None).unwrap();
        db.lock().unwrap().conn().execute_batch("CREATE TRIGGER fail_control_event BEFORE INSERT ON control_plane_events BEGIN SELECT RAISE(ABORT,'injected event failure'); END;").unwrap();
        let result = svc.submit(
            &scope("p", vec![Authority::Control]),
            &Submission {
                execution_id: "e".into(),
                project_id: "p".into(),
                idempotency_key: "k".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: "[]".into(),
            },
        );
        assert!(result.is_err());
        let count: i64 = db
            .lock()
            .unwrap()
            .conn()
            .query_row("SELECT count(*) FROM control_plane_executions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn failed_reservation_rolls_back_the_execution_claim() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(Mutex::new(db));
        let svc = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 9);
        svc.register_project("p", "/p", 0, None).unwrap();
        svc.submit(
            &scope("p", vec![Authority::Control]),
            &Submission {
                execution_id: "e".into(),
                project_id: "p".into(),
                idempotency_key: "k".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: "[]".into(),
            },
        )
        .unwrap();
        db.lock().unwrap().conn().execute_batch("CREATE TRIGGER fail_reservation_event BEFORE INSERT ON reservation_events BEGIN SELECT RAISE(ABORT,'injected reservation failure'); END;").unwrap();
        assert!(svc.claim_next().is_err());
        let locked = db.lock().unwrap();
        let state: String = locked
            .conn()
            .query_row(
                "SELECT state FROM control_plane_executions WHERE execution_id='e'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let reservations: i64 = locked
            .conn()
            .query_row("SELECT count(*) FROM resource_reservations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(state, "queued");
        assert_eq!(reservations, 0);
    }
    #[test]
    fn ceilings_pause_and_archival_are_enforced() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(Mutex::new(db));
        let svc = ControlPlaneService::new(
            db.clone(),
            SchedulingPolicy {
                global_ceiling: 2,
                default_project_ceiling: 1,
            },
            2,
        );
        let op = scope("p", vec![Authority::Control]);
        for p in ["p", "q"] {
            svc.register_project(p, &format!("/{p}"), 0, None).unwrap();
        }
        for (e, p) in [("e1", "p"), ("e2", "p"), ("e3", "q")] {
            svc.submit(
                &scope(p, vec![Authority::Control]),
                &Submission {
                    execution_id: e.into(),
                    project_id: p.into(),
                    idempotency_key: e.into(),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: String::new(),
                },
            )
            .unwrap();
        }
        let first = svc.claim_next().unwrap().unwrap();
        let second = svc.claim_next().unwrap().unwrap();
        assert!(svc.claim_next().unwrap().is_none());
        {
            let locked = db.lock().unwrap();
            let held: i64 = locked
                .conn()
                .query_row(
                    "SELECT count(*) FROM resource_reservations WHERE owner_kind='control-plane-worker' AND state='held'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let global_available: i64 = locked
                .conn()
                .query_row(
                    "SELECT available FROM resource_pools WHERE pool_id='control-plane:global' AND resource_type='inference-slots'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let running_accounting: i64 = locked
                .conn()
                .query_row(
                    "SELECT count(*) FROM execution_history WHERE outcome='running' AND agent='control-worker'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!((held, global_available), (2, 0));
            assert_eq!(running_accounting, 2);
        }
        svc.finish(&first, ExecutionState::Completed, "fixture")
            .unwrap();
        let released: i64 = db
            .lock()
            .unwrap()
            .conn()
            .query_row(
                "SELECT count(*) FROM resource_reservations WHERE execution_id=?1 AND state='released'",
                [&first],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(released, 1);
        let unknown_observation: String = db
            .lock()
            .unwrap()
            .conn()
            .query_row(
                "SELECT unknown_reason FROM usage_observations WHERE execution_id=?1",
                [&first],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unknown_observation, "provider usage was not observed");
        assert_ne!(first, second);
        let mut locked = db.lock().unwrap();
        assert!(ControlPlaneRepository::new(locked.conn_mut())
            .set_project_state("p", "archived")
            .unwrap());
        drop(locked);
        assert!(svc
            .submit(
                &op,
                &Submission {
                    execution_id: "x".into(),
                    project_id: "p".into(),
                    idempotency_key: "x".into(),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: String::new()
                }
            )
            .is_err());
    }
    #[test]
    fn equal_priority_projects_are_selected_in_durable_round_robin_order() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let svc =
            ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 1);
        for p in ["p", "q"] {
            svc.register_project(p, &format!("/{p}"), 0, None).unwrap();
        }
        for (id, p) in [("p1", "p"), ("p2", "p"), ("q1", "q")] {
            svc.submit(
                &scope(p, vec![Authority::Control]),
                &Submission {
                    execution_id: id.into(),
                    project_id: p.into(),
                    idempotency_key: id.into(),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: "[]".into(),
                },
            )
            .unwrap();
        }
        assert_eq!(svc.claim_next().unwrap(), Some("p1".into()));
        svc.finish("p1", ExecutionState::Completed, "fixture")
            .unwrap();
        assert_eq!(svc.claim_next().unwrap(), Some("q1".into()));
    }
    #[test]
    fn scoped_capability_is_hashed_and_escalation_stays_pending() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(Mutex::new(db));
        let svc = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 1);
        svc.register_project("p", "/p", 0, None).unwrap();
        let op = scope("p", vec![Authority::Control]);
        svc.submit(
            &op,
            &Submission {
                execution_id: "e".into(),
                project_id: "p".into(),
                idempotency_key: "k".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: "[]".into(),
            },
        )
        .unwrap();
        let internal = CapabilityScope {
            client_class: ClientClass::Internal,
            project_id: None,
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control],
        };
        let grant = svc
            .mint_session(
                &internal,
                CapabilityScope {
                    client_class: ClientClass::Mcp,
                    project_id: Some("p".into()),
                    execution_id: Some("e".into()),
                    attempt: Some(1),
                    worker_id: Some("w".into()),
                    authorities: vec![Authority::Observe, Authority::RequestEscalation],
                },
                60,
            )
            .unwrap();
        assert!(!format!("{:?}", db.lock().unwrap().conn()).contains(&grant.credential));
        let gate = svc
            .request_escalation(&grant.credential, "{\"capability\":\"network\"}")
            .unwrap();
        assert!(gate.starts_with("gate-e-"));
        let state: String = db
            .lock()
            .unwrap()
            .conn()
            .query_row(
                "SELECT state FROM control_plane_pending_gates WHERE gate_id=?1",
                [gate],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "pending");
    }

    #[test]
    fn worker_capability_exposes_only_its_assignment_and_append_operations() {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(Mutex::new(db));
        let svc = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 3);
        svc.register_project("p", "/p", 0, None).unwrap();
        svc.register_project("q", "/q", 0, None).unwrap();
        let op = scope("p", vec![Authority::Control, Authority::Observe]);
        svc.submit(
            &op,
            &Submission {
                execution_id: "e".into(),
                project_id: "p".into(),
                idempotency_key: "k".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: "[]".into(),
            },
        )
        .unwrap();
        assert_eq!(svc.claim_next().unwrap(), Some("e".into()));
        svc.bind_worker("e", "worker-1", std::process::id(), "fixture", "hash")
            .unwrap();
        let grant = svc.mint_worker_session("e", "worker-1").unwrap();
        let view = svc.agent_view(&grant.credential).unwrap();
        assert_eq!(
            (
                view.project_id.as_str(),
                view.execution_id.as_str(),
                view.worker_id.as_str()
            ),
            ("p", "e", "worker-1")
        );
        assert!(svc
            .report_agent_event(
                &grant.credential,
                Authority::ReportProgress,
                "agent_progress",
                "{\"stage\":\"review\"}"
            )
            .is_ok());
        let cap = svc.authenticate(&grant.credential).unwrap();
        assert!(svc
            .submit(
                &cap,
                &Submission {
                    execution_id: "other".into(),
                    project_id: "q".into(),
                    idempotency_key: "other".into(),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: "[]".into()
                }
            )
            .unwrap_err()
            .to_string()
            .contains("Control"));
    }

    #[test]
    fn recovery_distinguishes_dead_and_live_orphans_and_retains_late_outcome() {
        fn running(worker: &str) -> ControlPlaneService {
            let db = Database::open_in_memory().unwrap();
            db.run_migrations().unwrap();
            let svc =
                ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 2);
            svc.register_project("p", "/p", 0, None).unwrap();
            svc.submit(
                &scope("p", vec![Authority::Control]),
                &Submission {
                    execution_id: "e".into(),
                    project_id: "p".into(),
                    idempotency_key: "k".into(),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: "[]".into(),
                },
            )
            .unwrap();
            svc.claim_next().unwrap();
            svc.bind_worker("e", worker, 123, "start", "hash").unwrap();
            svc
        }
        let dead = running("dead");
        dead.recover(&[]).unwrap();
        assert_eq!(
            dead.execution_internal("e").unwrap().unwrap().state,
            ExecutionState::Queued
        );
        let live = running("live");
        live.recover(&["live".into()]).unwrap();
        assert_eq!(
            live.execution_internal("e").unwrap().unwrap().state,
            ExecutionState::AmbiguousLiveOrphan
        );
        assert!(live.claim_next().unwrap().is_none());
        assert!(live
            .record_late_worker_outcome("e", "live", "{\"status\":\"completed\"}")
            .unwrap());
        assert!(!live.record_late_worker_outcome("e", "other", "{}").unwrap());
    }
}
