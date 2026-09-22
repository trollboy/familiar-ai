//! PRD-109: one PRD lifecycle, derived and shown everywhere.
//!
//! A PRD's state used to be spelled in two vocabularies that were never
//! reconciled: the file's front matter (`draft`, `ready`, `in_progress`,
//! `completed`, `blocked`, written by a human) and the backlog ledger
//! (`pending`, `in_progress`, `completed`, `blocked`, written by Familiar,
//! per host), with attempt outcomes and checkpoint phases underneath both.
//! This module derives a single ten-state [`PrdLifecycle`] from those
//! records. Nothing here is stored; the inputs already exist, and the
//! derivation is the only place the ten names are spelled.
//!
//! Human-owned states (Draft, Ready, Blocked) come from the file, because
//! the file is the one record every host shares. Machine-owned states come
//! from the ledger, the latest attempt, the checkpoint and the pending human
//! gates on the host answering. See `docs/contracts/prd-lifecycle.md`.

use std::fmt;

/// The single lifecycle vocabulary. Order is the ordinary forward flow;
/// `Blocked`, `Failed` and `AwaitingFeedback` are the side states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrdLifecycle {
    Draft,
    Ready,
    Implementing,
    Testing,
    Reviewed,
    Approved,
    Completed,
    Blocked,
    Failed,
    AwaitingFeedback,
}

impl PrdLifecycle {
    pub const ALL: [PrdLifecycle; 10] = [
        Self::Draft,
        Self::Ready,
        Self::Implementing,
        Self::Testing,
        Self::Reviewed,
        Self::Approved,
        Self::Completed,
        Self::Blocked,
        Self::Failed,
        Self::AwaitingFeedback,
    ];

    /// The serialized spelling, identical to the serde form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Implementing => "implementing",
            Self::Testing => "testing",
            Self::Reviewed => "reviewed",
            Self::Approved => "approved",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::AwaitingFeedback => "awaiting_feedback",
        }
    }

    /// Human-readable label for operator surfaces.
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Ready => "Ready",
            Self::Implementing => "Implementing",
            Self::Testing => "Testing",
            Self::Reviewed => "Reviewed",
            Self::Approved => "Approved",
            Self::Completed => "Completed",
            Self::Blocked => "Blocked",
            Self::Failed => "Failed",
            Self::AwaitingFeedback => "Awaiting feedback",
        }
    }

    /// True for the states a human sets in the PRD file itself.
    pub fn is_human_owned(self) -> bool {
        matches!(self, Self::Draft | Self::Ready | Self::Blocked)
    }

    /// True when the PRD is waiting on a decision only a human can make.
    /// Under the 1.0 rule this is the only legitimate stop; everything in
    /// `Failed` is a defect.
    pub fn needs_human(self) -> bool {
        matches!(self, Self::AwaitingFeedback | Self::Blocked)
    }
}

impl fmt::Display for PrdLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Retained reasons that mean "a human has to decide", and nothing else.
/// A retained reason outside this list is `Failed`. Keep this closed: adding
/// a reason here is a claim that the stop is legitimately the owner's, which
/// is the claim the firing table exists to test.
pub const HUMAN_GATE_REASONS: &[&str] = &[
    "scope_ambiguous",
    "scope_broadened",
    "human_review_required",
];

/// True when a retained reason (or its class token before the first `:`) is
/// one a human must answer rather than a defect to fix.
pub fn is_human_gate_reason(reason: &str) -> bool {
    let class = reason.split(':').next().unwrap_or(reason).trim();
    HUMAN_GATE_REASONS.contains(&class)
}

/// The latest attempt's durable facts, as far as the lifecycle needs them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptFacts {
    /// `completed`, `retained`, or `None` while the attempt is still running.
    pub outcome: Option<String>,
    pub retained_reason: Option<String>,
    /// `preflight`, `implemented`, `verification`, `review_complete`,
    /// `integrated`, … as the driver records it.
    pub last_durable_phase: Option<String>,
}

impl AttemptFacts {
    fn is_running(&self) -> bool {
        self.outcome.is_none()
    }
    fn is_retained(&self) -> bool {
        self.outcome.as_deref() == Some("retained")
    }
    fn reached_verification(&self) -> bool {
        matches!(
            self.last_durable_phase.as_deref(),
            Some("verification") | Some("verified") | Some("review") | Some("review_complete")
        )
    }
}

/// Everything the derivation reads. Every field is optional because every
/// record it comes from can be absent; the table in
/// `docs/contracts/prd-lifecycle.md` says what wins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleInputs {
    /// Front-matter `status`, if the file declares one.
    pub file_status: Option<String>,
    /// True when the file lives under the archive (`done/`). Location is
    /// truth for completion (PRD-023).
    pub archived: bool,
    /// The reconciled backlog row's status on this host, if any.
    pub ledger_status: Option<String>,
    /// The most recent driver attempt for this PRD on this host, if any.
    pub latest_attempt: Option<AttemptFacts>,
    /// The durable checkpoint's phase, if one exists.
    pub checkpoint_phase: Option<String>,
    /// True when a scope decision, human review, or risk acceptance is
    /// pending for this PRD on this host.
    pub pending_human_gate: bool,
}

/// One derived state, plus a diagnostic when the inputs disagree in a way
/// the reader should see.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DerivedLifecycle {
    pub lifecycle: PrdLifecycle,
    /// Set when two authoritative records disagree, for example a ledger
    /// row saying `completed` while the file is still active.
    pub divergence: Option<String>,
}

/// Derive the lifecycle. First matching row of the contract's table wins.
pub fn derive(inputs: &LifecycleInputs) -> DerivedLifecycle {
    let file = inputs.file_status.as_deref();
    let ledger = inputs.ledger_status.as_deref();
    let checkpoint = inputs.checkpoint_phase.as_deref();
    let attempt = inputs.latest_attempt.as_ref();

    if inputs.archived || ledger == Some("completed") || checkpoint == Some("completed") {
        let divergence = (!inputs.archived
            && (ledger == Some("completed") || checkpoint == Some("completed")))
        .then(|| "ledger says completed but the file is still in the active directory".to_string());
        return DerivedLifecycle {
            lifecycle: PrdLifecycle::Completed,
            divergence,
        };
    }
    if file == Some("blocked") || ledger == Some("blocked") {
        return plain(PrdLifecycle::Blocked);
    }
    if file == Some("draft") {
        return plain(PrdLifecycle::Draft);
    }
    if checkpoint == Some("approved") {
        return plain(PrdLifecycle::Approved);
    }
    if inputs.pending_human_gate {
        return plain(PrdLifecycle::AwaitingFeedback);
    }
    if checkpoint == Some("reviewed") {
        return plain(PrdLifecycle::Reviewed);
    }
    if let Some(attempt) = attempt {
        if attempt.is_retained() {
            let reason = attempt.retained_reason.as_deref().unwrap_or("");
            return plain(if is_human_gate_reason(reason) {
                PrdLifecycle::AwaitingFeedback
            } else {
                PrdLifecycle::Failed
            });
        }
        if attempt.is_running() {
            return plain(if attempt.reached_verification() {
                PrdLifecycle::Testing
            } else {
                PrdLifecycle::Implementing
            });
        }
    }
    if ledger == Some("in_progress") || file == Some("in_progress") {
        return plain(PrdLifecycle::Implementing);
    }
    plain(PrdLifecycle::Ready)
}

fn plain(lifecycle: PrdLifecycle) -> DerivedLifecycle {
    DerivedLifecycle {
        lifecycle,
        divergence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(outcome: Option<&str>, reason: Option<&str>, phase: Option<&str>) -> AttemptFacts {
        AttemptFacts {
            outcome: outcome.map(str::to_owned),
            retained_reason: reason.map(str::to_owned),
            last_durable_phase: phase.map(str::to_owned),
        }
    }

    /// One row per line of the contract's derivation table, first match
    /// wins, in table order.
    #[test]
    fn every_row_of_the_derivation_table_holds() {
        let rows: Vec<(&str, LifecycleInputs, PrdLifecycle)> = vec![
            (
                "archived file",
                LifecycleInputs {
                    archived: true,
                    ..Default::default()
                },
                PrdLifecycle::Completed,
            ),
            (
                "ledger completed",
                LifecycleInputs {
                    ledger_status: Some("completed".into()),
                    file_status: Some("ready".into()),
                    ..Default::default()
                },
                PrdLifecycle::Completed,
            ),
            (
                "file blocked beats a running attempt",
                LifecycleInputs {
                    file_status: Some("blocked".into()),
                    latest_attempt: Some(attempt(None, None, Some("implemented"))),
                    ..Default::default()
                },
                PrdLifecycle::Blocked,
            ),
            (
                "file draft with a pending ledger row",
                LifecycleInputs {
                    file_status: Some("draft".into()),
                    ledger_status: Some("pending".into()),
                    ..Default::default()
                },
                PrdLifecycle::Draft,
            ),
            (
                "checkpoint approved",
                LifecycleInputs {
                    checkpoint_phase: Some("approved".into()),
                    ledger_status: Some("in_progress".into()),
                    ..Default::default()
                },
                PrdLifecycle::Approved,
            ),
            (
                "pending human gate",
                LifecycleInputs {
                    pending_human_gate: true,
                    ledger_status: Some("in_progress".into()),
                    ..Default::default()
                },
                PrdLifecycle::AwaitingFeedback,
            ),
            (
                "checkpoint reviewed, nothing pending",
                LifecycleInputs {
                    checkpoint_phase: Some("reviewed".into()),
                    ..Default::default()
                },
                PrdLifecycle::Reviewed,
            ),
            (
                "retained on a human gate reason",
                LifecycleInputs {
                    latest_attempt: Some(attempt(Some("retained"), Some("scope_ambiguous"), None)),
                    ..Default::default()
                },
                PrdLifecycle::AwaitingFeedback,
            ),
            (
                "retained on a defect reason",
                LifecycleInputs {
                    latest_attempt: Some(attempt(
                        Some("retained"),
                        Some("verification_failed"),
                        None,
                    )),
                    ..Default::default()
                },
                PrdLifecycle::Failed,
            ),
            (
                "retained with a prefixed reason",
                LifecycleInputs {
                    latest_attempt: Some(attempt(
                        Some("retained"),
                        Some("review_failed: configuration failed"),
                        None,
                    )),
                    ..Default::default()
                },
                PrdLifecycle::Failed,
            ),
            (
                "running, past verification",
                LifecycleInputs {
                    latest_attempt: Some(attempt(None, None, Some("review_complete"))),
                    ..Default::default()
                },
                PrdLifecycle::Testing,
            ),
            (
                "running, still implementing",
                LifecycleInputs {
                    latest_attempt: Some(attempt(None, None, Some("preflight"))),
                    ..Default::default()
                },
                PrdLifecycle::Implementing,
            ),
            (
                "ledger in_progress with no attempt row",
                LifecycleInputs {
                    ledger_status: Some("in_progress".into()),
                    ..Default::default()
                },
                PrdLifecycle::Implementing,
            ),
            (
                "file in_progress on another host, pending here",
                LifecycleInputs {
                    file_status: Some("in_progress".into()),
                    ledger_status: Some("pending".into()),
                    ..Default::default()
                },
                PrdLifecycle::Implementing,
            ),
            (
                "ready file, pending row",
                LifecycleInputs {
                    file_status: Some("ready".into()),
                    ledger_status: Some("pending".into()),
                    ..Default::default()
                },
                PrdLifecycle::Ready,
            ),
            (
                "nothing known at all",
                LifecycleInputs::default(),
                PrdLifecycle::Ready,
            ),
        ];
        for (name, inputs, expected) in rows {
            assert_eq!(derive(&inputs).lifecycle, expected, "row: {name}");
        }
    }

    #[test]
    fn a_completed_ledger_row_under_an_active_file_reports_divergence() {
        let derived = derive(&LifecycleInputs {
            ledger_status: Some("completed".into()),
            file_status: Some("ready".into()),
            ..Default::default()
        });
        assert_eq!(derived.lifecycle, PrdLifecycle::Completed);
        assert!(derived.divergence.is_some());
        assert!(derive(&LifecycleInputs {
            archived: true,
            ..Default::default()
        })
        .divergence
        .is_none());
    }

    #[test]
    fn the_vocabulary_is_closed_and_round_trips() {
        assert_eq!(PrdLifecycle::ALL.len(), 10);
        for state in PrdLifecycle::ALL {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{}\"", state.as_str()));
            let back: PrdLifecycle = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state);
        }
        assert!(PrdLifecycle::AwaitingFeedback.needs_human());
        assert!(!PrdLifecycle::Failed.needs_human());
        assert!(PrdLifecycle::Ready.is_human_owned());
        assert!(!PrdLifecycle::Testing.is_human_owned());
    }

    #[test]
    fn human_gate_reasons_are_exactly_the_owner_decisions() {
        for reason in [
            "scope_ambiguous",
            "scope_broadened",
            "human_review_required",
        ] {
            assert!(is_human_gate_reason(reason), "{reason}");
        }
        for reason in [
            "verification_failed",
            "malformed_output",
            "checkpoint_failed",
            "interrupted",
            "unclassified_result",
            "integration_failed",
            "review_failed: enabled review requires an explicit PRD Acceptance Criteria section",
        ] {
            assert!(!is_human_gate_reason(reason), "{reason}");
        }
    }
}
