use familiar_ai_core::control_plane::{
    Authority, CapabilityScope, ClientClass, ControlEvent, ExecutionMode, ExecutionRecord,
    ExecutionState, Submission, SubmissionAck,
};
use familiar_ai_core::{FamiliarError, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub struct ControlPlaneRepository<'a> {
    conn: &'a mut Connection,
}

impl<'a> ControlPlaneRepository<'a> {
    pub fn new(conn: &'a mut Connection) -> Self {
        Self { conn }
    }

    pub fn register_project(
        &mut self,
        project_id: &str,
        root: &str,
        priority: i64,
        ceiling: usize,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO control_plane_projects(project_id,root_path,priority,concurrency_ceiling,state,created_at,updated_at)
             VALUES(?1,?2,?3,?4,'active',datetime('now'),datetime('now'))
             ON CONFLICT(project_id) DO UPDATE SET root_path=excluded.root_path,priority=excluded.priority,concurrency_ceiling=excluded.concurrency_ceiling,updated_at=datetime('now')",
            params![project_id, root, priority, ceiling as i64],
        ).map_err(db)?;
        Ok(())
    }

    /// The acknowledgement row and first event are committed together. Errors
    /// roll the whole transaction back, so an uncommitted submission has no
    /// externally visible identity or partial queue state.
    pub fn submit(&mut self, request: &Submission) -> Result<SubmissionAck> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        if let Some((id, cursor)) = tx
            .query_row(
                "SELECT e.execution_id, ev.cursor FROM control_plane_executions e
             JOIN control_plane_events ev ON ev.execution_id=e.execution_id AND ev.kind='submitted'
             WHERE e.idempotency_key=?1 ORDER BY ev.cursor LIMIT 1",
                [&request.idempotency_key],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(db)?
        {
            tx.commit().map_err(db)?;
            return Ok(SubmissionAck {
                execution_id: id,
                duplicate: true,
                event_cursor: cursor,
            });
        }
        tx.execute(
            "INSERT INTO control_plane_executions(execution_id,project_id,idempotency_key,mode,priority,state,command_json,created_at,updated_at)
             SELECT ?1,?2,?3,?4,?5,'queued',?6,datetime('now'),datetime('now')
             WHERE EXISTS(SELECT 1 FROM control_plane_projects WHERE project_id=?2 AND state!='archived')",
            params![request.execution_id, request.project_id, request.idempotency_key, request.mode.as_str(), request.priority, request.command_json],
        ).map_err(db)?;
        if tx.changes() != 1 {
            return Err(FamiliarError::Database(
                "project is unknown or archived; submission refused".into(),
            ));
        }
        let event_id = format!("{}:submitted", request.execution_id);
        tx.execute("INSERT INTO control_plane_events(event_id,execution_id,kind,payload_json,created_at) VALUES(?1,?2,'submitted','{}',datetime('now'))", params![event_id, request.execution_id]).map_err(db)?;
        let cursor = tx.last_insert_rowid();
        tx.commit().map_err(db)?;
        Ok(SubmissionAck {
            execution_id: request.execution_id.clone(),
            duplicate: false,
            event_cursor: cursor,
        })
    }

    pub fn execution(&self, id: &str) -> Result<Option<ExecutionRecord>> {
        self.conn.query_row(
            "SELECT execution_id,project_id,mode,state,attempt,worker_identity,command_json FROM control_plane_executions WHERE execution_id=?1",
            [id], |r| Ok(ExecutionRecord {
                execution_id: r.get(0)?, project_id: r.get(1)?,
                mode: parse_mode(r.get::<_, String>(2)?.as_str()),
                state: parse_state(r.get::<_, String>(3)?.as_str()), attempt: r.get(4)?,
                worker_identity: r.get(5)?, command_json: r.get(6)?,
            }),
        ).optional().map_err(db)
    }

    pub fn project_root(&self, project_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT root_path FROM control_plane_projects WHERE project_id=?1",
                [project_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)
    }

    pub fn worker_candidates(&self) -> Result<Vec<(String, u32, String)>> {
        let mut stmt=self.conn.prepare("SELECT worker_identity,pid,process_start_identity FROM control_plane_workers WHERE state IN ('launching','running','adopted')").map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get(2)?))
            })
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    pub fn worker_pid(&self, execution: &str) -> Result<Option<u32>> {
        self.conn.query_row("SELECT w.pid FROM control_plane_workers w JOIN control_plane_executions e ON e.worker_identity=w.worker_identity WHERE e.execution_id=?1 AND w.state IN ('launching','running','adopted')",[execution],|r|r.get::<_,i64>(0).map(|v|v as u32)).optional().map_err(db)
    }

    pub fn finish(&mut self, id: &str, state: ExecutionState, reason: &str) -> Result<bool> {
        let name = state_name(state);
        if !matches!(
            state,
            ExecutionState::Completed
                | ExecutionState::Failed
                | ExecutionState::Cancelled
                | ExecutionState::ForegroundEnded
        ) {
            return Err(FamiliarError::Database(
                "invalid terminal execution state".into(),
            ));
        }
        let tx = self.conn.transaction().map_err(db)?;
        let changed = tx.execute("UPDATE control_plane_executions SET state=?2,completed_at=datetime('now'),updated_at=datetime('now') WHERE execution_id=?1 AND state IN ('running','queued')", params![id,name]).map_err(db)? == 1;
        if changed {
            tx.execute("INSERT INTO control_plane_events(event_id,execution_id,kind,payload_json,created_at) VALUES(?1,?2,?3,json_object('reason',?4),datetime('now'))", params![format!("{id}:{name}"),id,name,reason]).map_err(db)?;
            tx.execute("INSERT INTO control_plane_events(event_id,execution_id,kind,payload_json,created_at) VALUES(?1,?2,'usage_finalized','{\"known\":false}',datetime('now'))",params![format!("{id}:usage_finalized"),id]).map_err(db)?;
            tx.execute(
                "UPDATE execution_history SET ended_at=datetime('now'),duration_ms=MAX(0,CAST((julianday('now')-julianday(started_at))*86400000 AS INTEGER)),outcome=?2,unavailable_fields=json_object('usage','not_observed','model','not_reported') WHERE execution_id=?1 AND outcome='running'",
                params![id,if state==ExecutionState::Completed{"succeeded"}else{"failed"}],
            ).map_err(db)?;
            tx.execute(
                "INSERT OR IGNORE INTO accounting_evidence(evidence_id,execution_id,adapter,usage_json,observed_at,terminal_status,source_event_hash)
                 SELECT ?1,?2,'control-worker','{\"known\":false}',datetime('now'),?3,?4 WHERE EXISTS(SELECT 1 FROM execution_history WHERE execution_id=?2)",
                params![format!("{id}:usage-evidence"),id,name,format!("control-plane:{id}:usage-finalized")],
            ).map_err(db)?;
            tx.execute(
                "INSERT OR IGNORE INTO usage_observations(observation_id,evidence_id,project_id,execution_id,attempt_id,stage,worker_identity,adapter,unknown_reason,period_start,period_end,observed_at,ingested_at)
                 SELECT ?1,?2,e.project_id,e.execution_id,CAST(e.attempt AS TEXT),COALESCE(e.stage,'execution'),COALESCE(e.worker_identity,'unbound'),'control-worker','provider usage was not observed',h.started_at,datetime('now'),datetime('now'),datetime('now')
                 FROM control_plane_executions e JOIN execution_history h ON h.execution_id=e.execution_id JOIN accounting_evidence a ON a.evidence_id=?2 WHERE e.execution_id=?3",
                params![format!("{id}:usage-observation"),format!("{id}:usage-evidence"),id],
            ).map_err(db)?;
            release_control_plane_reservation(&tx, id, "execution_terminal")?;
        }
        tx.commit().map_err(db)?;
        Ok(changed)
    }

    pub fn bind_worker(
        &mut self,
        execution_id: &str,
        identity: &str,
        pid: u32,
        start: &str,
        token_hash: &str,
        generation: u64,
    ) -> Result<()> {
        let tx = self.conn.transaction().map_err(db)?;
        let attempt: i64 = tx.query_row("SELECT attempt FROM control_plane_executions WHERE execution_id=?1 AND state='running'", [execution_id], |r| r.get(0)).map_err(db)?;
        tx.execute("INSERT INTO control_plane_workers(worker_identity,execution_id,attempt,pid,process_start_identity,launch_token_hash,owner_generation,state,started_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'running',datetime('now'))", params![identity,execution_id,attempt,pid as i64,start,token_hash,generation as i64]).map_err(db)?;
        tx.execute(
            "UPDATE control_plane_executions SET worker_identity=?2 WHERE execution_id=?1",
            params![execution_id, identity],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(())
    }

    pub fn store_session(
        &mut self,
        hash: &str,
        scope: &CapabilityScope,
        expires_at: &str,
    ) -> Result<()> {
        self.conn.execute("INSERT INTO control_plane_capability_sessions(session_hash,client_class,project_id,execution_id,attempt,worker_id,authority_json,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![hash, client_class_name(scope.client_class), scope.project_id, scope.execution_id, scope.attempt, scope.worker_id, serde_json::to_string(&scope.authorities).map_err(|e| FamiliarError::Database(e.to_string()))?, expires_at]).map_err(db)?;
        Ok(())
    }

    pub fn session(&self, hash: &str) -> Result<Option<CapabilityScope>> {
        self.conn.query_row("SELECT client_class,project_id,execution_id,attempt,worker_id,authority_json FROM control_plane_capability_sessions WHERE session_hash=?1 AND revoked_at IS NULL AND expires_at>datetime('now')", [hash], |r| {
            let authorities: String = r.get(5)?;
            Ok(CapabilityScope { client_class: parse_client(&r.get::<_, String>(0)?), project_id:r.get(1)?, execution_id:r.get(2)?, attempt:r.get(3)?, worker_id:r.get(4)?, authorities: serde_json::from_str::<Vec<Authority>>(&authorities).unwrap_or_default() })
        }).optional().map_err(db)
    }

    pub fn revoke_session(&mut self, hash: &str) -> Result<bool> {
        Ok(self.conn.execute("UPDATE control_plane_capability_sessions SET revoked_at=datetime('now') WHERE session_hash=?1 AND revoked_at IS NULL",[hash]).map_err(db)?==1)
    }

    pub fn pending_gate(
        &mut self,
        hash: &str,
        scope: &CapabilityScope,
        request: &str,
    ) -> Result<String> {
        let project = scope
            .project_id
            .as_deref()
            .ok_or_else(|| FamiliarError::Database("capability is not project scoped".into()))?;
        let execution = scope
            .execution_id
            .as_deref()
            .ok_or_else(|| FamiliarError::Database("capability is not execution scoped".into()))?;
        let id = format!(
            "gate-{}-{}",
            execution,
            self.conn
                .query_row(
                    "SELECT count(*) FROM control_plane_pending_gates WHERE execution_id=?1",
                    [execution],
                    |r| r.get::<_, i64>(0)
                )
                .map_err(db)?
                + 1
        );
        self.conn.execute("INSERT INTO control_plane_pending_gates(gate_id,project_id,execution_id,requested_by_session_hash,request_json,created_at) VALUES(?1,?2,?3,?4,?5,datetime('now'))", params![id,project,execution,hash,request]).map_err(db)?;
        Ok(id)
    }

    pub fn append_agent_event(
        &mut self,
        execution: &str,
        kind: &str,
        payload: &str,
    ) -> Result<i64> {
        if !matches!(kind, "agent_progress" | "agent_evidence") {
            return Err(FamiliarError::Database(
                "agent event kind is not permitted".into(),
            ));
        }
        serde_json::from_str::<serde_json::Value>(payload).map_err(|_| {
            FamiliarError::Database("agent event payload must be valid JSON".into())
        })?;
        let sequence: i64 = self.conn.query_row(
            "SELECT COALESCE(count(*),0)+1 FROM control_plane_events WHERE execution_id=?1 AND kind=?2",
            params![execution,kind], |r| r.get(0)).map_err(db)?;
        self.conn.execute("INSERT INTO control_plane_events(event_id,execution_id,kind,payload_json,created_at) VALUES(?1,?2,?3,?4,datetime('now'))",params![format!("{execution}:{kind}:{sequence}"),execution,kind,payload]).map_err(db)?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn reservation_view(&self, execution: &str) -> Result<Option<String>> {
        let mut stmt=self.conn.prepare("SELECT r.reservation_id,r.state,i.pool_id,i.resource_type,i.granted_amount,COALESCE(i.observed_amount,0) FROM resource_reservations r JOIN resource_reservation_items i ON i.reservation_id=r.reservation_id WHERE r.execution_id=?1 AND r.state='held' ORDER BY r.reservation_id,i.pool_id,i.resource_type").map_err(db)?;
        let rows=stmt.query_map([execution],|r|Ok(serde_json::json!({"reservation_id":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"pool_id":r.get::<_,String>(2)?,"resource_type":r.get::<_,String>(3)?,"remaining":r.get::<_,i64>(4)?.saturating_sub(r.get::<_,i64>(5)?)}))).map_err(db)?.collect::<std::result::Result<Vec<_>,_>>().map_err(db)?;
        if rows.is_empty() {
            Ok(None)
        } else {
            serde_json::to_string(&rows)
                .map(Some)
                .map_err(|e| FamiliarError::Database(e.to_string()))
        }
    }

    pub fn warrant_view(&self, execution: &str) -> Result<Option<String>> {
        self.conn.query_row("SELECT s.warrant_json FROM driver_sessions s JOIN driver_attempts a ON a.session_id=s.session_id WHERE a.execution_id=?1 ORDER BY a.sequence DESC LIMIT 1",[execution],|r|r.get(0)).optional().map_err(db)
    }

    pub fn project_locations(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT project_id,root_path FROM control_plane_projects ORDER BY project_id")
            .map_err(db)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(rows)
    }

    pub fn record_divergence(
        &mut self,
        project: &str,
        execution: Option<&str>,
        kind: &str,
        detail: &str,
    ) -> Result<bool> {
        let id = format!("{}:{}:{}", project, execution.unwrap_or("project"), kind);
        Ok(self.conn.execute("INSERT INTO control_plane_divergences(divergence_id,project_id,execution_id,kind,detail,recorded_at) VALUES(?1,?2,?3,?4,?5,datetime('now')) ON CONFLICT(divergence_id) DO UPDATE SET detail=excluded.detail,recorded_at=excluded.recorded_at WHERE control_plane_divergences.detail!=excluded.detail",params![id,project,execution,kind,detail]).map_err(db)?>0)
    }

    pub fn events_after(
        &self,
        execution_id: &str,
        cursor: i64,
        limit: usize,
    ) -> Result<Vec<ControlEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT cursor,event_id,execution_id,kind,payload_json FROM control_plane_events
             WHERE execution_id=?1 AND cursor>?2 ORDER BY cursor LIMIT ?3",
            )
            .map_err(db)?;
        let events = stmt
            .query_map(params![execution_id, cursor, limit as i64], |r| {
                Ok(ControlEvent {
                    cursor: r.get(0)?,
                    event_id: r.get(1)?,
                    execution_id: r.get(2)?,
                    kind: r.get(3)?,
                    payload_json: r.get(4)?,
                })
            })
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        Ok(events)
    }

    pub fn set_project_state(&mut self, project_id: &str, state: &str) -> Result<bool> {
        if !matches!(state, "active" | "paused" | "archived") {
            return Err(FamiliarError::Database("invalid project state".into()));
        }
        Ok(self.conn.execute("UPDATE control_plane_projects SET state=?2,updated_at=datetime('now') WHERE project_id=?1", params![project_id,state]).map_err(db)? == 1)
    }

    /// Atomically selects one eligible project by priority and least-recent
    /// claim, while enforcing both project and global running ceilings.
    pub fn claim_next(
        &mut self,
        global_ceiling: usize,
        owner_generation: u64,
    ) -> Result<Option<String>> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let occupied: i64 = tx
            .query_row(
                "SELECT count(*) FROM control_plane_executions WHERE state IN ('running','ambiguous_live_orphan')",
                [],
                |r| r.get(0),
            )
            .map_err(db)?;
        ensure_slot_pool(&tx, "control-plane:global", global_ceiling, occupied)?;
        if occupied >= global_ceiling as i64 {
            tx.commit().map_err(db)?;
            return Ok(None);
        }
        let selected: Option<(String, String, i64, i64)> = tx.query_row(
            "SELECT e.execution_id,e.project_id,e.attempt+1,p.concurrency_ceiling FROM control_plane_executions e JOIN control_plane_projects p ON p.project_id=e.project_id
             WHERE e.state='queued' AND p.state='active' AND
               (SELECT count(*) FROM control_plane_executions r WHERE r.project_id=e.project_id AND r.state IN ('running','ambiguous_live_orphan')) < p.concurrency_ceiling
             ORDER BY p.priority DESC, p.last_claim_sequence ASC, e.priority DESC,
               e.created_at, e.execution_id LIMIT 1",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional().map_err(db)?;
        if let Some((ref execution_id, ref project_id, attempt, project_ceiling)) = selected {
            let project_occupied: i64 = tx.query_row("SELECT count(*) FROM control_plane_executions WHERE project_id=?1 AND state IN ('running','ambiguous_live_orphan')",[project_id],|r|r.get(0)).map_err(db)?;
            let project_pool = format!("control-plane:project:{project_id}");
            ensure_slot_pool(
                &tx,
                &project_pool,
                project_ceiling as usize,
                project_occupied,
            )?;
            acquire_control_plane_reservation(
                &tx,
                execution_id,
                project_id,
                attempt,
                owner_generation,
                &project_pool,
            )?;
            tx.execute("UPDATE control_plane_executions SET state='running',attempt=attempt+1,claim_generation=?2,updated_at=datetime('now') WHERE execution_id=?1 AND state='queued'", params![execution_id, owner_generation as i64]).map_err(db)?;
            let event_id = format!("{}:claimed:{}:{}", execution_id, owner_generation, attempt);
            tx.execute("INSERT INTO control_plane_events(event_id,execution_id,kind,payload_json,created_at) VALUES(?1,?2,'claimed','{}',datetime('now'))", params![event_id,execution_id]).map_err(db)?;
            tx.execute("UPDATE control_plane_projects SET last_claim_sequence=(SELECT COALESCE(MAX(last_claim_sequence),0)+1 FROM control_plane_projects) WHERE project_id=(SELECT project_id FROM control_plane_executions WHERE execution_id=?1)",[execution_id]).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(selected.map(|(id, _, _, _)| id))
    }

    pub fn recover_running(&mut self, live_worker_identities: &[String]) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT execution_id,worker_identity FROM control_plane_executions WHERE state='running'").map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(db)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db)?;
        drop(stmt);
        let tx = self.conn.transaction().map_err(db)?;
        let mut changed = 0;
        for (id, worker) in rows {
            let (state, kind) = if worker
                .as_ref()
                .is_some_and(|w| live_worker_identities.contains(w))
            {
                ("ambiguous_live_orphan", "ambiguous_live_orphan")
            } else {
                ("queued", "runner_interrupted")
            };
            changed += tx.execute("UPDATE control_plane_executions SET state=?2,updated_at=datetime('now') WHERE execution_id=?1 AND state='running'", params![id,state]).map_err(db)?;
            if let Some(ref worker_id) = worker {
                tx.execute("UPDATE control_plane_workers SET state=?2,exit_reason=?3,ended_at=CASE WHEN ?2='exited' THEN datetime('now') ELSE ended_at END WHERE worker_identity=?1",params![worker_id,if state=="ambiguous_live_orphan"{"ambiguous"}else{"exited"},kind]).map_err(db)?;
            }
            if state == "queued" {
                release_control_plane_reservation(&tx, &id, "runner_interrupted")?;
            }
            tx.execute("INSERT INTO control_plane_events(event_id,execution_id,kind,payload_json,created_at) VALUES(?1,?2,?3,'{}',datetime('now'))", params![format!("{}:recovery:{}",id,kind),id,kind]).map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(changed)
    }

    pub fn record_late_outcome(
        &mut self,
        execution: &str,
        worker: &str,
        payload: &str,
    ) -> Result<bool> {
        serde_json::from_str::<serde_json::Value>(payload).map_err(|_| {
            FamiliarError::Database("late worker outcome must be valid JSON".into())
        })?;
        let attempt: Option<i64> = self.conn.query_row("SELECT attempt FROM control_plane_executions WHERE execution_id=?1 AND state='ambiguous_live_orphan' AND worker_identity=?2",params![execution,worker],|r|r.get(0)).optional().map_err(db)?;
        let Some(attempt) = attempt else {
            return Ok(false);
        };
        self.conn.execute("INSERT OR IGNORE INTO control_plane_events(event_id,execution_id,kind,payload_json,created_at) VALUES(?1,?2,'late_worker_outcome',?3,datetime('now'))",params![format!("{execution}:late:{worker}:{attempt}"),execution,payload]).map_err(db)?;
        Ok(true)
    }
}

fn parse_mode(s: &str) -> ExecutionMode {
    match s {
        "attached" => ExecutionMode::Attached,
        "foreground_only" => ExecutionMode::ForegroundOnly,
        _ => ExecutionMode::Detached,
    }
}

fn ensure_slot_pool(
    tx: &rusqlite::Transaction<'_>,
    pool_id: &str,
    capacity: usize,
    occupied: i64,
) -> Result<()> {
    let capacity = i64::try_from(capacity)
        .map_err(|_| FamiliarError::Database("concurrency ceiling exceeds SQLite range".into()))?;
    let available = capacity.saturating_sub(occupied).max(0);
    tx.execute(
        "INSERT INTO resource_pools(pool_id,resource_type,capacity,available,renewable)
         VALUES(?1,'inference-slots',?2,?3,1)
         ON CONFLICT(pool_id,resource_type) DO UPDATE SET capacity=excluded.capacity,available=excluded.available,renewable=1",
        params![pool_id, capacity, available],
    )
    .map_err(db)?;
    Ok(())
}

fn acquire_control_plane_reservation(
    tx: &rusqlite::Transaction<'_>,
    execution: &str,
    project: &str,
    attempt: i64,
    generation: u64,
    project_pool: &str,
) -> Result<()> {
    for pool in ["control-plane:global", project_pool] {
        if tx
            .execute(
                "UPDATE resource_pools SET available=available-1 WHERE pool_id=?1 AND resource_type='inference-slots' AND available>=1",
                [pool],
            )
            .map_err(db)?
            != 1
        {
            return Err(FamiliarError::Database(format!(
                "atomic concurrency reservation unavailable for {pool}"
            )));
        }
    }
    let reservation_id = format!("control-plane:{execution}:{attempt}");
    let sequence: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(arrival_sequence),0)+1 FROM resource_reservations",
            [],
            |row| row.get(0),
        )
        .map_err(db)?;
    let installation: Option<String> = tx
        .query_row(
            "SELECT installation_id FROM control_plane_installation WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?;
    tx.execute(
        "INSERT INTO resource_reservations(reservation_id,owner_instance_id,installation_id,nonce_or_generation,owner_kind,project_id,execution_id,component_id,state,arrival_sequence,acquired_at)
         VALUES(?1,?2,?3,?4,'control-plane-worker',?5,?6,?7,'held',?8,datetime('now'))",
        params![reservation_id,format!("{execution}:{attempt}"),installation,generation.to_string(),project,execution,format!("attempt-{attempt}"),sequence],
    ).map_err(db)?;
    for pool in ["control-plane:global", project_pool] {
        tx.execute(
            "INSERT INTO resource_reservation_items(reservation_id,pool_id,resource_type,requested_amount,granted_amount) VALUES(?1,?2,'inference-slots',1,1)",
            params![reservation_id,pool],
        ).map_err(db)?;
    }
    tx.execute(
        "INSERT INTO reservation_events(reservation_id,event_type,actor,detail,occurred_at) VALUES(?1,'acquired','control-plane','atomic execution claim',datetime('now'))",
        [reservation_id],
    ).map_err(db)?;
    tx.execute(
        "INSERT OR IGNORE INTO execution_history(execution_id,started_at,agent,outcome,repository,worktree,prd_path,unavailable_fields)
         SELECT ?1,datetime('now'),'control-worker','running',p.project_id,p.root_path,'',json_object('usage','not_yet_observed','model','not_yet_reported')
         FROM control_plane_projects p WHERE p.project_id=?2",
        params![execution,project],
    ).map_err(db)?;
    Ok(())
}

fn release_control_plane_reservation(
    tx: &rusqlite::Transaction<'_>,
    execution: &str,
    reason: &str,
) -> Result<()> {
    let reservation: Option<String> = tx
        .query_row(
            "SELECT reservation_id FROM resource_reservations WHERE execution_id=?1 AND owner_kind='control-plane-worker' AND state='held' ORDER BY arrival_sequence DESC LIMIT 1",
            [execution],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?;
    let Some(reservation) = reservation else {
        return Ok(());
    };
    let mut statement = tx
        .prepare("SELECT pool_id,resource_type,granted_amount FROM resource_reservation_items WHERE reservation_id=?1")
        .map_err(db)?;
    let items = statement
        .query_map([&reservation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(db)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db)?;
    drop(statement);
    for (pool, resource, amount) in items {
        tx.execute(
            "UPDATE resource_pools SET available=MIN(capacity,available+?1) WHERE pool_id=?2 AND resource_type=?3",
            params![amount,pool,resource],
        ).map_err(db)?;
    }
    tx.execute(
        "UPDATE resource_reservations SET state='released',resolved_at=datetime('now') WHERE reservation_id=?1 AND state='held'",
        [&reservation],
    ).map_err(db)?;
    tx.execute(
        "INSERT INTO reservation_events(reservation_id,event_type,actor,detail,occurred_at) VALUES(?1,'released','control-plane',?2,datetime('now'))",
        params![reservation,reason],
    ).map_err(db)?;
    Ok(())
}
fn parse_state(s: &str) -> ExecutionState {
    match s {
        "running" => ExecutionState::Running,
        "paused" => ExecutionState::Paused,
        "completed" => ExecutionState::Completed,
        "failed" => ExecutionState::Failed,
        "cancelled" => ExecutionState::Cancelled,
        "foreground_ended" => ExecutionState::ForegroundEnded,
        "ambiguous_live_orphan" => ExecutionState::AmbiguousLiveOrphan,
        _ => ExecutionState::Queued,
    }
}
fn state_name(s: ExecutionState) -> &'static str {
    match s {
        ExecutionState::Queued => "queued",
        ExecutionState::Running => "running",
        ExecutionState::Paused => "paused",
        ExecutionState::Completed => "completed",
        ExecutionState::Failed => "failed",
        ExecutionState::Cancelled => "cancelled",
        ExecutionState::ForegroundEnded => "foreground_ended",
        ExecutionState::AmbiguousLiveOrphan => "ambiguous_live_orphan",
    }
}
fn client_class_name(c: ClientClass) -> &'static str {
    match c {
        ClientClass::Operator => "operator",
        ClientClass::Observer => "observer",
        ClientClass::Mcp => "mcp",
        ClientClass::Worker => "worker",
        ClientClass::Internal => "internal",
    }
}
fn parse_client(s: &str) -> ClientClass {
    match s {
        "operator" => ClientClass::Operator,
        "observer" => ClientClass::Observer,
        "mcp" => ClientClass::Mcp,
        "worker" => ClientClass::Worker,
        _ => ClientClass::Internal,
    }
}

fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

#[allow(dead_code)]
fn _mode_name(mode: ExecutionMode) -> &'static str {
    mode.as_str()
}
