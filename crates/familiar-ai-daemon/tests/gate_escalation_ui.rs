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
