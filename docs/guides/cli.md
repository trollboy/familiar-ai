# Familiar CLI reference

## Commands

Bare `familiar-ai` -- no arguments -- reports the repository's current state
and the single next runnable command: nothing to do, work eligible (`run
<path>`), a decision pending (`approve ...`), or a session that stopped and
needs a look (`report <session>`). A pending scope decision always wins, so
the fix for a stopped attempt is never buried under `backlog release` or
`backlog complete`.

### Daily verbs

| Command | Purpose |
|---|---|
| `next` | Select the next eligible PRD without executing it |
| `run <prd>` | Execute one PRD with the configured agent |
| `drive` | Execute eligible PRDs unattended until the backlog is empty, nothing is eligible, or the budget warrant is exhausted |
| `resume <prd>` | Continue one durable partial, or inspect/schedule all of them |
| `report [session]` | One screen: what got built, what stopped and why, what it cost, what needs `approve` |
| `approve` | Decide one pending scope finding, by ordinal on a terminal or by flags |
| `deliver <ownership-record>` | Publish, check, merge, deploy, and smoke-test one reviewed worktree |

`drive` **requires** a finite warrant (PRD count, cost, or duration) and refuses
to start without one. Command-line flags may only *tighten* the configured
warrant, never loosen it.

### Administrative namespaces

Everything else lives under one of five declared namespaces, so the top level
stays limited to the seven verbs above:

| Namespace | Holds |
|---|---|
| `config` | Providers, models, artifacts, `config compress`, `config model-residency` |
| `accounting` | `month-to-date`, `prd-cost`, `accounting billing`, `accounting usage` |
| `stewardship` | Read-only queries: backlog, sessions, attempts, checkpoints, recovery, delivery, budget, review, gates, reconciliation, workers, `stewardship status`, `stewardship preflight`, `stewardship history` |
| `plan` | Proposal batches, `plan onboard`, `plan backlog`, `plan batch-review` |
| `ops` | `ops control`, `ops worker`, `ops operator`, `ops gate`, `ops waive` |

Every command that used to live at the top level -- `billing`, `compress`,
`model-residency`, `status`, `preflight`, `history`, `usage`, `onboard`,
`backlog`, `batch-review`, `control`, `worker`, `operator`, `gate`, `waive`,
and `scope-decisions` (now `approve`) -- keeps working exactly as before; it
prints a note naming the form above the first time you use it.

### Operator repairs

Three repairs that used to require the source tree and `cargo run --example`
are shipped commands, under `ops operator`. Each writes durable orchestration
state or consults the scheduler, so each demands an explicit human actor and a
reason, and each refuses while the control-plane claim is live rather than
racing a running driver.

| Command | Repairs |
|---|---|
| `ops operator rebind` | `resume` refuses a candidate because a surgical edit moved its content away from the recorded hash |
| `ops operator set-phase` | A checkpoint is stuck in a phase the pipeline will not advance |
| `ops operator width` | An authored wave width disagrees with what the scheduler will actually admit |
| `stewardship substance` | Which approval substance hashes this repository has durably approved |

```sh
# Rebind a checkpoint to its worktree's current content after a repair.
familiar-ai ops operator rebind PRD-63 \
  --actor human:you --reason "fixed a dangling doc reference blocking resume"

# Move a stuck checkpoint through the audited transition API.
familiar-ai ops operator set-phase <checkpoint-id> implemented \
  --actor human:you --reason "verification passed out of band"

# Ask the scheduler what it will actually admit, for the whole backlog or a subset.
familiar-ai ops operator width PRD-63 PRD-72 \
  --actor human:you --reason "sizing the next wave"

# Read-only: the approval substance this repository has approved.
familiar-ai stewardship substance
```

Both hashes, the actor and the reason are recorded on the checkpoint's own
event trail, so a rebind is auditable rather than a silent overwrite.

---
