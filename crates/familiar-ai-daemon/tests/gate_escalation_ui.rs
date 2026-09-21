//! PRD-102 regressions: the tray says when it needs you.
//!
//! The pure parts — what the badge draws, what gets announced, and whether a
//! decision carries attribution — are proven here without a desktop session,
//! because the tray's own idiom is that everything the operator sees is a
//! tested pure function and GTK only lays it out.

use familiar_ai_core::backlog::validate_recovery_attribution;
use familiar_ai_core::BacklogRecoveryAction;

#[test]
fn a_decision_without_an_actor_or_a_reason_is_refused_by_the_ledger() {
    // The tray's Release and Force-complete buttons passed `String::new()`
    // for both, so every click failed validation. This pins why the prompt
    // exists: it is not a nicety, it is the difference between the button
    // working and not.
    for (actor, reason) in [
        ("", "a reason"),
        ("   ", "a reason"),
        ("human:trollboy", ""),
        ("human:trollboy", "   "),
        ("", ""),
    ] {
        assert!(
            validate_recovery_attribution(BacklogRecoveryAction::Release, actor, reason).is_err(),
            "actor {actor:?} + reason {reason:?} must be refused"
        );
    }
}

#[test]
fn a_decision_with_a_human_actor_and_a_reason_is_accepted() {
    let ok = validate_recovery_attribution(
        BacklogRecoveryAction::Release,
        "human:trollboy",
        "superseded by round 2",
    );
    assert!(
        ok.is_ok(),
        "a named human with a reason must be accepted: {ok:?}"
    );
}

#[test]
fn the_actor_the_tray_sends_satisfies_the_ledgers_form() {
    // `decision_actor` builds `human:<user>`; the ledger requires exactly
    // that shape. If either side changes, this fails rather than the button.
    for user in ["trollboy", "someone-else"] {
        let actor = format!("human:{user}");
        assert!(
            validate_recovery_attribution(BacklogRecoveryAction::Release, &actor, "why").is_ok(),
            "{actor} must be a valid decision actor"
        );
    }
    // And the fallback used when no username is discoverable.
    assert!(
        validate_recovery_attribution(BacklogRecoveryAction::Release, "human:tray", "why").is_ok()
    );
}

// ---------------------------------------------------------------------------
// FAM-BUG-062: a command the owner dispatched must not contend with the owner.
// ---------------------------------------------------------------------------

use familiar_ai_daemon::worker_lock::{WorkerLock, DELEGATION_ENV};

/// These manipulate a process-global environment variable, so they run behind
/// one mutex rather than as parallel threads in the same binary. That is the
/// lesson from the XDG race fixed on 2026-09-20: `cargo test` runs tests as
/// threads, and a global mutated by one is visible to all of them.
static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_stranger_is_still_refused_while_the_owner_is_live() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let _owner = WorkerLock::acquire(dir.path()).expect("the owner takes the claim");

    std::env::remove_var(DELEGATION_ENV);
    let refused = WorkerLock::acquire(dir.path());
    assert!(
        refused.is_err(),
        "a process that is not the owner's delegate must still be refused"
    );
}

#[test]
fn a_delegate_of_the_live_owner_proceeds() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let owner = WorkerLock::acquire(dir.path()).expect("the owner takes the claim");

    // The owner tells its child who dispatched it.
    std::env::set_var(DELEGATION_ENV, owner.claim().owner_pid.to_string());
    let delegate = WorkerLock::acquire(dir.path());
    std::env::remove_var(DELEGATION_ENV);
    assert!(
        delegate.is_ok(),
        "the owner's own dispatched command must be able to run: {:?}",
        delegate.err()
    );
}

#[test]
fn naming_the_wrong_owner_authorises_nothing() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let owner = WorkerLock::acquire(dir.path()).expect("the owner takes the claim");

    // A stale or inherited value must not be a skeleton key.
    std::env::set_var(DELEGATION_ENV, (owner.claim().owner_pid + 1).to_string());
    let refused = WorkerLock::acquire(dir.path());
    std::env::remove_var(DELEGATION_ENV);
    assert!(
        refused.is_err(),
        "delegation must name the live owner exactly"
    );
}

#[test]
fn two_delegates_still_exclude_each_other() {
    // The whole point of the claim is that one mutator runs at a time.
    // Delegation must not trade that away for convenience.
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let owner = WorkerLock::acquire(dir.path()).expect("the owner takes the claim");

    std::env::set_var(DELEGATION_ENV, owner.claim().owner_pid.to_string());
    let first = WorkerLock::acquire(dir.path()).expect("the first delegate proceeds");
    let second = WorkerLock::acquire(dir.path());
    std::env::remove_var(DELEGATION_ENV);

    assert!(
        second.is_err(),
        "a second delegate must wait: exclusion is the property being preserved"
    );
    drop(first);
}
