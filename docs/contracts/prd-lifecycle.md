# PRD lifecycle

**Defined by:** PRD-109. **Code:** `familiar_ai_core::lifecycle`.

One vocabulary for the state of a PRD, derived from records that already
exist and shown identically on every operator surface. Nothing in this
contract is stored, and Familiar never writes a lifecycle state into a PRD
file.

## The ten states

Forward flow:

```
Draft → Ready → Implementing → Testing → Reviewed → Approved → Completed
```

Side states, reachable from any stage before Completed:

- **AwaitingFeedback** — a decision only a human can make is pending. Under
  the 1.0 rule this is the only legitimate stop.
- **Failed** — the latest attempt was retained for a reason that is not a
  human decision. Every Failed is a defect to fix, never a state to wait in.
- **Blocked** — a human has said do not run this. Set in the file.

**Completed has no transition out.** Unmet work becomes a successor PRD
(`completion-is-immutable.md`).

## Who owns which state

| State | Owner | Record |
|---|---|---|
| Draft, Ready, Blocked | human | front-matter `status` in the PRD file |
| Implementing, Testing, Reviewed, Approved, Failed, AwaitingFeedback | Familiar | ledger row, latest attempt, checkpoint, pending gates on the answering host |
| Completed | location | file under `done/`, or the ledger row |

The file is the one record every host shares, so the human-owned states win
over a stale ledger row. Machine-owned states are per host; where a row says
`in_progress` on a host that is not running it, the file's state wins
(FAM-BUG-074).

## Derivation

First matching row wins. Serialized spellings are the lowercase snake-case
of the state names (`awaiting_feedback`).

| Inputs | Lifecycle |
|---|---|
| file archived, or ledger `completed`, or checkpoint `completed` | Completed (with a divergence note when the file is still active) |
| file `blocked` or ledger `blocked` | Blocked |
| file `draft` | Draft |
| checkpoint `approved` | Approved |
| a pending scope decision, human review, or risk acceptance for this PRD | AwaitingFeedback |
| checkpoint `reviewed` | Reviewed |
| latest attempt retained, reason in `HUMAN_GATE_REASONS` | AwaitingFeedback |
| latest attempt retained, any other reason | Failed |
| latest attempt running, phase at or past verification | Testing |
| latest attempt running, earlier phase | Implementing |
| ledger or file `in_progress`, no attempt row | Implementing |
| otherwise | Ready |

`HUMAN_GATE_REASONS` is closed: `scope_ambiguous`, `scope_broadened`,
`human_review_required`. A retained reason's class is the token before its
first `:`. Adding a reason to that list is a claim that the stop is
legitimately the owner's; the firing table exists to test exactly that
claim.

## Where it appears

- `stewardship backlog` items: `lifecycle` and, when set, `lifecycle_divergence`,
  beside the raw `status`.
- The operator snapshot the Tauri desktop and the GTK view consume: the
  same two fields on backlog rows and dependency nodes.
- `familiar-ai next`: a fifth column.
- The session report: beside each PRD's outcome.

The raw inputs stay visible everywhere the lifecycle is shown, so an auditor
can see why a state was derived.
