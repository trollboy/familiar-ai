# Familiar Security Threat Model

This threat model is the durable index for the PRD-037 burn-in suite. Familiar
treats repository and PRD content, agent output, verification output, delivery
provider responses, persisted journals, environment variables, and operator
input as hostile. A process exit, reboot, unavailable filesystem or database,
truncated journal, rate limit, network partition, or ambiguous external result
is likewise hostile state.

## Trust boundaries

| Boundary | Untrusted input | Required closed outcome |
|---|---|---|
| PRD admission | Markdown and Expected Files paths | Reject ambiguous, absolute, expanded, or traversing paths |
| Repository | Paths, symlinks, Git state, worktree ownership | Contain access; reject escape or unproved state |
| Agent | Prompt content, argv-shaped text, event stream, process lifetime | Pass prompts on stdin; reject malformed or unterminated streams; retain authenticated process outcome |
| Verification and review | Commands, evidence, findings, reviewer identity | Use literal argv; persist pending/failed state before retry; never infer approval |
| Storage and journals | Partial transaction, corruption, stale phase | Atomic commit; surface corruption; make identical retries no-ops |
| Delivery | Provider errors and ambiguous external effects | Journal intent/result; look up an existing effect before retry; never infer delivery |
| Credentials and observability | Environment, stderr, reports, comments | Do not copy credential values; redact suspected secrets; report only presence or failure class |
| Supervisor and overrides | Repeated crashes, forged labels/paths, non-human assertions | Finite warrants and restart limits; durable logs; explicit human authority remains distinct |

## Invariants

Completion, review, approval, integration, and delivery are positive claims and
require their own durable evidence. Absence, parse failure, I/O failure, process
death, timeout, or an unknown phase cannot create that evidence. Completed
phases and external effects have stable identities, so replay is observable and
idempotent. Unknown or corrupt state is retained as blocked/invalid and remains
reportable to a human.

Secrets are data, never authority. Familiar does not intentionally place host
credentials in prompts or persisted evidence. Tests use a canary value and scan
all captured output and durable rows; the canary itself must never be printed by
a failing assertion or diagnostic.

## Burn-in operation

The executable matrix is [coverage-matrix.md](coverage-matrix.md). Every row
names a stable test. CI should run the four `security_burn_in` test targets on
every change to agent execution, review, recovery, delivery, storage, logging,
or supervisor behavior. A row may be added when a phase or threat boundary is
added; removing a row requires replacement coverage for the same phase and
fault class.
