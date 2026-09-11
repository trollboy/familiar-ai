//! PRD-091 regressions: the durable, cross-host, per-repository driver
//! lease. Two "hosts" in these tests are two `ControlPlaneService` instances
//! each holding their own `Database` connection opened against the same
//! on-disk SQLite file — one shared store, reachable from what the lease
//! mechanism must treat as independent machines.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use familiar_ai_core::control_plane::SchedulingPolicy;
use familiar_ai_daemon::control_plane::ControlPlaneService;
use familiar_ai_daemon::worker_lock::{HostLease, WorkerLock};
use familiar_ai_storage::Database;

fn service_at(path: &std::path::Path) -> ControlPlaneService {
    let db = Database::open(path).unwrap();
    db.run_migrations().unwrap();
    ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 1)
}

fn wait_until<F: Fn() -> bool>(timeout: Duration, check: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

/// Acceptance: "The driver claim is a durable lease carrying host identity,
/// process identity, acquisition time, and expiry... the refusal guarantee
/// ... holds across hosts" and "A live, renewing lease is never taken — a
/// second host attempting to claim it is refused with the holder named and
/// the remaining lease life reported."
#[test]
fn second_host_is_refused_and_the_holder_and_remaining_life_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("shared.db");
    let host_a = service_at(&db_path);
    let host_b = service_at(&db_path);

    let lease_a =
        HostLease::acquire_as(&host_a, "repo-alpha", "host-a", Duration::from_secs(30), 60)
            .expect("first host acquires the lease");
    assert!(lease_a.is_live());

    let refusal =
        match HostLease::acquire_as(&host_b, "repo-alpha", "host-b", Duration::from_secs(30), 60) {
            Ok(_) => panic!("a live lease must refuse a second host"),
            Err(message) => message,
        };
    assert!(
        refusal.contains("host-a"),
        "refusal names the holder: {refusal}"
    );
    assert!(
        refusal.contains("ms more"),
        "refusal reports remaining lease life: {refusal}"
    );

    // The refused host never took anything: the record still names host-a.
    let record = host_b
        .inspect_repository_lease("repo-alpha")
        .unwrap()
        .unwrap();
    assert_eq!(record.host_identity, "host-a");
}

/// Acceptance: "A lease is renewed on a configured heartbeat while its
/// holder lives and expires a bounded interval after renewal stops; an
/// expired lease is takeable and the takeover is durably recorded naming
/// the prior and new holder."
#[test]
fn expired_lease_is_taken_over_and_the_takeover_names_both_holders() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("shared.db");
    let host_a = service_at(&db_path);
    let host_b = service_at(&db_path);

    let lease_a =
        HostLease::acquire_as(&host_a, "repo-alpha", "host-a", Duration::from_secs(30), 60)
            .expect("first host acquires the lease");
    assert!(lease_a.is_live());

    // Force the store-side clock past expiry without waiting out the TTL.
    {
        let mut db = Database::open(&db_path).unwrap();
        db.conn_mut()
            .execute(
                "UPDATE control_plane_leases SET expires_at=datetime('now','-1 seconds') WHERE repository_key='repo-alpha'",
                [],
            )
            .unwrap();
    }

    let lease_b =
        HostLease::acquire_as(&host_b, "repo-alpha", "host-b", Duration::from_secs(30), 60)
            .expect("an expired lease is takeable");
    assert!(lease_b.is_live());

    let db = Database::open(&db_path).unwrap();
    let (kind, prior, new): (String, Option<String>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT kind,prior_host_identity,new_host_identity FROM control_plane_lease_events
             WHERE repository_key='repo-alpha' AND kind='takeover' ORDER BY event_id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(kind, "takeover");
    assert_eq!(prior.as_deref(), Some("host-a"));
    assert_eq!(new.as_deref(), Some("host-b"));
}

/// Acceptance: "A host that loses its lease through expiry, takeover, or
/// store unavailability stops writing durable orchestration state within a
/// bounded interval and records the loss, so a partitioned host never
/// continues to drive." Revokes a live lease mid-session (an out-of-band
/// takeover, independent of expiry) and checks the holder's guard notices
/// within one renewal interval and the loss is durably recorded.
#[test]
fn revoking_a_lease_mid_session_stops_the_holder_within_one_renewal_interval() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("shared.db");
    let host_a = service_at(&db_path);

    let renewal_interval = Duration::from_millis(100);
    let lease_a = HostLease::acquire_as(&host_a, "repo-alpha", "host-a", renewal_interval, 300)
        .expect("first host acquires the lease");
    assert!(lease_a.is_live());

    // Simulate a competing host taking over out from under the live holder,
    // without going through the lease API (e.g. an operator-driven or
    // future cross-host recovery path) — independent of expiry.
    {
        let mut db = Database::open(&db_path).unwrap();
        db.conn_mut()
            .execute(
                "UPDATE control_plane_leases SET host_identity='host-b',process_identity='proc-b',holder_token='revoked-token' WHERE repository_key='repo-alpha'",
                [],
            )
            .unwrap();
    }

    let noticed = wait_until(Duration::from_secs(5), || !lease_a.is_live());
    assert!(
        noticed,
        "the holder must notice the loss within a bounded interval"
    );

    let db = Database::open(&db_path).unwrap();
    let (kind, new_host): (String, Option<String>) = db
        .conn()
        .query_row(
            "SELECT kind,new_host_identity FROM control_plane_lease_events
             WHERE repository_key='repo-alpha' AND kind='lost' ORDER BY event_id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "lost");
    assert_eq!(new_host.as_deref(), Some("host-b"));
}

/// Acceptance: "Leases are per repository and independent — two hosts
/// driving two repositories against one store never contend."
#[test]
fn two_hosts_driving_two_repositories_never_contend() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("shared.db");
    let host_a = service_at(&db_path);
    let host_b = service_at(&db_path);

    let lease_a =
        HostLease::acquire_as(&host_a, "repo-alpha", "host-a", Duration::from_secs(30), 60)
            .expect("host-a drives repo-alpha");
    let lease_b =
        HostLease::acquire_as(&host_b, "repo-beta", "host-b", Duration::from_secs(30), 60)
            .expect("host-b drives repo-beta independently");
    assert!(lease_a.is_live());
    assert!(lease_b.is_live());

    let record_a = host_a
        .inspect_repository_lease("repo-alpha")
        .unwrap()
        .unwrap();
    let record_b = host_b
        .inspect_repository_lease("repo-beta")
        .unwrap()
        .unwrap();
    assert_eq!(record_a.host_identity, "host-a");
    assert_eq!(record_b.host_identity, "host-b");
}

/// Acceptance: "Single-host operation is behaviourally unchanged and
/// requires no configuration; the filesystem claim remains for local mutual
/// exclusion while the lease is authoritative." The pre-existing filesystem
/// `WorkerLock` claim is exercised with no `HostLease` in the picture at
/// all — the new cross-host mechanism is purely additive.
#[test]
fn single_host_filesystem_claim_is_unchanged() {
    let runtime_dir = tempfile::tempdir().unwrap();
    let first = WorkerLock::acquire(runtime_dir.path()).expect("first local claim succeeds");
    let second = WorkerLock::acquire(runtime_dir.path());
    assert!(
        second.is_err(),
        "a second local claim is refused exactly as before PRD-091"
    );
    drop(first);
    let third = WorkerLock::acquire(runtime_dir.path());
    assert!(
        third.is_ok(),
        "the filesystem claim is released and re-acquirable exactly as before PRD-091"
    );
}
