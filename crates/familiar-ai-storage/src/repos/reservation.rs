use chrono::Utc;
use familiar_ai_core::{
    FamiliarError, GrantMode, OwnerLiveness, OwnerLivenessEvidence, ReservationOwnerIdentity,
    ResourceRequest, ResourceType, UnknownConsumptionPolicy,
};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationGrantItem {
    pub pool_id: String,
    pub resource_type: ResourceType,
    pub requested_amount: u64,
    pub granted_amount: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationGrant {
    pub reservation_id: String,
    pub arrival_sequence: u64,
    pub items: Vec<ReservationGrantItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    Granted(ReservationGrant),
    Refused { unavailable: Vec<ResourceRequest> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementObservation {
    Known(Vec<ResourceRequest>),
    Unknown { policy: UnknownConsumptionPolicy },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementResult {
    pub state: String,
    pub unknown_consumption: bool,
    pub overrun: bool,
}

pub struct ReservationRepository<'a> {
    conn: &'a mut Connection,
}

impl<'a> ReservationRepository<'a> {
    pub fn new(conn: &'a mut Connection) -> Self {
        Self { conn }
    }

    pub fn define_pool(
        &mut self,
        pool_id: &str,
        resource_type: &ResourceType,
        capacity: u64,
        renewable: bool,
    ) -> familiar_ai_core::Result<()> {
        let capacity = sql_amount(capacity)?;
        self.conn
            .execute(
                "INSERT INTO resource_pools(pool_id,resource_type,capacity,available,renewable) VALUES(?1,?2,?3,?3,?4)
                 ON CONFLICT(pool_id,resource_type) DO UPDATE SET capacity=excluded.capacity,
                   available=resource_pools.available+(excluded.capacity-resource_pools.capacity),renewable=excluded.renewable
                 WHERE resource_pools.available+(excluded.capacity-resource_pools.capacity)>=0",
                params![pool_id, resource_type.as_str(), capacity, renewable],
            )
            .map_err(db)?;
        Ok(())
    }

    /// Atomically grants every request, or none. Partial mode grants each
    /// positive remainder and records the requested and granted amounts.
    pub fn acquire(
        &mut self,
        owner: &ReservationOwnerIdentity,
        requests: &[ResourceRequest],
        mode: GrantMode,
        expires_at: Option<&str>,
    ) -> familiar_ai_core::Result<AcquireOutcome> {
        owner.validate().map_err(FamiliarError::Config)?;
        validate_requests(requests)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let mut unavailable = Vec::new();
        let mut grants = Vec::new();
        for request in requests {
            let available: Option<i64> = tx
                .query_row(
                    "SELECT available FROM resource_pools WHERE pool_id=?1 AND resource_type=?2",
                    params![request.pool_id, request.resource_type.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db)?;
            let available = available.unwrap_or(0).max(0) as u64;
            if available < request.amount {
                unavailable.push(request.clone());
            }
            let granted = match mode {
                GrantMode::AllOrNothing => request.amount,
                GrantMode::Partial => request.amount.min(available),
            };
            if granted > 0 {
                grants.push(ReservationGrantItem {
                    pool_id: request.pool_id.clone(),
                    resource_type: request.resource_type.clone(),
                    requested_amount: request.amount,
                    granted_amount: granted,
                });
            }
        }
        if (!unavailable.is_empty() && mode == GrantMode::AllOrNothing) || grants.is_empty() {
            tx.rollback().map_err(db)?;
            return Ok(AcquireOutcome::Refused { unavailable });
        }
        let reservation_id = format!("res_{}", random_hex()?);
        let sequence: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(arrival_sequence),0)+1 FROM resource_reservations",
                [],
                |row| row.get(0),
            )
            .map_err(db)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO resource_reservations(reservation_id,owner_instance_id,installation_id,nonce_or_generation,owner_kind,project_id,execution_id,component_id,state,arrival_sequence,acquired_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'held',?9,?10,?11)",
            params![reservation_id,owner.owner_instance_id,owner.installation_id,owner.nonce_or_generation,owner.owner_kind,owner.project_id,owner.execution_id,owner.component_id,sequence,now,expires_at],
        ).map_err(db)?;
        for grant in &grants {
            let changed = tx.execute(
                "UPDATE resource_pools SET available=available-?1 WHERE pool_id=?2 AND resource_type=?3 AND available>=?1",
                params![sql_amount(grant.granted_amount)?,grant.pool_id,grant.resource_type.as_str()],
            ).map_err(db)?;
            if changed != 1 {
                return Err(FamiliarError::Database(
                    "resource capacity changed during atomic acquisition".into(),
                ));
            }
            tx.execute(
                "INSERT INTO resource_reservation_items(reservation_id,pool_id,resource_type,requested_amount,granted_amount) VALUES(?1,?2,?3,?4,?5)",
                params![reservation_id,grant.pool_id,grant.resource_type.as_str(),sql_amount(grant.requested_amount)?,sql_amount(grant.granted_amount)?],
            ).map_err(db)?;
        }
        event(
            &tx,
            &reservation_id,
            "acquired",
            &owner.owner_instance_id,
            match mode {
                GrantMode::AllOrNothing => "all-or-nothing",
                GrantMode::Partial => "partial",
            },
        )?;
        tx.commit().map_err(db)?;
        Ok(AcquireOutcome::Granted(ReservationGrant {
            reservation_id,
            arrival_sequence: sequence as u64,
            items: grants,
        }))
    }

    pub fn settle(
        &mut self,
        reservation_id: &str,
        observation: SettlementObservation,
        actor: &str,
    ) -> familiar_ai_core::Result<SettlementResult> {
        let tx = self.conn.transaction().map_err(db)?;
        require_held(&tx, reservation_id)?;
        if matches!(
            observation,
            SettlementObservation::Unknown {
                policy: UnknownConsumptionPolicy::HoldReservation
            }
        ) {
            tx.execute(
                "UPDATE resource_reservations SET unknown_consumption=1 WHERE reservation_id=?1",
                [reservation_id],
            )
            .map_err(db)?;
            event(
                &tx,
                reservation_id,
                "unknown-consumption-held",
                actor,
                "reservation retained conservatively",
            )?;
            tx.commit().map_err(db)?;
            return Ok(SettlementResult {
                state: "held".into(),
                unknown_consumption: true,
                overrun: false,
            });
        }
        let unknown = matches!(observation, SettlementObservation::Unknown { .. });
        let known = match observation {
            SettlementObservation::Known(items) => Some(items),
            SettlementObservation::Unknown { .. } => None,
        };
        let items = reservation_items(&tx, reservation_id)?;
        let mut any_overrun = false;
        for item in items {
            let observed = known
                .as_ref()
                .and_then(|values| {
                    values
                        .iter()
                        .find(|v| {
                            v.pool_id == item.pool_id && v.resource_type == item.resource_type
                        })
                        .map(|v| v.amount)
                })
                .unwrap_or(item.granted_amount);
            let overrun = observed.saturating_sub(item.granted_amount);
            any_overrun |= overrun > 0;
            let returned = item.granted_amount.saturating_sub(observed);
            tx.execute("UPDATE resource_pools SET available=available+?1 WHERE pool_id=?2 AND resource_type=?3", params![sql_amount(returned)?,item.pool_id,item.resource_type.as_str()]).map_err(db)?;
            tx.execute("UPDATE resource_reservation_items SET observed_amount=?1,overrun_amount=?2 WHERE reservation_id=?3 AND pool_id=?4 AND resource_type=?5", params![sql_amount(observed)?,sql_amount(overrun)?,reservation_id,item.pool_id,item.resource_type.as_str()]).map_err(db)?;
        }
        tx.execute("UPDATE resource_reservations SET state='committed',resolved_at=?1,unknown_consumption=?2,overrun=?3 WHERE reservation_id=?4", params![Utc::now().to_rfc3339(),unknown,any_overrun,reservation_id]).map_err(db)?;
        event(
            &tx,
            reservation_id,
            "committed",
            actor,
            if any_overrun {
                "observed consumption overran reservation"
            } else if unknown {
                "unknown consumption settled at reserved amount"
            } else {
                "observed consumption settled"
            },
        )?;
        tx.commit().map_err(db)?;
        Ok(SettlementResult {
            state: "committed".into(),
            unknown_consumption: unknown,
            overrun: any_overrun,
        })
    }

    pub fn commit(
        &mut self,
        reservation_id: &str,
        observed: Vec<ResourceRequest>,
        actor: &str,
    ) -> familiar_ai_core::Result<SettlementResult> {
        self.settle(
            reservation_id,
            SettlementObservation::Known(observed),
            actor,
        )
    }

    pub fn release(&mut self, reservation_id: &str, actor: &str) -> familiar_ai_core::Result<()> {
        self.resolve_returning(reservation_id, "released", actor, "explicit release")
    }

    pub fn expire_due(&mut self, now: &str) -> familiar_ai_core::Result<Vec<String>> {
        let ids: Vec<String> = {
            let mut statement = self.conn.prepare("SELECT reservation_id FROM resource_reservations WHERE state='held' AND expires_at IS NOT NULL AND expires_at<=?1 ORDER BY expires_at,arrival_sequence,owner_instance_id").map_err(db)?;
            let rows = statement
                .query_map([now], |row| row.get(0))
                .map_err(db)?
                .collect::<Result<_, _>>()
                .map_err(db)?;
            rows
        };
        for id in &ids {
            self.resolve_returning(id, "expired", "expiry", "deadline passed")?;
        }
        Ok(ids)
    }

    pub fn renew(
        &mut self,
        reservation_id: &str,
        new_expiry: &str,
        actor: &str,
        justification: &str,
    ) -> familiar_ai_core::Result<()> {
        let tx = self.conn.transaction().map_err(db)?;
        require_held(&tx, reservation_id)?;
        let nonrenewable: i64 = tx.query_row("SELECT count(*) FROM resource_reservation_items i JOIN resource_pools p USING(pool_id,resource_type) WHERE i.reservation_id=?1 AND p.renewable=0", [reservation_id], |r| r.get(0)).map_err(db)?;
        if nonrenewable != 0 || justification.trim().is_empty() {
            return Err(FamiliarError::Config(
                "renewal requires renewable resources and an explicit justification".into(),
            ));
        }
        tx.execute("UPDATE resource_reservations SET expires_at=?1 WHERE reservation_id=?2 AND (expires_at IS NULL OR expires_at<?1)", params![new_expiry,reservation_id]).map_err(db)?;
        event(&tx, reservation_id, "renewed", actor, justification)?;
        tx.commit().map_err(db)
    }

    pub fn recover(
        &mut self,
        reservation_id: &str,
        evidence: &OwnerLivenessEvidence,
    ) -> familiar_ai_core::Result<bool> {
        let tx = self.conn.transaction().map_err(db)?;
        let owner: (String,String) = tx.query_row("SELECT owner_instance_id,nonce_or_generation FROM resource_reservations WHERE reservation_id=?1", [reservation_id], |r| Ok((r.get(0)?,r.get(1)?))).map_err(db)?;
        if owner.0 != evidence.owner_instance_id || owner.1 != evidence.nonce_or_generation {
            return Err(FamiliarError::Config(
                "liveness evidence does not identify the exact reservation owner instance".into(),
            ));
        }
        let resolution = match evidence.resolution {
            OwnerLiveness::Live => "live",
            OwnerLiveness::ProvablyDead => "provably-dead",
            OwnerLiveness::Ambiguous => "ambiguous",
        };
        tx.execute("INSERT INTO reservation_liveness_evidence(reservation_id,owner_instance_id,nonce_or_generation,resolution,provenance,observed_at,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![reservation_id,evidence.owner_instance_id,evidence.nonce_or_generation,resolution,evidence.provenance,evidence.observed_at,Utc::now().to_rfc3339()]).map_err(db)?;
        event(
            &tx,
            reservation_id,
            "liveness-observed",
            &evidence.provenance,
            resolution,
        )?;
        tx.commit().map_err(db)?;
        if evidence.resolution == OwnerLiveness::ProvablyDead {
            self.resolve_returning(
                reservation_id,
                "recovered",
                &evidence.provenance,
                "exact owner instance provably dead",
            )?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn resolve_returning(
        &mut self,
        reservation_id: &str,
        state: &str,
        actor: &str,
        detail: &str,
    ) -> familiar_ai_core::Result<()> {
        let tx = self.conn.transaction().map_err(db)?;
        require_held(&tx, reservation_id)?;
        for item in reservation_items(&tx, reservation_id)? {
            tx.execute("UPDATE resource_pools SET available=available+?1 WHERE pool_id=?2 AND resource_type=?3", params![sql_amount(item.granted_amount)?,item.pool_id,item.resource_type.as_str()]).map_err(db)?;
        }
        tx.execute(
            "UPDATE resource_reservations SET state=?1,resolved_at=?2 WHERE reservation_id=?3",
            params![state, Utc::now().to_rfc3339(), reservation_id],
        )
        .map_err(db)?;
        event(&tx, reservation_id, state, actor, detail)?;
        tx.commit().map_err(db)
    }
}

fn validate_requests(requests: &[ResourceRequest]) -> familiar_ai_core::Result<()> {
    let mut keys = std::collections::BTreeSet::new();
    if requests.is_empty()
        || requests
            .iter()
            .any(|r| r.amount == 0 || !keys.insert((r.pool_id.clone(), r.resource_type.clone())))
    {
        return Err(FamiliarError::Config(
            "reservation requests must be non-empty, positive, and unique by pool/resource".into(),
        ));
    }
    Ok(())
}
fn require_held(tx: &Transaction<'_>, id: &str) -> familiar_ai_core::Result<()> {
    let state: Option<String> = tx
        .query_row(
            "SELECT state FROM resource_reservations WHERE reservation_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db)?;
    match state.as_deref() {
        Some("held") => Ok(()),
        Some(_) => Err(FamiliarError::Database(
            "reservation is already terminal".into(),
        )),
        None => Err(FamiliarError::Database("reservation does not exist".into())),
    }
}
fn reservation_items(
    tx: &Transaction<'_>,
    id: &str,
) -> familiar_ai_core::Result<Vec<ReservationGrantItem>> {
    let mut statement=tx.prepare("SELECT pool_id,resource_type,requested_amount,granted_amount FROM resource_reservation_items WHERE reservation_id=?1 ORDER BY pool_id,resource_type").map_err(db)?;
    let items = statement
        .query_map([id], |r| {
            Ok(ReservationGrantItem {
                pool_id: r.get(0)?,
                resource_type: ResourceType::parse(&r.get::<_, String>(1)?),
                requested_amount: r.get::<_, i64>(2)? as u64,
                granted_amount: r.get::<_, i64>(3)? as u64,
            })
        })
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    Ok(items)
}
fn event(
    tx: &Transaction<'_>,
    id: &str,
    kind: &str,
    actor: &str,
    detail: &str,
) -> familiar_ai_core::Result<()> {
    tx.execute("INSERT INTO reservation_events(reservation_id,event_type,actor,detail,occurred_at) VALUES(?1,?2,?3,?4,?5)", params![id,kind,actor,detail,Utc::now().to_rfc3339()]).map_err(db)?;
    Ok(())
}
fn sql_amount(value: u64) -> familiar_ai_core::Result<i64> {
    i64::try_from(value)
        .map_err(|_| FamiliarError::Config("resource amount exceeds durable integer range".into()))
}
fn random_hex() -> familiar_ai_core::Result<String> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| FamiliarError::Database("secure random generation failed".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn db(error: rusqlite::Error) -> FamiliarError {
    FamiliarError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::{OwnerLivenessEvidence, ReservationOwnerIdentity};

    fn database() -> crate::Database {
        let db = crate::Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }
    fn owner(id: &str) -> ReservationOwnerIdentity {
        ReservationOwnerIdentity {
            owner_instance_id: format!("owner-{id}"),
            installation_id: Some("installation-1".into()),
            nonce_or_generation: format!("nonce-{id}"),
            owner_kind: "drive-component".into(),
            project_id: "project-1".into(),
            execution_id: "execution-1".into(),
            component_id: format!("component-{id}"),
        }
    }
    fn request(kind: ResourceType, amount: u64) -> ResourceRequest {
        ResourceRequest {
            pool_id: "session".into(),
            resource_type: kind,
            amount,
        }
    }
    fn grant(outcome: AcquireOutcome) -> ReservationGrant {
        match outcome {
            AcquireOutcome::Granted(grant) => grant,
            other => panic!("expected grant, got {other:?}"),
        }
    }

    #[test]
    fn all_or_nothing_and_partial_grants_are_atomic() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        repo.define_pool("session", &ResourceType::NanousdBudget, 100, false)
            .unwrap();
        repo.define_pool("session", &ResourceType::UncachedTokens, 10, false)
            .unwrap();
        let requests = vec![
            request(ResourceType::NanousdBudget, 50),
            request(ResourceType::UncachedTokens, 20),
        ];
        assert!(matches!(
            repo.acquire(&owner("a"), &requests, GrantMode::AllOrNothing, None)
                .unwrap(),
            AcquireOutcome::Refused { .. }
        ));
        let partial = grant(
            repo.acquire(&owner("b"), &requests, GrantMode::Partial, None)
                .unwrap(),
        );
        assert_eq!(partial.items[0].granted_amount, 50);
        assert_eq!(partial.items[1].granted_amount, 10);
    }

    #[test]
    fn expiry_renewal_overrun_and_unknown_settlement_are_explicit() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        repo.define_pool("session", &ResourceType::UncachedTokens, 100, true)
            .unwrap();
        let first = grant(
            repo.acquire(
                &owner("renew"),
                &[request(ResourceType::UncachedTokens, 20)],
                GrantMode::AllOrNothing,
                Some("2026-01-01T00:00:00Z"),
            )
            .unwrap(),
        );
        repo.renew(
            &first.reservation_id,
            "2026-02-01T00:00:00Z",
            "owner-renew",
            "component still running",
        )
        .unwrap();
        assert!(repo.expire_due("2026-01-15T00:00:00Z").unwrap().is_empty());
        assert_eq!(
            repo.expire_due("2026-02-02T00:00:00Z").unwrap(),
            vec![first.reservation_id]
        );

        let overrun = grant(
            repo.acquire(
                &owner("overrun"),
                &[request(ResourceType::UncachedTokens, 10)],
                GrantMode::AllOrNothing,
                None,
            )
            .unwrap(),
        );
        let settled = repo
            .settle(
                &overrun.reservation_id,
                SettlementObservation::Known(vec![request(ResourceType::UncachedTokens, 15)]),
                "owner-overrun",
            )
            .unwrap();
        assert!(settled.overrun);

        let unknown = grant(
            repo.acquire(
                &owner("unknown"),
                &[request(ResourceType::UncachedTokens, 10)],
                GrantMode::AllOrNothing,
                None,
            )
            .unwrap(),
        );
        let held = repo
            .settle(
                &unknown.reservation_id,
                SettlementObservation::Unknown {
                    policy: UnknownConsumptionPolicy::HoldReservation,
                },
                "owner-unknown",
            )
            .unwrap();
        assert_eq!(held.state, "held");
        let committed = repo
            .settle(
                &unknown.reservation_id,
                SettlementObservation::Unknown {
                    policy: UnknownConsumptionPolicy::SettleReservedAmount,
                },
                "owner-unknown",
            )
            .unwrap();
        assert!(committed.unknown_consumption);
    }

    #[test]
    fn recovery_requires_exact_provably_dead_owner_evidence() {
        let mut db = database();
        let mut repo = ReservationRepository::new(db.conn_mut());
        repo.define_pool("session", &ResourceType::InferenceSlots, 1, false)
            .unwrap();
        for (suffix, resolution, recovered) in [
            ("live", OwnerLiveness::Live, false),
            ("ambiguous", OwnerLiveness::Ambiguous, false),
            ("dead", OwnerLiveness::ProvablyDead, true),
        ] {
            let identity = owner(suffix);
            let reservation = grant(
                repo.acquire(
                    &identity,
                    &[request(ResourceType::InferenceSlots, 1)],
                    GrantMode::AllOrNothing,
                    None,
                )
                .unwrap(),
            );
            let evidence = OwnerLivenessEvidence {
                owner_instance_id: identity.owner_instance_id,
                nonce_or_generation: identity.nonce_or_generation,
                resolution,
                provenance: "durable-test-provider".into(),
                observed_at: "2026-01-01T00:00:00Z".into(),
            };
            assert_eq!(
                repo.recover(&reservation.reservation_id, &evidence)
                    .unwrap(),
                recovered
            );
            if !recovered {
                repo.release(&reservation.reservation_id, "test").unwrap();
            }
        }
    }

    #[test]
    fn concurrent_claimants_cannot_double_spend_capacity() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let mut setup = crate::Database::open(&path).unwrap();
        setup.run_migrations().unwrap();
        ReservationRepository::new(setup.conn_mut())
            .define_pool("session", &ResourceType::ExclusiveRuntime, 1, false)
            .unwrap();
        drop(setup);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for id in ["a", "b"] {
            let path = path.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let mut db = crate::Database::open(&path).unwrap();
                barrier.wait();
                ReservationRepository::new(db.conn_mut())
                    .acquire(
                        &owner(id),
                        &[request(ResourceType::ExclusiveRuntime, 1)],
                        GrantMode::AllOrNothing,
                        None,
                    )
                    .unwrap()
            }));
        }
        barrier.wait();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, AcquireOutcome::Granted(_)))
                .count(),
            1
        );
    }
}
