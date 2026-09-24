# Familiar Running Bugs and Friction

This is the live operator-facing defect and friction log. Entries remain open
until a verified fix is committed; a fix records its evidence and disposition
instead of deleting the history.

**Every bug gets an entry of the form `### FAM-BUG-NNN — title` followed by a
`- **Status:**` line whose first word is `Open`, `Fixed`, `Closed` or
`Reopened`.** Recording a bug as a bullet inside a dated narrative hides it
from any count that follows this convention: on 2026-09-21 six of sixty-two
entries were off-format, and "how many are open" had a different answer
depending on how it was asked. `bug_log_contract.rs` fails the build if an id
appears with no entry, or with a status a reader cannot classify.

## 2026-09-22 — queue audit and the PR #19 landing

### FAM-BUG-070 — Raw implementations were recorded twice in the usage ledger

- **Status:** Fixed 2026-09-22 on PR #19's branch (`703f68c`), merged to `main` in `e16fc73`.
- **Found:** by the independent review of PR #19 (PRD-100).
- **Detail:** `execute_tracked_inner` called `persist_accounting_observations`
  for the implementation stage unconditionally, appending an aggregate row
  from `ExecutionResult`'s totals, while `SqliteRawAgentHost::finish` had
  already persisted one PRD-051 observation per attempt through
  `persist_run_outcome`. The two rows carried different adapter labels and
  different `source_event_hash` values, so `append_observation`'s hash
  dedup could not see them as one. Every owned-loop implementation counted
  roughly twice — in the ledger that PRD-100's edit-success number and
  PRD-086's cost basis are meant to read from.
- **Fix:** the harness row is skipped when the implementation adapter is
  `RawAgentLoop` or `Ollama`, whose host writes the per-attempt rows.
  Pinned by `only_owned_loop_workers_have_host_persisted_usage`. An
  integration regression through `execute_with_config_tracked` that counts
  observations per attempt is owed and carried in PRD-105.

### FAM-BUG-071 — The raw agent claimed budget enforcement it did not perform

- **Status:** Fixed 2026-09-22 on PR #19's branch (`703f68c`), merged to `main` in `e16fc73`.
- **Found:** by the independent review of PR #19 (PRD-100).
- **Detail:** `RawAgent::budget_capability` returned `cost: true, tokens:
  true, duration: true`. `execute` read the cost ceiling only to size a
  PRD-064 reservation and read the request timeout; `budget.max_tokens`
  and `budget.max_duration_ms` were never consulted, and
  `StopReason::BudgetStop` has no producer in the loop. The
  `UnenforceableBudget` gate in `run.rs` trusts that declaration, so a
  per-execution token, duration or cost warrant on a raw worker was
  accepted as enforceable and enforced by nothing. That is the silent-spend
  shape Core Principle #11 exists to prevent.
- **Fix:** the warrant's token ceiling folds into the loop's
  `max_output_tokens` and its duration ceiling into the wall-clock ceiling,
  so the loop stops on both; cost is declared unenforced, so a per-execution
  cost ceiling on a raw worker is now refused by name rather than silently
  ignored. Pinned by `a_token_budget_stops_the_loop_at_the_ceiling`. Cost
  enforcement inside the loop waits on a price basis (PRD-086).

### FAM-BUG-072 — The attempts ledger mixes another repository's fixture rows into this project's numbers

- **Status:** Open — narrowed 2026-09-22. Every shipped reader is already
  repository-scoped: `attempts(session_id)`, `latest_attempt_for_prd`, the
  rounds query and PRD-085's autonomy queries all join `driver_sessions`
  on `repository_key`. The unscoped reads were hand-written SQL in three
  documents. What remains is that no shipped command answers the
  retained-reason histogram, so a human opens sqlite and gets it wrong;
  that command is PRD-098's scope and this entry stays open until it
  ships.
- **Found:** 2026-09-22, auditing the queue against the ledger.
- **Detail:** 13 of the 43 rows in `driver_attempts` belong to sessions
  whose `repository_key` is `~/Projects/spectra` (ids `PRD-177a`,
  `PRD 0177f` and so on, dated 2026-08-09 and 08-19). Every status
  document since 2026-09-16 — the README correction, FAM-BUG-019's
  reopening, PRD-098, EXECUTION-PLAN's round table — quoted the unfiltered
  table. The two largest rows of the histogram they cite, "no Acceptance
  Criteria section" ×6 and "no reason recorded" ×6, are all spectra rows,
  and PRD-085 and PRD-093 were sequenced into round 1 on them. On this
  repository the figures are 30 attempts, 2 completed, 2 `integrated_at`.
- **Expected fix:** every ledger query that produces a number a human
  reads filters by `driver_sessions.repository_key` (the stall taxonomy
  query, the session rollup, `stewardship`, and whatever PRD-098 ships),
  and the report names the repository scope beside the host scope.
  PRD-098's first criterion now requires it.

### FAM-BUG-073 — The resume-landing path integrates without recording the integration

- **Status:** Fixed 2026-09-22 — `DriverRepository::mark_latest_attempt_integrated`
  finds the latest unintegrated attempt for the PRD in this repository and
  stamps `integrated_at`, `candidate_revision` and phase `integrated`;
  `complete_landed` in `resume.rs` calls it right after
  `approve_and_complete`, so a resumed landing now leaves the same row the
  merge queue leaves. Pinned by
  `marking_the_latest_attempt_integrated_is_scoped_and_idempotent`: scoped
  to the repository, exactly once, no-op when nothing is waiting. The
  historical rows for PRD-60, 76, 85 and 96 are not backfilled; the ledger
  keeps the undercount as a fact about the past.
- **Found:** 2026-09-22, reconciling `main` against `driver_attempts`.
- **Detail:** `main` carries five `familiar: integrate reviewed candidate`
  commits; `driver_attempts.integrated_at` has two rows (PRD-53, PRD-81).
  The other three (PRD-60, PRD-76 on 09-01; PRD-85 and PRD-96 on 09-19,
  each preceded by a `PRD-NN: resumed candidate` commit) landed through the
  resume path, which never marks the attempt. PRD-81's recorded candidate
  SHA no longer exists in the repository. The one column the project uses
  as its integration record undercounts the project's own successes.
- **Expected fix:** the resume-landing path writes `integrated_at` and the
  landed revision on the attempt it resumed, in the same transaction as
  `approve_and_complete`. Pinned by a regression driving resume to landing
  and asserting the row. PRD-098's second criterion is satisfied by this
  fix's regression if it lands first.

### FAM-BUG-074 — A PRD claimed on another host is pending and eligible here

- **Status:** Fixed 2026-09-22 — `front_matter_hold` in `backlog.rs` names
  `draft`, `in_progress` and `blocked` as holds, and all three selection
  surfaces consult it: `next` reports
  `front matter status <s>`, `run` admission refuses with `RunStatus`, and
  the drive's batch selection records a durable `front_matter_hold`
  decision and moves on. `ready` and an absent status stay selectable;
  `completed` is left to location. Pinned by
  `front_matter_status_holds_a_prd_out_of_admission`. This also delivers
  the `draft` half of PRD-093's seventh criterion ahead of that PRD.
- **Found:** 2026-09-22, answering whether PRD-104 could be picked up by
  the Linux driver while the macOS session implements it.
- **Detail:** backlog discovery inserts a newly seen PRD file as `pending`
  and never consults the front matter `status`; only `blocked` is an
  ineligibility reason, and only from the row's own status. Each host has
  its own store, so a PRD marked `in_progress` by the other machine is
  `pending` here, and with no dependencies and no scope conflict it is
  eligible for the next drive. PRD-104 — 14 criteria, 40 expected files,
  in progress on macOS by hand — is exactly that shape.
- **Expected fix:** front-matter `status` participates in eligibility:
  `draft` and `in_progress` are ineligibility reasons alongside `blocked`
  (PRD-093's seventh criterion covers `draft`; this entry adds
  `in_progress`), or the multi-host lease from PRD-091 is consulted at
  selection. Until then, drives on this host should name their PRDs
  explicitly.

### FAM-BUG-075 — A terminal cannot submit a drive to the resident daemon

- **Status:** Open
- **Found:** 2026-09-22, launching PRD-103 hands-off through the designed
  path while the daemon held the control-plane claim.
- **Detail:** `familiar-ai ops control submit <project> -- familiar-ai
  drive --prd PRD-103 ...` is refused by the daemon with `authority denied:
  a valid minted session is required`. `cli/control.rs` mints an Operator
  session only on the in-process path it takes when no daemon is resident;
  against a live daemon it calls the socket with no credential. The Linux
  tray submits in-process with a scope it builds itself, so the only
  surface that can dispatch a drive while the daemon runs is a button. A
  terminal `drive` or `resume` is refused by the worker lock (FAM-BUG-062's
  delegate fix covers children of the daemon, not a shell), so the
  documented workaround is to stop the daemon, run the drive, and restart
  it — which takes the tray down for the run and is what the owner's
  directive that the tray be live forbids.
- **Expected fix:** the CLI mints an Operator session against the resident
  daemon the same way it does in-process — same-user peer identity is the
  precondition PRD-056 already checks — and presents it on `submit`,
  `attach` and `show`; a regression drives `submit` against a live daemon
  fixture and asserts the execution is accepted and runs as the owner's
  delegate.

### FAM-BUG-076 — The verification image cannot compile the workspace, so every drive dies in preflight at zero cost

- **Status:** Open — image half fixed 2026-09-22: the tester stage now installs
  `libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev`, and
  the `lint` verification check passes in the rebuilt image in 2m52s. The
  retained-output half — the capture keeping the head of stdout and
  dropping the tail where the error is — stays open.
- **Found:** 2026-09-22, the first hands-off run of PRD-103
  (session `drive-00001790079983258612-0002341699-000000`): `preflight_failed`
  on `verification.lint`, exit 101, four minutes in, `attempted=0`, $0.
- **Detail:** `crates/familiar-ai-desktop` became a workspace member with
  PRD-104, and `cargo clippy --workspace --all-targets` in the tester image
  now compiles `webkit2gtk-sys`. The image installs `pkg-config
  libgtk-3-dev libayatana-appindicator3-dev` and nothing for WebKit;
  `pkg-config --exists` inside it reports `webkit2gtk-4.1`,
  `javascriptcoregtk-4.1` and `libsoup-3.0` all missing, `gtk+-3.0`
  present. The host gate passed the same commit because the host has the
  packages. This is stop number one of the firing table, and it is
  infrastructure: no PRD, no model, no candidate was involved.
- **Also:** the retained failure output is the wrong end. The session's
  `termination_detail` is 53,930 bytes of Docker layer progress and the
  first minutes of `Compiling` lines, and the actual `error:` never
  appears — the capture keeps the head of stdout and drops the tail where
  a compiler puts its verdict. The report then inlines all of it, so
  `familiar-ai report` is 55KB and says nothing. Diagnosing this took a
  container run by hand.
- **Expected fix:** the image installs `libwebkit2gtk-4.1-dev
  libjavascriptcoregtk-4.1-dev libsoup-3.0-dev` beside GTK — PRD-092's
  fifth criterion, "the verification image carries the documented minimal
  system dependencies" — and the retained preflight output keeps its tail
  (or the first `error` block) rather than its head. Until the image is
  fixed no drive on this host can pass preflight, so this outranks every
  queued PRD.

### FAM-BUG-077 — The Tauri desktop core-dumps under its systemd unit on this Linux host

- **Status:** Open — mitigated 2026-09-22 with a drop-in; the unit or the application should carry the fix.
- **Found:** 2026-09-22, relaunching after a fresh build with
  `familiar-ai ops desktop install`.
- **Detail:** `familiar-ai-desktop.service` started, WebKitGTK printed
  `Could not create GBM EGL display: EGL_NOT_INITIALIZED. Aborting...`,
  the process dumped core (SIGABRT), and systemd restart-looped it every
  ten seconds. Host: NVIDIA (`10de:1f08`), X11, `driver (null)` from
  libEGL. Launched by hand with `WEBKIT_DISABLE_DMABUF_RENDERER=1` the
  desktop stays up with libEGL warnings only. The daemon unit was fine
  throughout. With the headless daemon and a crashing desktop this host
  had no tray at all, which the standing direction forbids.
- **Mitigation applied:**
  `~/.config/systemd/user/familiar-ai-desktop.service.d/override.conf`
  sets `Environment=WEBKIT_DISABLE_DMABUF_RENDERER=1`; unit restarted.
- **Expected fix:** the generated unit sets the variable on Linux, or the
  application detects a failed EGL display and falls back to the
  non-DMA-BUF renderer before WebKit aborts; either way the Linux
  graphical smoke gate PRD-104 requires before the GTK cutover would have
  caught this, and it has not run.
- **Related:** the legacy `~/.config/autostart/familiar-ai-daemon.desktop`
  entry still exists beside the new systemd units and will start a second
  daemon at next login, the shape the macOS session recorded as
  FAM-BUG-068. Not removed here.

### FAM-BUG-078 — A new selection decision killed the session it was meant to explain

- **Status:** Fixed 2026-09-22 — migration 071 widens the CHECK constraint on
  `driver_selection_decisions.decision` to include `front_matter_hold`, and
  a regression persists every decision string `drive.rs` can emit so the
  two vocabularies cannot drift apart again without a failing test.
- **Found:** 2026-09-22, the second hands-off run (PRD-108, session
  `drive-00001790081489487623-0002489786-000000`): preflight passed on the
  rebuilt image, selection reached PRD-104's front-matter hold, and the
  drive terminated `storage_failure`, `attempted=0`, $0, with `CHECK
  constraint failed: decision IN (...)`.
- **Detail:** the FAM-BUG-074 fix that morning added `front_matter_hold`
  as a durable selection decision in `drive.rs`. The decision vocabulary is
  also a CHECK constraint in the storage schema, last widened by migration
  053 for PRD-077's decisions, and nothing pinned the two together. The
  targeted tests and the full gate were green because no test drives
  selection over a held PRD against a real database. This is the author's
  own defect, found by the run that was meant to measure other stops, and
  it is stop number two of the firing table: a `storage_failure` that was
  correctly classified and correctly fatal — a session that cannot record
  its decisions should not continue.
- **Fix:** `071_selection_decision_front_matter_hold.sql` rebuilds the
  table one value wider, same shape as 052/053; `SELECTION_DECISIONS` in
  `drive.rs` enumerates every decision the driver emits, and
  `every_selection_decision_the_driver_emits_is_persistable` inserts each
  one against a migrated database.

### FAM-BUG-079 — The daemon never reconciled the backlog itself, so the dashboard and dependency Gantt could disagree with the filesystem

- **Status:** Fixed 2026-09-22 (PRD-108).
- **Found:** 2026-09-22, implementing PRD-108. `familiar-ai-core::backlog`'s
  discovery and `reconcile_and_snapshot` were only ever invoked from
  synchronous CLI paths (`next`, `run`, `drive`, `backlog`, `resume`) — never
  from the long-running daemon. The daemon's watcher updated file summaries
  and generic lifecycle rows but never touched the backlog.
- **Detail:** `DaemonDataSource::dependencies` discovered PRDs live off disk
  on every call, but joined them against `stewardship::list_backlog`'s
  ledger read, which reflected whatever a CLI invocation had last written —
  possibly nothing, possibly stale. A PRD pulled in while the daemon was
  running had no ledger row and its dependents rendered its status as
  `"not found"`, even though the file existed and could be read; the first
  dependency-Gantt implementation rendered exactly this. A PRD moved into
  `docs/prds/done/` could likewise sit unreconciled, its old path still
  looking like open work until some CLI command happened to reconcile it.
- **Fix:** a new `BacklogReconciler`
  (`familiar-ai-daemon::backlog_reconciler`) owns discovery and
  reconciliation for the daemon, invoked from three places: once for every
  configured repository at startup before the control socket or dashboard
  exist; debounced and coalesced per repository from watcher events whose
  paths fall under a repository's configured PRD locations; and a bounded,
  single-flight reconcile-on-read fallback for the operator `backlog` and
  `dependencies` queries and the HTTP dashboard's `/stewardship/backlog`.
  A reconciliation that actually changes the backlog publishes one
  `OperatorDispatcher` event so connected Tauri/GTK clients refresh through
  their existing gap/restart logic; a no-op reconciliation (in particular,
  the read fallback re-checking an already-current repository) publishes
  nothing, so polling a repository with nothing new to discover cannot turn
  into a self-sustaining refresh loop. A failed reconciliation preserves the
  prior snapshot and records a repository-scoped diagnostic. A dependency
  whose file is discovered but has no ledger row yet is now labeled
  `"unenrolled"`, never `"not found"`
  — that label is reserved for a dependency with no matching file at all.
  Pinned by `crates/familiar-ai-daemon/tests/watcher_backlog_reconciliation.rs`
  against a real temporary Git repository and a real `FileWatcher`.

### FAM-BUG-080 — The merge queue's integration commit is reachable from no branch when no delivery policy is configured

- **Status:** Fixed 2026-09-22 — `merge_candidate_archiving` builds the
  integration commit with the PRD file already moved into the configured
  archive directory (through a temporary index; no checkout is touched,
  idempotent across a retried landing), and every landing path — the merge
  queue, the escalated-candidate path, the scope-approval continuation, and
  `resume` — calls `fast_forward_checkout` once its transaction commits, so
  the checked-out branch advances onto the integration revision when that
  is a pure fast-forward and reports, never fails, when it is not. Pinned
  by `integration_archives_the_prd_and_the_checkout_fast_forwards` and by
  the FAM-BUG-019 closure test, which now asserts `HEAD` equals the
  integration revision and both PRD files are under `done/` in it. This
  occurrence (PRD-108) was resolved by hand first and is recorded as a
  FAM-BUG-019 recurrence.
- **Found:** 2026-09-22, at the end of the first hands-off run that completed:
  PRD-108, session `drive-00001790082829700506-0002649502-000000`,
  `attempted=1 completed=1`, $11.70, clean independent review on the second
  pass, `integrated_at` written, backlog `completed` — and `main` unchanged.
- **Detail:** the merge queue wrote `bba2c87` (`familiar: integrate reviewed
  candidate`, parents `1a0a086` = main and `780ec63` = the candidate) and
  recorded it as the session's `integration_revision`. No branch or tag
  points at it. This machine's configuration declares no `[delivery]`
  policy, so delivery is disabled and nothing advances any ref to the
  integration revision; the commit is one `gc` away from vanishing. Every
  one of the ledger's six merge-queue commits has needed a human to move a
  branch onto it, which is the "landed by hand" step in every after-action
  report, and the reason the exit criterion of FAM-BUG-019 has never been
  met even when the loop itself completed.
- **Resolution this time:** `git merge --ff-only bba2c87` on `main`, a pure
  fast-forward onto Familiar's own reviewed merge commit, then the ordinary
  push through the gate.
- **Expected fix:** a completed integration is never left unreferenced. With
  delivery disabled the merge queue fast-forwards the session's checked-out
  branch to the integration revision itself (the same operation performed
  by hand here), or refuses to report the PRD `completed` until some ref
  holds the commit; with a policy configured, `deliver` publishes it. The
  drive log should print the ref that now holds the work, not only the SHA.

### FAM-BUG-081 — Two hosts plan different waves for the same repository

- **Status:** Fixed 2026-09-22 — selection consults git, the one channel
  both hosts share: a drive branch `familiar/<session>/PRD-<n>` on origin
  from another session records `claimed_elsewhere`; a PRD file under the
  archive directory on origin's default branch records `archived_upstream`;
  neither is selected. `ForeignState` is computed once per selection pass
  and degrades to empty when there is no remote or no network. Migration
  072 widens the decision CHECK; `SELECTION_DECISIONS` and its regression
  cover both. Pinned by `remote_drive_branches_of_other_sessions_are_claims`.
  With FAM-BUG-080's archive-on-integration, a completion now reaches the
  other host on `git pull` as well.
- **Found:** 2026-09-22. The macOS session proposed a wave of 103, 104,
  106, 108 and 97 while this host was driving 97 and had already landed
  108. Each host plans from its own SQLite store — claims, attempts and
  completions never cross the wire — and the only shared records were the
  PRD files, whose `status` Familiar never writes and whose archive move
  was a separate human act.
- **Also fixed here, the other half of the disparity:** the desktop's
  dependency Gantt laid out dependency *layers* and its Launch-wave button
  launched a layer, while the scheduler's wave is dependency-ready AND
  scope-disjoint (the owner's definition, EXECUTION-PLAN, 2026-08-31). The
  `dependencies` query now emits each PRD's `conflicts_with` from
  `achievable_width`'s own conflict edges and its front-matter `hold`, and
  `build_dependency_gantt` assigns rounds that respect both, so two PRDs the
  scheduler would serialize are never drawn side by side. Pinned by
  `conflicting_prds_never_share_a_wave`.
- **Still open, by design:** there is no shared authority for claims and
  completions across hosts. PRD-056's control plane and PRD-091's leases
  are per host. Until one exists, git carries claims (branches) and
  completions (archive moves), and a host should pull before it plans.

### FAM-BUG-082 — `human_review_required` cannot tell "a human should decide" from "the review machinery failed"

- **Status:** Open — this occurrence (PRD-97) landed by hand after a human review, recorded as a FAM-BUG-019 recurrence.
- **Found:** 2026-09-22, the fourth hands-off run (PRD-97, session
  `drive-00001790092661105835-0003486999-000000`). Implementation and every
  required verification check passed. The independent reviewer then
  produced the same finding, `gitlab-bang-id-fed-back-to-argv`, on all
  three attempts, each rejected by `ReviewValidationError::InvalidEvidence`
  because the finding lacked its category's minimum evidence; the retry
  limit tripped and the attempt was retained `human_review_required` at
  $5.72.
- **Detail:** two different things now share one retained reason. A
  reviewer that *wants* a human (a genuine judgment call) and a reviewer
  that *could not produce a valid review* (a defect in the reviewer, the
  validation rule, or the prompt) both end as `human_review_required`, and
  PRD-109's lifecycle therefore shows both as AwaitingFeedback. The owner
  was asked to decide something that was not a decision: the finding was
  wrong about the code, which stores GitLab's bare IID as the id and the
  `!123` form only as display, and the candidate's own test
  `gitlab_consuming_verbs_use_the_bare_parsed_id_not_the_bang_display`
  already pinned it. Three validation rejections of the same finding is
  also a retry that could never succeed, the FAM-BUG-054 shape one stage
  later: nothing changed between attempts.
- **Resolution this time:** human review of the candidate; landed as
  `bca551e` with the PRD archived in the integration commit.
- **Expected fix:** a cycle that stops because every review attempt failed
  validation retains as a distinct class (`review_failed`, which the stall
  taxonomy already names) rather than `human_review_required`, so the
  lifecycle reads Failed and the firing table counts it as a defect; and a
  second identical validation rejection reroutes to a different reviewer or
  stops immediately rather than spending the third attempt on the same
  model repeating itself.

### FAM-BUG-083 — Releasing a PRD does not retire the stops that put it on "Waiting on you"

- **Status:** Fixed 2026-09-22 — `pending_human_gates` excludes a stopped
  attempt or blocked checkpoint that a human recovery event (release,
  manual completion, recorded completion, approval) has answered since; the
  event's `changed_at` after the stop is the answer. Pinned by
  `a_release_after_the_stop_retires_the_gate`. The "nothing to decide"
  half stays open under FAM-BUG-082.
- **Found:** 2026-09-22, sweeping the pending-gates surface. PRD-92's
  2026-09-05 candidate (retained `scope_broadened`, checkpoint `blocked`,
  worktree long since reaped, base 152 commits behind `main`) was released
  with actor and reason. The backlog row went `in_progress → pending`; the
  card stayed. `pending_human_gates` lists every retained attempt and every
  blocked checkpoint whose PRD is not completed, and a release changes
  neither, so the operator is asked again about a stop they have already
  answered — until a new attempt happens to run.
- **Also seen in the same sweep:** a `scope_broadened` stop with zero
  pending decisions is not waiting on anyone; the only offered actions were
  Release and Force-complete. That is FAM-BUG-082's shape one class over:
  a stop presented as a decision when there is nothing to decide.
- **Expected fix:** a release (or any later recovery event) for a PRD
  supersedes the stopped attempts and blocked checkpoints that precede it,
  and the gates predicate excludes them; a stop with no decidable finding
  reads as Failed on the lifecycle and offers "run again", not "release or
  force-complete".

### FAM-BUG-093 — A dispatched run can take ownership from the daemon that spawned it, then delete the claim on exit

- **Status:** Fixed (2026-09-24: a process whose delegation variable names
  the claim's owner pid is that owner's delegate whenever the pid is alive,
  independent of the start-identity probe; if the pid is dead the delegate
  stops with "no longer running" instead of recovering the claim. Two lock
  tests pin both branches and assert the owner's claim file is untouched.)
- **Found:** 2026-09-24 on the Mac, first Launch wave after FAM-BUG-092.
  The desktop reported "daemon ownership is stale" and then "daemon is not
  running" in that order.
- **Detail:** the delegate branch sat inside `claim_process_matches`, so it
  applied only when pid liveness *and* the start-identity probe both
  agreed. On macOS that probe is `ps -o lstart=`, run by a sandboxed,
  environment-cleared child; when it does not reproduce the owner's
  recorded string the child fell through to `recover_exact`, wrote its own
  pid into `control-plane.claim`, and its `Drop` removed the file on exit.
  The desktop reads that file: a foreign live pid is "stale", no file is
  "not running", while the daemon itself is still up. Unreachable before
  FAM-BUG-092 because the child could not read the claim at all.
- **Expected fix:** as landed. Recovery is for a dead owner and nobody
  else; a delegate is never a recovery path.

### FAM-BUG-092 — Start, Re-drive and Launch wave from the desktop have never run a PRD: the worker sandbox denies the child the control-plane claim

- **Status:** Fixed (2026-09-24: the child's denied read path is the
  credentials directory, not its parent; a worker test asserts the child
  can read `control-plane.claim` and cannot read another worker's
  `.session`.)
- **Found:** 2026-09-24. "Launch wave" on the Mac flashed and launched
  nothing. This box's ledger then showed every desktop-submitted execution
  since 2026-09-10, ten of them, `failed`; the 2026-09-10 `run` never
  recorded a driver session. Every PRD that ever completed was launched
  from the CLI.
- **Detail:** `control_worker::execute` isolates the child with
  `denied = capability_dir.parent()`. `capability_dir` is
  `<runtime>/capabilities`, so the denied subtree is the runtime directory
  itself, which holds `control-plane.claim`. `familiar-ai run` acquires
  the worker lock first, and that reads the claim to check it is the
  owner's delegate (FAM-BUG-062). Under Landlock on Linux or sandbox-exec
  on macOS the read fails with permission denied, the run exits, and the
  record says `worker_failed`. Introduced with the control plane in
  `4fd660e`; FAM-BUG-062's delegation fix could not have been observed
  working through this path.
- **Expected fix:** as landed. The sandbox hides other workers'
  credentials and nothing else.

### FAM-BUG-091 — Configure Local LLM accepts and reports Healthy for a model the endpoint does not serve, and never discovers from its own endpoint

- **Status:** Fixed (2026-09-24: the save refuses a model the endpoint's
  `/v1/models` does not list and names what it serves, while an
  unreachable endpoint still saves; the connection test checks the same
  thing and returns a `message` the panel actually displays; discovery
  probes the saved builtin endpoint as well as `[providers]`, tolerates a
  base URL already ending in `/v1`, and no longer bails when the full
  config fails validation; the probe uses the async client through the
  context-aware helper, because the blocking client panics on a tokio
  worker the same way the save did in FAM-BUG-087.)
- **Found:** 2026-09-24 on Linux right after FAM-BUG-087 landed. Saved
  `qwen2.5:3b` against an Ollama serving 0.5b/1.5b/7b: "Connection
  succeeded", Text primary "Healthy", and "0 discovered model(s)" beside
  a reachable server with three models.
- **Detail:** health meant "the endpoint answered". Discovery iterated the
  `[providers]` table only, and returned early with "config could not be
  read" whenever `Config::load` failed on an unrelated repository. The
  panel printed `x.message || x.status`, neither of which the result
  carried, so every test read "Connection succeeded".

### FAM-BUG-090 — Dependency Gantt places PRDs in id-text order, not the scheduler's numeric order

- **Status:** Fixed (2026-09-24: rounds and wave contents order by PRD
  number, then id, mirroring `PrdId: Ord`.)
- **Found:** 2026-09-24 on the rebuilt chart: PRD-92 sat in Wave 4 behind
  PRD-101/103/104/105/106 because `"PRD-101" < "PRD-92"` as strings and
  the greedy round assignment handed the free slots to the 1xx PRDs first.
- **Detail:** the chart is meant to be the scheduler's answer; the
  scheduler admits in numeric id order. A different tie-break gives a
  different first wave, which is the one the operator launches.

### FAM-BUG-089 — A released stop still reads as Awaiting Feedback on the lifecycle and the card

- **Status:** Fixed (2026-09-24: `stewardship::prd_lifecycle` ignores a
  latest attempt that a later release/force-complete superseded, and the
  blocked-reason card skips a checkpoint superseded the same way; both use
  one storage helper carrying the rule `pending_human_gates` already
  applied in SQL since FAM-BUG-083.)
- **Found:** 2026-09-24. PRD-92's scope stop from 2026-09-05 was released
  by the owner on 2026-09-22, the "Waiting on you" list agreed, and the
  Gantt card still said `awaiting_feedback` with "scope broadened — 2 files
  outside the declared scope" and a greyed Start.
- **Detail:** FAM-BUG-083 fixed the gates query only. The lifecycle
  derivation took the latest `driver_attempts` row at face value, and the
  blocked-reasons card read every resumable checkpoint. Three surfaces,
  one rule, applied once.

### FAM-BUG-088 — Dependency Gantt waves ignore that parents have landed, and one legacy archived file empties every scope conflict

- **Status:** Fixed (2026-09-24: the chart's rounds skip completed parents
  and completed nodes; the daemon computes scope conflicts only over active,
  unfinished PRDs; the width error names the PRD that failed; the chart
  carries `conflicts_error` and the desktop prints it above the waves; each
  wave header counts unfinished PRDs and says how many are done.)
- **Found:** 2026-09-24, Mac and Linux desktops alike. Foundations held
  103/104/106 as one launchable wave and PRD-92 sat in Wave 2, while the
  scheduler's own answer was 92+103, then 106, then 104.
- **Detail:** two defects in one chart. (1) `build_dependency_gantt` derived
  rounds from every declared parent, so PRD-92 behind three completed PRDs
  (034, 036, 099) was drawn a round later than work with no parents.
  (2) `dependencies` ran `achievable_width` over all discovered PRDs
  including `docs/prds/done/001-daemon-skeleton.md` and eleven other
  pre-contract files with neither front matter nor an Expected Files
  heading; the loader failed on the first, the whole conflict map came back
  empty, and the daemon logged "scope conflicts unavailable" without naming
  the file. The desktop then drew dependency layers and offered "Launch
  wave" on three PRDs that overlap.
- **Expected fix:** as landed. The chart is now the scheduler's answer:
  completed work is history, conflicts come from the PRDs that can still run.

### FAM-BUG-087 — Saving the local LLM configuration from the desktop panics the daemon and bricks every later operator action

- **Status:** Fixed (2026-09-24: the save uses the async-aware `block_on`
  helper the other inference queries already used; the operator dispatcher
  contains a panicking action as one failed reply and recovers its locks
  instead of poisoning them; a regression test performs the save from a
  tokio worker, which is where the desktop's request actually arrives.)
- **Found:** 2026-09-24 on the Mac. Configure Local LLM → Save and apply
  returned an error once, then "operator request state is unavailable" on
  every retry until the daemon was restarted.
- **Detail:** `save_inference_config` called `Handle::block_on` directly.
  The local transport dispatches operator mutations inline on a tokio
  worker, where that panics with "Cannot start a runtime from within a
  runtime". The panic unwound through `OperatorDispatcher::mutate` while it
  held the idempotency mutex, poisoning it; every later mutation failed at
  the lock. The existing tests call the save from a plain thread and never
  hit the runtime context, and the test file's harness had also dropped its
  runtime before running (every inference test in `tray_actions.rs` was
  already failing with "context is being shutdown"), unseen because the
  gate runs without the `tray` feature that file requires.
- **Expected fix:** as landed; the gate still skips `tray`-feature tests,
  which is a separate hole.

### FAM-BUG-086 — `ops desktop install` right after `uninstall` races launchd teardown and leaves nothing running

- **Status:** Fixed (2026-09-24: `deactivate` polls `launchctl print` until
  the job is gone after `bootout`; `activate` retries `bootstrap` for up to
  10s and returns its last error unless the job is genuinely loaded, instead
  of excusing it because a dying job still printed; a failed `kickstart`
  says the job is not loaded and how to recover.)
- **Found:** 2026-09-24, first run of `scripts/reinstall.sh` on the Mac:
  `launchctl kickstart gui/501/com.trollboy.familiar.daemon failed (exit
  status: 113): Could not find service`.
- **Detail:** `bootout` returns before launchd finishes tearing the job
  down. The immediate `bootstrap` fails; `activate` swallowed that error
  because `launchctl print` still showed the dying job; `kickstart` then ran
  against a domain the job had just left. Result: daemon plist written but
  not loaded, desktop plist never written, both processes stopped. Recovery
  was simply re-running `familiar-ai ops desktop install` once launchd had
  settled.
- **Expected fix:** as landed; the systemd path was never affected because
  `disable --now` is synchronous.

### FAM-BUG-084 — On macOS, rebuilding the desktop does not change the desktop that runs

- **Status:** Fixed (2026-09-24: `ops desktop status` reads the program back
  from the installed definition and reports a blocker when it is not the
  binary `install` would write, naming both paths and whether the two
  binaries differ; `ops desktop install` prints `program=` per definition;
  `scripts/diagnose-host.sh` compares each running process's binary with
  the tree and prints each definition's program line. The second Mac log
  after the fix showed the same stale bundle, which is what the blocker
  now says out loud.)
- **Found:** 2026-09-24, from `scripts/diagnose-host.sh` run on the Mac
  after three rebuilds had not changed what the Gantt showed. The
  installed `familiar-ai` and `familiar-ai-daemon` were byte-identical to
  the tree's release build (Sep 23 20:22). The desktop process was
  `~/Applications/Familiar.app/Contents/MacOS/familiar-ai-desktop`, built
  Sep 21 04:58, and `~/.local/bin/familiar-ai-desktop` was also Sep 21 —
  neither was the tree's Sep 23 build.
- **Detail:** `ops desktop install --desktop ~/Applications/Familiar.app`
  writes a LaunchAgent that points at the bundle. The README's install
  step copies `target/release/familiar-ai-desktop` to `~/.local/bin`,
  which the LaunchAgent never reads, and nothing rebuilds the bundle. So
  the daemon advanced through PRD-108, 109 and every fix since Sunday
  while the UI reading it stayed three days old: dependency layers instead
  of rounds, no lifecycle field, PRD-92 absent. Both operators concluded
  the system was inconsistent; only the binary was.
- **Expected fix:** `ops desktop status` reports the running desktop's
  build identity beside the tree's and says when they differ; `ops desktop
  install` warns when the LaunchAgent target is not the binary just
  installed; the README's macOS steps rebuild the bundle
  (`cargo tauri build --bundles app`) or re-run `ops desktop install`
  without `--desktop` so launchd runs `~/.local/bin/familiar-ai-desktop`.
- **Workaround now:** `bash scripts/reinstall.sh` (builds, installs, verifies,
  reinstalls the supervisor). Previously: on the Mac, `familiar-ai ops desktop uninstall &&
  familiar-ai ops desktop install` (no `--desktop`), then restart; or
  rebuild the bundle and replace `~/Applications/Familiar.app`.

## 2026-08-31 — Provider and model registration

### FAM-BUG-001 — Model inventory does not distinguish installed, registered, enabled, and routable

- **Status:** FIXED 2026-09-03 — `familiar-ai stewardship workers` reports every configured worker with its states typed separately (enabled, capability provenance per capability, model identity, measured cost) and the exact command that advances each unavailable transition. PRD-057 built the provenance vocabulary but never surfaced it; this is the diagnostic the entry asked for.
- **Original status:** Open
- **Observed:** `config provider list` and `config model list` were empty even
  though Codex, Claude, Ollama, and Unsloth were installed or running. The
  operator had to ask repeatedly what Familiar could actually use.
- **Impact:** Familiar and its operators can incorrectly describe machine
  capability. Discovery, registration, enablement, authentication, and routing
  readiness are separate states but are not presented together.
- **Expected fix:** One diagnostic command reports every candidate with typed
  states and an exact remediation command for each unavailable transition.
- **Disposition (2026-08-31):** transferred to **PRD-057** — its
  WorkerSpec/capability-provenance surface (declared, probed, observed,
  unknown) is exactly the typed-state vocabulary this inventory needs; the
  diagnostic command should land as part of 057's acceptance surface.

### FAM-BUG-002 — OpenAI-compatible endpoints were treated as Ollama

- **Status:** Fixed — committed in `4f0305e`
- **Observed:** Every non-CLI inference provider was probed through
  `/api/tags`, so authenticated Unsloth `/v1/models` endpoints could not be
  registered.
- **Impact:** A running Unsloth server was invisible to Familiar.
- **Local fix:** Added typed `runtime = "unsloth"`, authenticated `/v1/models`
  discovery, environment-reference authentication, and nested model IDs.
- **Evidence:** Focused auth/discovery tests and a live registration of
  `unsloth-local` discovering `unsloth/Qwen3.8-27B-GGUF`.

### FAM-BUG-003 — BYO-auth environment references do not integrate with macOS Keychain

- **Status:** FIXED 2026-09-03 (audit) — PRD-074 delivered it: `AuthDescriptor::CredentialStore{store,service,account}` with a `SystemCredentialStore` resolver that reads macOS Keychain at USE time via `security find-generic-password`, returns typed conditions (Missing/AccessDenied/Unavailable/UnsupportedPlatform), and never exports to the environment. Preflight probes it in daemon and supervisor context without a login shell, the resolved secret is discarded (`Ok(_)`), and only the descriptor identifiers appear in any output.
- **Original status:** Open
- **Observed:** Unsloth stores only hashes of existing API keys and Familiar
  accepts `env: NAME`, but not a Keychain credential reference. Registration
  required a one-time Keychain-to-environment bridge.
- **Impact:** Background workers cannot refresh or use the endpoint unless the
  environment is separately provisioned, despite the credential being stored
  securely in Keychain.
- **Expected fix:** Add a non-exporting Keychain auth descriptor/resolver with
  redaction, probe, daemon, and supervisor coverage.
- **Disposition (2026-08-31):** transferred to **PRD-074** (platform
  credential-store authentication: use-time Keychain resolution, never
  exported or persisted, supervisor-context coverage, fail-closed).

### FAM-BUG-004 — Claude CLI false-positive authentication

- **Status:** Fixed — committed in `4f0305e`
- **Observed:** `claude auth status` exited zero while returning
  `{"loggedIn":false}`. Familiar checked only the exit status and incorrectly
  persisted Claude as verified.
- **Impact:** An unauthenticated worker could enter the provider registry and
  fail later during execution.
- **Local fix:** Claude admission now parses the status JSON and requires an
  explicit boolean `loggedIn: true`; the incorrectly admitted provider was
  removed.
- **Evidence:** Focused true/false/malformed status tests.

### FAM-BUG-005 — Ollama discovery rejected valid chunked HTTP

- **Status:** Fixed — committed in `4f0305e`
- **Observed:** Familiar's hand-written TCP/HTTP parser attempted to parse a
  valid chunked `/api/tags` body as plain JSON and reported `provider returned
  malformed discovery`.
- **Impact:** A healthy Ollama installation with eight models could not be
  registered.
- **Local fix:** Replaced the hand-written response parser with the existing
  standards-compliant HTTP client.
- **Evidence:** Live registration succeeded and discovered all eight models.

### FAM-BUG-006 — First model enable can create an invalid mixed configuration

- **Status:** Fixed — the corruption guard landed in `4f0305e`, and the
  migration UX it was waiting on shipped with PRD-075 (now in
  `docs/prds/done/`). `familiar-ai config migrate agents --actor ACTOR` is a
  shipped command that takes an actor and writes a backup before touching the
  file, and enabling a registry model while legacy `[agents]` is present now
  refuses with that exact command rather than producing a mixed
  configuration: "cannot enable a registry model while legacy [agents] is
  configured; run `familiar-ai config migrate agents --actor ACTOR`, then
  retry". That is precisely what the Expected fix below asked for. Closed
  2026-09-21 on inspection; it had been done for some time and nobody closed
  the entry.
- **Prior status:** Open — the corruption guard is fixed (committed in `4f0305e`); the migration UX remains open
- **Disposition (2026-08-31):** transferred to **PRD-075** (audited lossless
  `[agents]` → `[worker_registry]` migration command, plus the generalized
  invariant: every configuration mutation validates the complete proposed
  configuration before atomic persistence).
- **Observed:** `config model enable codex/codex` added `[worker_registry]` to a
  configuration already containing `[agents]`. The mutation path validated the
  registry in isolation, then every subsequent load failed because the two
  sections are mutually exclusive.
- **Impact:** One nominally successful command leaves configuration invalid and
  blocks all further model enablement.
- **Expected fix:** Mutations must validate the complete proposed configuration
  before atomic persistence. Enabling the first registry worker should either
  refuse with an exact migration command or perform an explicit, audited,
  lossless migration from legacy agents.
- **Local recovery:** The machine configuration was backed up, legacy Codex
  identity was migrated losslessly into `codex/codex`, and the registry now
  loads. The mutation now refuses before writing when `[agents]` is present.

### FAM-BUG-007 — Equal unknown costs collapse automatic routing onto one worker

- **Status:** FIXED 2026-09-03 — unknown cost is now `Option<u64>`, never 0.
  The audit found PRD-032 had NOT delivered this: `WorkerDescriptor` and the
  worker-registry config both stored `estimated_cost_microusd: u64`, so an
  unmeasured worker was indistinguishable from a free one and
  `min_by_key((cost, id))` handed every stage to the lexicographically first
  id while the selection record claimed `lowest-cost-then-id`. Now: cost is
  `Option`, absent means never measured; a known cost sorts ahead of an
  unmeasured one; budget ceilings only reject costs they know; cost-based
  escalation requires BOTH the incumbent and the candidate to have known
  costs (an unmeasured worker is not provably an upgrade); and the record
  says `unmeasured-cost-then-id` when cost genuinely did not decide, instead
  of asserting a tiebreak that never happened. Regression pins all three.
- **Original status:** Open
- **Observed:** Newly enabled local and subscription workers all default to
  `estimated_cost_microusd = 0`. The deterministic cost-then-ID fallback treats
  this as known zero and selects the lexicographically first eligible worker.
- **Impact:** A nominally multi-model registry can continue using Codex almost
  exclusively; unknown cost is incorrectly conflated with free execution.
- **Expected fix:** Represent unknown cost as unknown, never zero. Use explicit
  local-resource cost semantics, qualification evidence, and empirical routing
  history before optimizing across workers.
- **Disposition (2026-08-31):** transferred to **PRD-032**, which already
  carries the never-zero-dollar-cheap subscription rule, with PRD-051's
  unknown-stays-unknown semantics as the substrate. **Operational note until
  032 lands: the multi-model registry routes essentially lexicographically —
  do not read model diversity into routing records.** This is the same defect
  class PRD-024 found in budgets (zero sails past every ceiling); 032 should
  cite it as motivating evidence.

### FAM-BUG-008 — Claude model enable selected the Codex adapter

- **Status:** Fixed — committed in `4f0305e`
- **Observed:** Provider registration correctly admitted authenticated Claude,
  but `config model enable claude/claude` mapped every provider other than the
  literal name `ollama` to `adapter = "codex"`.
- **Impact:** Familiar would invoke Codex while displaying a Claude provider
  and model identity, corrupting routing and provenance.
- **Local fix:** Provider-to-adapter selection now maps `claude` to
  `claude-code`, with a configuration round-trip regression test. The unsafe
  worker entry was removed before any execution.

### FAM-BUG-009 — Synthetic Claude discovery identity passed admission and failed every Wave 3 attempt

- **Status:** FIXED 2026-09-03 — a synthetic identity is now REFUSED at registry admission, not merely reported: a model equal to the executable's basename or the provider's own name is rejected with an explicit message, so `claude/claude` costs one error instead of a nine-PRD wave. The inventory surfaces the same condition as a blocker with remediation.
- **Original status:** Open — needs re-verification 2026-09-02 (audit)
- **Audit evidence:** Preflight now probes and dedups agent identities per session, but no evidence was found that a discovery-synthesized worker identity is refused at admission. Re-verify with a deliberately synthetic registry entry before closing.
- **Original status:** Open; unsafe worker disabled and claims recovered
- **Observed:** The CLI-login probe recorded the command label `claude` as a
  discovered model. The registry admitted `claude/claude`, routing selected it
  for every stage, and Claude Code rejected `--model claude` as
  `unrecognized_model`. The allowlisted Wave 3 session consumed all nine PRD
  attempts without launching implementation.
- **Impact:** Synthetic discovery metadata can pass provider verification,
  worker enablement, routing, and preflight, then fan one configuration error
  across an entire wave. The resulting token-usage-unknown classification hides
  the actual configuration failure.
- **Recovery:** Disabled `claude/claude` before retry and released all nine
  claims with attributed recovery events; no implementation changes occurred.
- **Expected fix:** CLI-backed providers must represent an omitted/default model
  honestly or discover a valid selectable identity. Worker preflight must test
  the configured model, and a deterministic configuration rejection must stop
  the session before consuming every PRD attempt.
- **Disposition (2026-08-31):** identity honesty and configured-model preflight
  transfer to **PRD-057** (this is FAM-FRICTION-002's predicted blast radius,
  realized). The stop-the-session circuit breaker for identical deterministic
  terminal failures is driver work with no owner — flagged for the 066
  remediation batch below.

### FAM-BUG-010 — Authored Wave 3 achievable width disagrees with scheduler

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** PRD-076 replaced authored widths with `achievable_width()` — the same computation the scheduler uses — and regenerated EXECUTION-PLAN.md as computed rounds. Live confirmation: wave 6 printed `achievable_width=1 requested_width=3` and wave 5 achieved its computed width 2 exactly.
- **Original status:** Open
- **Observed:** The execution plan claims Wave 3 achievable width `~3–4`, but
  the real scheduler computed width 1. PRD-050's declared
  `docs/contracts/providers.md` and configuration scope overlaps every other
  Wave 3 candidate directly or through coarse core scope.
- **Impact:** The approved wave promises concurrency that the actual declarations
  cannot achieve; dogfooding serializes all nine items.
- **Expected fix:** Validate and persist the exact authored wave through the
  production scheduler before approval, list every conflicting pair, and update
  either scopes/wave composition or the claimed width.
- **Disposition (2026-08-31): addressed.** The plan row was corrected to the
  measured width 1 (`ea8af19` lineage) before this entry synced, the owner's
  wave definition (dependency-ready AND scope-disjoint) is now stated in the
  plan, and owner-approved **PRD-076** (top gate) narrows scopes and
  regenerates the rows as scheduler-computed true rounds.

### FAM-BUG-011 — Preflight is silent, duplicated per stage, and looks hung

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** PRD-078: per-check heartbeats naming check id and child pid, per-session probe dedup (`probed_agents`), and last-output-line activity. Every session tonight showed live preflight progress.
- **Original status:** Open
- **Observed:** Drive emitted only `session started` for several minutes while
  preflight repeatedly probed the same routed Claude executable across stages
  and ran required `cargo test --workspace` with output suppressed. An operator
  reasonably interpreted the first healthy session as orphaned and interrupted
  it; recovery later marked it correctly.
- **Impact:** Long healthy preflight is operationally indistinguishable from a
  deadlock, and duplicate expensive probes inflate every drive startup.
- **Expected fix:** Stream check start/finish/elapsed heartbeats, deduplicate
  identical executable/auth probes, expose the active check in durable session
  status, and retain bounded captured diagnostics on failure.
- **Disposition (2026-08-31):** no owning PRD — direct code fix, batched into
  the 066 remediation below (preflight is the last silent phase; 066 shipped
  heartbeats for execution only).

### FAM-BUG-012 — Dependent PRD admitted after its dependency retained without integration

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** PRD-077 `dependency_not_integrated` decision. Fired live in wave 6: PRDs 59/60/61/63/72 were all refused while PRD-58 was attempted-but-unintegrated.
- **Original status:** Open; observed during Wave 3
- **Observed:** PRD-052 retained with `human_review_required` and never landed,
  but the same session immediately admitted PRD-054, which declares PRD-052 as
  a dependency. PRD-054's worker correctly observed that the collector and
  reconciliation implementation did not exist in its base revision and created
  a minimal compatibility seam instead.
- **Impact:** Dependency admission is checking historical/backlog state rather
  than successful integration into the session revision. Dependents can fork
  incompatible duplicate foundations, guarantee merge conflicts, and falsely
  narrate acceptance against code they never inherited.
- **Expected fix:** A dependency is satisfied for session admission only when
  its required commit is contained in the current integration revision. A
  retained, review-blocked, or verification-failed predecessor blocks or defers
  every dependent with a durable `dependency_not_integrated` decision.
- **Disposition (2026-08-31): defect against landed PRD-066** (its contract
  says dependency satisfaction consumes `integrated`; the implementation
  checks backlog status). One factual correction: PRD-054's authoritative
  frontmatter declares dependencies 047 and 051 only — both integrated — so
  its admission was contract-valid; the forked seam came from prose-level
  coupling to 052/053, which the integration-containment rule would still
  have surfaced honestly. Fix in the 066 remediation batch.

### FAM-BUG-013 — Reviewer preflight admitted an incompatible Ollama runtime

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** PRD-079 capability-probed review routing: `record_review_capability_probe` + `review_capability_probes` (migration 053) bind structured-output/tool-calling/protocol per spec identity before a reviewer is admitted.
- **Original status:** Open; review blocked for PRD-052 and PRD-054
- **Observed:** Independent review routed to Ollama. Each of three review
  attempts then failed identically because installed Ollama 0.12.3 is below
  Codex's required 0.13.4. The failure was reported as malformed structured
  review/EOF and retried three times.
- **Impact:** Preflight does not establish runtime compatibility, deterministic
  configuration failures consume the full retry budget, and completed
  implementations retain behind `human_review_required` without review.
- **Expected fix:** Worker preflight must probe the complete adapter/runtime/model
  tuple and minimum version before claims. Deterministic incompatibility must
  stop once with its real typed reason, not be reclassified as malformed model
  output or retried.
- **Disposition (2026-08-31):** tuple/version preflight probes transfer to
  **PRD-057** (capability provenance: probed, not assumed). Deterministic
  failures being reclassified as malformed output and retried is **PRD-067**'s
  durable-truth family — its environment/typed-reason machinery should absorb
  runtime-version incompatibility as a typed preflight class.

### FAM-BUG-014 — Standing batch approval still stops dependency changes as ambiguous scope

- **Status:** FIXED 2026-09-02 — bounded allowance implemented
- **Audit evidence:** PRD-080 fixed the DECLARED case: a manifest path listed in a PRD's `expected_files` now carries standing batch approval (`file_class:<class>:declared_expected_file`). The case still open is the UNDECLARED-but-necessary one: wave 6's adapter PRDs each legitimately touched `Cargo.toml`/`Cargo.lock` without declaring them, and each paused for a human scope decision. Either PRDs must declare their manifests (authoring rule) or the policy needs a bounded allowance for lockfile/manifest edits that add no new external crate. Owner decision pending.
- **Original status:** Open; PRD-050 retained
- **Observed:** PRD-050 legitimately added one dependency and changed
  `Cargo.toml`/`Cargo.lock` within the approved implementation, but global scope
  policy classified both files as `human_review`. The execution plan's standing
  batch approval did not produce a usable waiver or review decision, so the
  attempt retained as `scope_ambiguous`.
- **Impact:** Approved unattended work that necessarily changes dependencies
  cannot land, recreating the human-review wall the execution plan intended to
  remove.
- **Expected fix:** PRD-declared manifest/lock scope plus standing approval must
  become a durable, hash-bound policy decision before execution, or admission
  must refuse such PRDs before spending implementation tokens.
- **Disposition (2026-08-31):** the mechanism exists (066's hash-bound scope
  decisions); the missing rule is that a manifest path declared in the PRD's
  authoritative `expected_files` (several pending PRDs declare `Cargo.toml`/
  `Cargo.lock` for exactly this reason) is already owner-authorized by the
  plan's batch approval and should mint the hash-bound decision at admission.
  Fix in the 066 remediation batch.

### FAM-BUG-015 — Required verification cannot bind loopback in the agent sandbox

- **Status:** FIXED 2026-09-03 — both halves. PRD-078 already classified the durable result correctly (`output_environment_denied` maps "operation not permitted"/"permission denied"/etc. to `VerificationStatus::EnvironmentDenied`, never `Failed`). The missing half was the fixtures: the two tests that bind a loopback listener now report the environment condition and skip instead of failing as a product defect, so a sandboxed verification run no longer teaches workers to dismiss a required failure as narration.
- **Original status:** Open; PRD-050/057 verification affected
- **Observed:** The existing Unsloth authenticated-discovery regression binds a
  loopback listener. Focused and workspace verification inside the coding-agent
  sandbox intermittently fails with `Operation not permitted`, although the same
  test passes in the operator environment.
- **Impact:** Unrelated PRDs retain as verification failures and workers learn to
  dismiss a required workspace failure as environmental narration.
- **Expected fix:** Required verification must run in a preflighted environment
  matching its declared network/socket needs, or the fixture must use a
  deterministic transport abstraction that needs no forbidden socket. The
  durable result must distinguish environment denial from product failure.
- **Disposition (2026-08-31):** environment-needs declaration extends
  **PRD-067**'s environment-identity contract (it covers writable paths;
  sockets are the missing class). The fixture itself should also gain a
  transport seam so the regression needs no real listener — direct test fix,
  batched below.

### FAM-BUG-016 — Recovery blocks current work on stale checkpoints for already-integrated PRDs

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** Root cause was the canonical-vs-zero-padded PrdId spelling; `terminal_prds` records both. Recovery ran cleanly across every session in waves 5 and 6.
- **Original status:** Open; Wave 3 recovery requires manual integration
- **Observed:** After the Wave 3 drive retained all nine candidates,
  `familiar-ai resume all --dry-run` classified old Wave 2 PRD-048 and PRD-051
  worktrees as `stale_base`, then reported PRD-050 blocked on PRD-048 and
  PRD-052/054/057/064/069/070 blocked on PRD-051. Both predecessor PRDs are
  already integrated and durably complete on `main`; only their obsolete
  preserved worktrees are stale.
- **Impact:** The recovery planner gives obsolete checkpoint state precedence
  over current backlog and Git integration evidence. Valid, resumable Wave 3
  candidates cannot be recovered through Familiar and require manual worktree
  review, rebase, testing, and integration.
- **Expected fix:** Reconcile recovery inventory against the current backlog
  and integration revision before constructing dependency waves. Suppress or
  archive checkpoints for PRDs whose integrated commit is contained in the
  current base, and satisfy dependencies from that integrated state. A stale
  historical candidate must never make a completed predecessor block new work.
- **Disposition (2026-08-31): defect against landed PRD-066.**
- **Status update (2026-08-31): Fixed.** Root cause was an identity-spelling
  mismatch: `terminal_prds` filtered by zero-padded file stems ("PRD-048")
  while checkpoints store canonical ids ("PRD-48"), so the suppression filter
  never matched. Fixed: terminal set now carries both spellings; the
  ownership-file recovery scan and `resume <prd>` also consult it; the
  recovery planner's completed set unions durable backlog completion with
  archived location. Regression pins the padded-stem/canonical-id shape.

### FAM-BUG-017 — Provider verification constructs invalid TOML before atomic validation

- **Status:** Fixed 2026-08-31 — provenance comments now decorate the KEY
  (rendering above `key = ...`); a value prefix rendered inside the
  assignment (`models =# added by …`) and could never parse. Regression pins
  a provider followed by a commented provider table, proves the render
  reparses, and forbids the in-assignment shape.
- **Observed:** After upgrading Ollama, `familiar-ai config provider verify
  ollama --actor human:trollboy` attempted to construct
  `models =# added by ...`, then failed full-configuration parsing with
  `invalid config after edit`. The original machine configuration remained
  intact.
- **Impact:** An operator cannot refresh provider verification through the
  supported command. Atomic validation prevents corruption, but the mutation
  renderer cannot safely update a provider table adjacent to provenance
  comments.
- **Expected fix:** Make provider verification update the existing TOML item
  without attaching a provenance comment inside the `models` assignment.
  Add a byte-exact regression using a provider followed immediately by another
  provider comment/table, and prove failed rendering never reaches persistence.

### FAM-BUG-018 — Recovery commit invalidates the checkpoint it is meant to integrate

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** `rebind_operator_commit` rebinds a stale-base checkpoint whose HEAD parent is the base and whose tree is clean. Used successfully to land PRD-59 on 2026-09-02.
- **Original status:** Open; PRD-050 required an audited manual completion override
- **Observed:** The preserved PRD-050 candidate was reviewed, tested, committed
  in its owned worktree as `f8e55a6`, and cherry-picked to main as `016f641`.
  `backlog approve-and-complete` then rejected the checkpoint as `stale_base`
  because the worktree HEAD had advanced from the recorded base commit to the
  candidate commit.
- **Impact:** The normal Git operation required to integrate a dirty preserved
  candidate destroys Familiar's proof predicate before Familiar can bind the
  landed commit. Operators must choose between leaving changes uncommitted or
  using a manual completion override after successful integration.
- **Expected fix:** Recovery checkpoints must distinguish recorded base revision
  from candidate revision. Accept a candidate commit whose parent is the
  recorded base and whose tree/diff matches the recorded candidate manifest,
  then bind its equivalent cherry-pick/merge commit by patch or tree evidence.

### FAM-FRICTION-001 — Provider registration does not imply execution readiness

- **Status:** FIXED 2026-09-03 — the worker inventory surfaces the blocked transition and its remediation, so a worker that is authenticated and discovered but not routable says so and says why.
- **Original status:** Open design/UX gap
- **Observed:** Unsloth can be authenticated and discovered, but its model
  cannot be enabled until the local raw-inference agent runtime is implemented.
- **Impact:** `provider list` looks successful while the model remains unusable
  for PRD execution; `model list` is empty without explaining why.
- **Expected fix:** Surface the blocked transition and dependency in inventory
  output, and finish the neutral local runtime rather than routing Unsloth
  through Codex.
- **Disposition (2026-08-31):** the runtime is **PRD-058/PRD-063** (specced,
  waves 5–6); the blocked-transition display joins FAM-BUG-001's inventory
  work in **PRD-057**.

### FAM-FRICTION-002 — Provider CLI discovery for subscription CLIs is not real model discovery

- **Status:** FIXED 2026-09-03 — CLI-default/synthetic model identity is now explicit: refused at admission and reported as `synthetic_model_identity` in the inventory rather than silently routed as a real model.
- **Original status:** Open
- **Observed:** Codex and Claude provider probes return synthetic model IDs
  (`codex`, `claude`) derived from the login command rather than identifying the
  selected or available model.
- **Impact:** Routing provenance cannot honestly answer which hosted model will
  execute work.
- **Confirmed failure (2026-08-31):** `claude` was used as a literal model and
  rejected by Claude Code across all nine Wave 3 attempts; see FAM-BUG-009.
- **Expected fix:** Represent CLI-default/unknown model identity explicitly and
  capture the provider-reported model from execution; never present a command
  name as a discovered model.
- **Disposition (2026-08-31):** transferred to **PRD-057** (worker identity:
  provider/model addresses become aliases; CLI-default identity is exactly its
  material-parameter problem). PRD-051 already records the provider-reported
  model per observation — 057 joins the two.

### FAM-FRICTION-003 — Capability declarations are manual and unverified

- **Status:** FIXED 2026-09-03 — declared capability is distinguished from verified: the inventory reports provenance per capability and a worker whose capabilities are all `declared`/`unknown` is reported NOT routable, with `familiar-ai preflight` named as the remedy. Declared is a claim; probed/observed is evidence.
- **Original status:** Open
- **Observed:** `config model enable` requires the operator to assert planning,
  implementation, review, remediation, or narrow-task capabilities without a
  qualification probe or evidence record.
- **Impact:** Enabling all installed models risks optimistic routing claims.
- **Expected fix:** Start conservatively, distinguish declared from verified
  capabilities, and promote models through deterministic qualification and
  empirical history.
- **Disposition (2026-08-31):** already specced — **PRD-057** (capability
  provenance: declared / probed / observed / unknown) plus **PRD-032**
  (probation and promotion on empirical history). No new work needed beyond
  executing them.

### FAM-FRICTION-004 — Capability display is not canonical

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** Canonical `as_str()` accessors on the worker capability/config enums in `config/registry_workers.rs`.
- **Original status:** Open
- **Observed:** `config model list` renders `narrow-task` as `narrowtask`.
- **Impact:** Display output does not round-trip to the accepted CLI value and
  invites invalid copy/paste commands.
- **Expected fix:** Use the canonical serialized capability spelling on every
  CLI and dashboard surface.
- **Status update (2026-08-31): Fixed.** `WorkerCapabilityConfig::as_str`
  provides the canonical kebab-case spelling, `config model list` uses it, and
  a regression pins display output to the serde serialization for every
  variant.

## 2026-08-31 — Disposition summary: the 066 remediation batch

Bugs 011, 012, 014, and 016 (plus 009's session circuit breaker and 015's
fixture transport seam) are one bounded remediation of landed PRD-066
behavior: reconciled recovery inventory (016, urgent — blocks current
recovery), integration-contained dependency admission (012), declared-
manifest scope decisions minted at admission (014), preflight heartbeats
with probe deduplication (011), identical-deterministic-failure circuit
breaker (009), and the loopback-free discovery fixture (015). Bugs 009/013
identity and probe halves transfer to PRD-057; 013's typed deterministic
classification and 015's socket-needs declaration extend PRD-067's
contract; 010 is addressed by the corrected plan and owner-approved
PRD-076.

### FAM-BUG-019 — Dogfood workflow repeatedly collapses into manual per-PRD delivery

- **Status:** REOPENED 2026-09-16 — the 2026-09-02 closure is not supported by the execution ledger. The exit criterion below has never been met.
- **Reopening evidence (2026-09-16), queried from `~/.local/share/familiar-ai/familiar.db` on the Linux host:** 26 driver sessions, 39 PRD attempts, **2** with outcome `completed`, and **2** with a non-null `driver_attempts.integrated_at` (PRD-53, PRD-81). There is no merge-queue or integration table anywhere in the schema; `integrated_at` is the only integration record that exists.
- **Scope correction (2026-09-17):** those totals are **this host only** and were first written as if they were the project's. They are not. Every `repository_key` in this store is a `/home/trollboy` path; the M1 Mac ran the early work and all of the Codex execution against its own SQLite, which this box cannot read, and `driver_sessions` carries no host-identity column to distinguish them even in principle. What survives unchanged is the specific evidence below: the six PRDs named in the closure all have attempt rows **in this store**, dated 2026-09-01 and 2026-09-02, and they resolve as shown. What does **not** survive is any project-wide "never" — the Mac's ledger is unexamined, and if it holds a multi-PRD integrated wave then the original closure was right and this reopening is wrong. Resolving that needs the Mac's `driver_attempts`, which is the first thing to get. The six PRDs named in the closure resolve as:

  | PRD | Outcome | Reason | Integrated |
  |---|---|---|---|
  | PRD-38 | retained | `scope_ambiguous` | no |
  | PRD-53 | completed | — | **yes** |
  | PRD-58 | retained | `scope_ambiguous` | no |
  | PRD-59 | retained | `scope_broadened` | no |
  | PRD-60 | retained | `scope_ambiguous` | no |
  | PRD-61 | retained | `scope_broadened` | no |

  One attempt row exists per PRD in this store; no later attempt succeeded here. Wave 5's session integrated exactly one PRD; wave 6's integrated none. The exit criterion requires a **multi-PRD** wave, and no session *recorded on this host* has integrated more than one candidate — which is why the entry is reopened pending the Mac's ledger rather than closed as disproven.
- **What the closure got right, and what it did not:** the *designed pause* genuinely works. `scope_ambiguous` pausing a candidate, freeing the slot, and resuming on an owner decision is real, is what PRD-080 demonstrated, and is correctly described in `docs/prds/EXECUTION-PLAN.md` as landing under recorded scope approvals and a manual completion override. What does not hold is the step after it: no resumed candidate has ever reached integration through Familiar. PRDs 59–61 carry no `integrated_at`, and no resume recorded an attempt row at all — so either the resumes did not run through Familiar, or resume does not record an attempt. Both are findings; the second is measurement debt covered by PRD-098.
- **Why it was closeable on a wrong picture:** the closure was an audit of narratives rather than a query of the table underneath them, and neither north-star metric has ever been computed — there is no command that computes them. PRD-098 makes delivery claims a shipped query, gives "delivered autonomously" one machine-checkable definition, and requires an execution-outcome bug to close on a recorded query rather than an audit note. This entry is the migration case for that criterion.
- **Original status:** Open; systemic release-blocking dogfood failure
- **Observed:** The repeated delivery workflow is: (1) launch an allowlisted
  `familiar-ai drive` wave, (2) encounter a cascade of retained or failed
  attempts, and (3) finish, reconcile, test, integrate, and complete each PRD
  individually outside Familiar. Wave 3 reproduced the full pattern: the
  nine-PRD drive integrated zero candidates, after which all nine preserved
  worktrees were landed manually in dependency order.
- **Impact:** Familiar is acting as an expensive candidate generator rather
  than an autonomous delivery system. Batch success, recovery, merge-queue,
  review, verification, and completion claims are not credible while the
  operator remains the actual orchestrator for every successful wave.
- **Expected fix:** Treat a wave as successful only when Familiar itself
  integrates and durably completes its candidates. A deterministic shared
  failure must trip a session circuit breaker instead of cascading across the
  allowlist; recoverable retained candidates must be resumed through Familiar;
  dependency successors must consume the integrated session revision; and the
  session must emit one actionable terminal recovery plan. Add an end-to-end
  dogfood acceptance test proving a multi-PRD wave proceeds from drive through
  integration and completion without manual Git or backlog operations.
- **Exit criterion:** Complete one multi-PRD wave using only Familiar commands,
  with no manual worktree edits, cherry-picks, backlog overrides, or per-PRD
  completion commands. Until then, this bug remains open regardless of whether
  the individual underlying defects are dispositioned elsewhere.
- **PRD-078/079 reproduction (2026-08-31):** Familiar admitted both allowlisted
  PRDs concurrently and produced complete isolated candidates, but the parent
  process routed both reviews through the same incompatible Ollama
  `llama3:latest` worker. Both were retained as `human_review_required`, and the
  operator again had to audit, test, commit, cherry-pick, and complete them
  outside Familiar. Concurrent implementation worked; autonomous delivery did
  not.

### FAM-BUG-020 — Unused provider credential blocks every drive session

- **Status:** Fixed 2026-08-31; exposed at Wave 4 admission
- **Observed:** Wave 4 spent roughly eleven silent minutes in shared preflight,
  then terminated before its first attempt because the registered but unrouted
  `unsloth-local` inventory provider referenced an absent `UNSLOTH_API_KEY`.
  No enabled worker used that provider, and the local endpoint was offline.
- **Impact:** Merely registering an optional provider turns its credential into
  a global availability dependency. One offline experimental endpoint can
  prevent healthy Codex and Ollama workers from executing any PRD.
- **Fix:** When a worker registry is present, provider-auth preflight now checks
  only providers referenced by enabled registry workers. Inventory-only
  providers remain registered without gating unrelated work. A regression pins
  an unused provider with a missing credential as absent from the preflight
  report.
- **Remaining friction:** The failure was silent for roughly eleven minutes,
  reaffirming FAM-BUG-011. The execution plan also directs operators to
  `make wave-plan-check`, but the repository currently defines no such target.

### FAM-BUG-021 — Familiar invalidates its own checkpoint after remediation

- **Status:** CLOSED 2026-09-02 (audit)
- **Audit evidence:** PRD-077 refreezes the candidate after remediation (`candidate_snapshot` + checkpoint put). Live proof: PRD-59 was remediated twice on 2026-09-02 and still landed.
- **Original status:** Open; blocks autonomous Wave 4 recovery
- **Observed:** PRD-032 completed implementation, verification, two independent
  review/remediation cycles, and a third review that found one new actionable
  defect. After increasing the bounded remediation allowance and invoking
  `resume PRD-32`, recovery rejected the preserved candidate with
  `hash_mismatch`: expected
  `sha256:e7b6dfbafc1ff9e345667c891c082ea13010c16e5c6583e88ba9838cd378b116`,
  actual
  `sha256:5a15121af0d18148b42de212a2b8de4b6c263acb1f3ff70e37deb262a6443263`.
  The changed bytes were produced by Familiar's own remediation workers.
- **Impact:** A valid reviewer finding cannot be remediated through Familiar
  after the configured retry ceiling changes. Familiar turns its own durable
  candidate into an invalid checkpoint and forces manual worktree recovery.
- **Expected fix:** Every successful remediation must atomically advance the
  checkpoint manifest/hash and preserve the review lineage it supersedes.
  Resume must accept the exact candidate last produced and verified by
  Familiar, while still rejecting external mutation.

### FAM-BUG-022 — Wave 4 reproduces cascade-then-manual delivery

- **Status:** REOPENED 2026-09-16 — see FAM-BUG-019. This entry was closed on FAM-BUG-019's evidence by reference; that evidence did not hold, so the closure does not either.
- **Reopening note (2026-09-16):** the closure read "two consecutive waves integrated through the merge queue". The ledger records two integrations in the project's entire history (PRD-53 on 2026-09-01, PRD-81 on 2026-09-04), in separate sessions, neither of them a wave. Cascade-then-manual delivery is therefore unretired as a reproduction.
- **Original status:** Open; concrete second reproduction of FAM-BUG-019
- **Observed:** Wave 4 first spent roughly eleven silent minutes before an
  unused Unsloth credential aborted the whole session. After that was fixed,
  PRD-032 implemented successfully but the routed Qwen reviewer emitted prose
  before JSON three times; Familiar retained it and immediately admitted
  PRD-055 against the same broken review fleet. The operator interrupted the
  cascade. A restricted llama3 reviewer then completed two useful remediation
  cycles, but FAM-BUG-021 made the candidate unresumable.
- **Impact:** The live workflow is again `drive` → shared-stage cascade →
  manual per-PRD landing outside Familiar. The operator had to supply the
  circuit breaker, repair provider routing, alter retry policy, and now recover
  the checkpoint manually.
- **Expected fix:** FAM-BUG-019's end-to-end exit criterion remains mandatory.
  Additionally, identical structured-output failures must quarantine the
  worker for the session and fall through to another eligible reviewer before
  retaining the PRD or admitting another candidate.
- **Second cascade (2026-08-31):** After PRD-032 was landed manually, a new
  three-PRD drive implemented and fully tested PRD-055, then retained it as
  `scope_broadened` solely because
  `crates/familiar-ai-mcp/tests/integration.rs` was not in the PRD's expected
  files even though the PRD declares the entire MCP source surface and requires
  deterministic offline query coverage. Familiar immediately admitted
  PRD-056 on the unintegrated base. The operator again had to interrupt the
  session to prevent a second incompatible-candidate pile.
- **Third cascade (2026-08-31):** After PRD-055 was landed manually, Familiar's
  PRD-056 worker explicitly reported that detached CLI execution, the live
  socket host/client, MCP migration away from direct SQLite, worker adoption,
  and full capability sessions were not implemented. Verification retained the
  knowingly incomplete candidate, and the driver immediately admitted PRD-062
  anyway. The operator again supplied the missing circuit breaker.
- **PRD-056 recovery cost (2026-08-31):** The preserved candidate passed some
  focused checks but was missing acceptance-critical daemon transport, detached
  CLI lifecycle, MCP isolation, worker adoption, ownership races, atomic
  PRD-064 reservations, accounting, and shared-service boundaries. External
  recovery required two explicit acceptance audits and ultimately landed a
  4,551-line change across 32 files. The first recovery pass even reported a
  green workspace while correctly admitting that MCP still opened SQLite and
  legacy CLI orchestration remained duplicated; only a second audit closed
  those gaps. This is the exact failed workflow: `familiar-ai drive` produced
  a retained partial, the session cascaded into PRD-062, and the operator then
  finished PRD-056 individually outside Familiar.

### FAM-BUG-023 — Valid config cannot be edited because disabled delivery defaults active

- **Status:** Fixed 2026-08-31 — a legacy `[delivery]` table with
  `enabled = false` (or an empty table) now deserializes to disabled mode
  through a compatibility deserializer; an explicit `mode` always wins.
  Regression starts from the exact historical table shape and proves it
  validates.
- **Observed:** Every `config model disable` command failed atomic validation,
  first demanding `max_deliveries_per_session`, then a remote/base, because the
  existing `[delivery] enabled = false` table defaulted `mode` to
  `reviewed_pr_manual`. Adding `mode = "disabled"` made the supported commands
  work.
- **Impact:** An older valid configuration can run drives but cannot be changed
  through Familiar's own atomic configuration commands, blocking emergency
  worker quarantine.
- **Expected fix:** Legacy `enabled = false` must migrate or deserialize to
  disabled mode before validation. Add an edit regression starting from that
  exact historical table.

### FAM-BUG-024 — Drive preflight drops verification configuration and hides failures

- **Status:** Fixed 2026-08-31 by PRD-078; installed and live-verified 2026-09-01
- **Observed:** Five PRD-062 drive sessions spent roughly five to ten silent
  minutes apiece running `verification.workspace-tests`, then reported only
  `command exited with code Some(101)`. `preflight::run` reconstructs each
  `ReviewVerificationConfig` as a `PreflightCommandConfig`, discarding its
  configured `environment`, `timeout_ms`, applicability, and captured output;
  `command_check` redirects stdout and stderr to null. Direct
  `cargo test --workspace` passed on the same revision.
- **Impact:** Operators cannot distinguish a code failure from an execution-
  environment denial, and the configured verification contract is not the
  contract preflight executes. Each retry pays for a full silent suite before
  Familiar touches the PRD.
- **Workaround used:** The duplicated pre-claim workspace check was marked
  optional, Familiar generated the PRD-062 candidate, and the operator ran the
  complete workspace suite manually before integration. The check still ran
  during review and reproduced the environment-sensitive failure there.
- **Expected fix:** Execute the original verification specification without
  lossy conversion, enforce its finite timeout, retain bounded redacted stdout
  and stderr as durable evidence, stream a heartbeat naming the active check,
  and classify environment denial separately from test failure.
- **Fix:** Preflight now executes the exact configured verification argv,
  working directory, environment, and timeout; retains bounded redacted output;
  probes only routed providers; deduplicates session checks; emits flushed
  heartbeats with elapsed time and PID; and distinguishes environment denial.
  Contract tests and the complete workspace suite pass.

### FAM-BUG-025 — Reviewer capability mismatch retries and forces manual recovery

- **Status:** Fixed 2026-08-31 by PRD-079; installed and live-verified 2026-09-01
- **Observed:** PRD-062 implementation and focused verification completed, but
  review routed to Ollama `llama3:latest`. The runtime reported that the model
  does not support tools. Familiar retried the same incompatible reviewer three
  times, each including five transport reconnects, then converted the result to
  `HumanReviewRequired` and retained the PRD.
- **Impact:** A deterministic capability mismatch is treated as review judgment
  rather than routing failure. The candidate can be correct and fully tested,
  yet Familiar cannot finish it. The operator must inspect the preserved
  worktree, run tests, commit, cherry-pick, and manually complete the backlog.
- **Expected fix:** Probe and persist structured-review/tool capability before
  selection. On a deterministic capability failure, quarantine that worker for
  the session and reroute to an eligible independent reviewer; do not consume
  all review attempts or label infrastructure failure as human judgment.
- **Workflow evidence:** Wave 4 again followed the exact sequence: (1)
  `familiar-ai drive --prd PRD-62 --max-prds 1`; (2) preflight and reviewer
  cascade; (3) PRD-062 finished individually outside Familiar. FAM-BUG-019's
  end-to-end exit criterion remains unmet.
- **Fix:** Structured-output, native-tool, protocol, and minimum-runtime probes
  are now persisted with age and provenance. Deterministic incompatibility is a
  typed routing outage, quarantines the worker once, and reroutes without
  consuming malformed-output retries. Regressions cover tool-less llama3 and
  Ollama 0.12.3. Migration 052 is also tested from a populated pre-052 worker
  database, not only from a fresh schema.

### FAM-BUG-026 — Fresh-database tests missed a production migration failure

- **Status:** Fixed 2026-08-31 during PRD-062 release verification
- **Observed:** PRD-062's candidate passed focused migration tests and the full
  workspace suite, but the freshly installed binary failed `familiar-ai next`
  against the real database: `migration 51 failed: worker specs are immutable`.
  Migration 051 attempted to update historical `worker_specs`, contradicting
  the immutability trigger installed by migration 041. Fresh fixtures contained
  no existing Ollama worker spec and therefore never executed the failing row.
- **Impact:** A fully green candidate made every installed CLI command that
  opens storage unusable on the actual machine.
- **Fix:** Migration 051 now leaves historical worker specs immutable and binds
  their aliases to explicit degraded, unverified artifact records. A regression
  upgrades a pre-051 database containing an Ollama worker, verifies the degraded
  alias, and proves the historical worker row still rejects mutation.
- **Migration-number collision (2026-09-01):** PRD-079 initially used migration
  052 while remote PRD-077 independently landed a different migration 052. The
  pre-rebase binary had already recorded the probe-table bytes as version 52 in
  the live database. After rebase, migration 053 failed because that table
  existed and PRD-077's selection-decision widening had never run. Migration
  053 now idempotently repairs both histories, and a regression constructs the
  exact collision ledger and verifies both schemas. The repaired release then
  migrated the live database successfully.
- **Required systemic follow-up:** Every data-migrating PRD needs at least one
  populated prior-version fixture that exercises production constraints and
  triggers; empty fresh-database migration tests are insufficient release
  evidence.

## 2026-08-31 — Disposition summary after waves 3–4 (bug-carrier PRDs created)

Per the bugs-preempt policy, every open entry now has a carrier scheduled
NEXT, closed evidence, or an explicit direct-fix assignment:

- **Closed by landed PRDs:** 003 (PRD-074 platform credential stores),
  006 residual (PRD-075 audited migration + whole-config validation),
  007 (PRD-032 empirical probation; verify on next multi-worker session),
  016 (fixed `22cc6f9`), 026 (fixed `299c013`), FRICTION-004 (fixed
  `3a0ec95`). 001/FRICTION-002/FRICTION-003 are partially closed by
  PRD-057's worker-spec identity; the remaining probe-before-eligible gap
  continues as PRD-079.
- **PRD-077 (runs first):** 012, 018, 019, 021, 022, and 009's
  deterministic-failure circuit breaker. 077's final acceptance criterion
  is 019's closure condition.
- **PRD-078:** 011, 015, 020, 024.
- **PRD-079:** 013, 025, and the probe-before-eligible residue of 009.
- **PRD-080:** 014 and the wave-3 PRD-050 / wave-4 PRD-055 scope walls.
- **Direct fixes, next Claude session:** 017 (provider-verify TOML
  rendering), 023 (config edit blocked by disabled delivery defaults).
- **Recorded, not carried:** waves' patch-application brittleness
  (wave-3 §8 / wave-4 §8) is Codex-CLI tool behavior, not Familiar code;
  tracked here for visibility only.

## 2026-08-31 — PRD-077 landed (direct implementation)

- **012 (dependency admitted past unintegrated predecessor): Fixed.**
  Selection defers dependents of session-attempted, unintegrated
  predecessors with a durable `dependency_not_integrated` decision, and
  in-flight workers' scopes are held across scheduling passes
  (`deferred_scope_held`) — the per-pass-local scope hole is closed.
- **018 (recovery commit invalidates checkpoint): Fixed.** A stale-base
  candidate with exactly the committed-candidate shape rebinds
  (`rebound_operator_commit`); tampered worktrees stay invalid.
- **021 (remediation orphans its own checkpoint): Fixed.** After a review
  cycle with remediation, the candidate snapshot is recomputed and the
  checkpoint's diff hash and manifest advance.
- **019/022 (cascade-then-manual): Fix implemented; live confirmation
  pending.** The closure regression passes — a two-PRD shared-scope wave
  completes end to end through drive alone, integration-ordered, with the
  circuit breaker (3 identical deterministic failures → stop with an
  executable recovery plan) and `implementation_incomplete`
  terminalization (empty manifest or a `FAMILIAR-INCOMPLETE:`
  self-declaration retains before any review spend). `resume all` now
  LANDS finished candidates into the checked-out branch. These entries
  close for real when the M1's next live wave completes hands-off.
- **New FAM-BUG-027:** `worker_lock::simultaneous_fallback_claims_have_
  exactly_one_winner` flakes under full-suite parallel load (passes
  targeted, twice). A flaky test inside the verification gate can halt
  unattended sessions. Open; surfaced 2026-08-31 during PRD-077
  verification.

## 2026-09-21 — PRD-090 landed by hand, recorded as the rule requires

### FAM-BUG-065 — A false completion stranded finished work outside every recovery path

- **Status:** Open
- **Found:** 2026-09-21, trying to land PRD-90 after FAM-BUG-064 was fixed.
- **Detail:** FAM-BUG-064 marked PRD-90 `completed` while its eight changed
  files sat uncommitted in a worktree. Fixing the ordering stops that
  happening again; it does not undo the one that happened. And because
  completion is immutable by contract
  (`docs/contracts/completion-is-immutable.md`), the status cannot be walked
  back — so `resume` refuses the PRD outright:

      PRD-90 is already completed and integrated; its preserved worktree is
      historical evidence, not resumable work

  A false completion therefore does not merely misreport. It **locks the
  work out of every supported recovery path**, permanently.
- **How it was resolved this time:** by hand. `git apply` of the worktree
  diff onto `main` — 1,631 lines across seven files plus one new test file —
  which applied cleanly, compiled, and passed the full suite. PRD-090's
  namespacing absorbed the `ops gate` command added since, alias and all.
- **Recorded as a FAM-BUG-019 recurrence**, per the scheduling rule that a
  hand-merge to `main` "goes in the bug log, not just the terminal history".
  The nuance, stated rather than used as an excuse: Familiar did the
  implementation and the review, and only the final landing was manual. The
  reason it needed hands was a defect in Familiar, now fixed.
- **Expected fix:** completion is immutable, so the answer is not a reverse
  transition. Either a PRD whose candidate never landed must be unreachable
  as a completion in the first place — which FAM-BUG-064 now enforces — or
  there needs to be a supported way to land evidence that is already marked
  complete, distinct from resuming it. The second is a real design question
  and deserves a PRD.

## 2026-09-21 — a false completion, and the rule that replaces reversing it

### FAM-BUG-064 — The backlog reaches `completed` before the candidate reaches `main`

- **Status:** Fixed 2026-09-21 — completion is now the last durable write.
  `resume_implemented_checkpoint` defers completion and stops at `approved`;
  the caller lands the candidate and only then calls `approve_and_complete`,
  which writes the approval and the completion in one transaction with the
  merge commit bound to it. A failure before landing now leaves the PRD
  resumable instead of claiming done.

  This is the order the code's own contract already specified — "the driver
  alone may integrate and then commit backlog completion" — and the resume
  path had it inverted. It also removes a phase label that was lying:
  `("integrated", "backlog_completion_committed")` claimed integration while
  meaning bookkeeping, and a path that does not integrate no longer writes
  it.

  Pinned by `the_resume_path_does_not_complete_before_landing`. Because
  completion is immutable, preventing a false one is the only remedy
  available, which makes the ordering a correctness property rather than a
  preference.
- **Prior status:** Open
- **Found:** 2026-09-21, re-driving PRD-90 after FAM-BUG-062 was fixed.
- **Detail:** The resume passed verification, returned a clean independent
  review (`ReadyForHumanApproval`, `CleanReview`), wrote
  `backlog_prds.status = completed`, and then failed with
  `execution history failed: database error: database is locked`. Delivery
  never ran. `main` did not move, `integrated_at` is empty, and the eight
  changed files are still uncommitted in the worktree — while the backlog
  says the PRD is done and the gate that would have shown it has gone.
- **Contributing:** the checkpoint phase list names a phase `integrated`
  whose detail is `backlog_completion_committed`. The name promises delivery;
  the detail is bookkeeping. A reader auditing phases would conclude the
  candidate had landed.
- **Contributing:** `busy_timeout` is 5000ms, so the daemon held a write
  transaction for longer than five seconds. FAM-BUG-062's delegation fix let
  the delegate past the worker lock, and the worker lock had been the thing
  incidentally preventing two processes from contending on SQLite. One
  failure was traded for another, and this entry records that honestly.
- **Expected fix:** completion is the last durable write, after delivery
  succeeds — or the two are one transaction. A lock timeout must abort the
  run, not leave a PRD claiming done.
- **Not fixed by reversing the status.** See
  `docs/contracts/completion-is-immutable.md`: there is deliberately no path
  out of `completed`, and PRD-103 is the successor for what PRD-090 left
  unmet. This entry is about the ordering defect that produced a *false*
  completion, which is a different thing from a *partial* one.

## 2026-09-21 — actions taken in the window report nothing

### FAM-BUG-063 — A dispatched command's output is discarded, so failures are silent

- **Status:** Fixed 2026-09-21 — the child's stdout and stderr go to a
  per-execution log, and a non-zero exit records its code plus the tail of
  that output and the log path, instead of the bare string `worker_failed`.

  A file rather than a pipe, deliberately: these children are detached and
  long-lived, and a pipe nobody drains fills and blocks the child — the
  capture would have caused a worse bug than it fixed. Losing the log is a
  warning, not a failed execution; failing a run because its logging failed
  would be the tail wagging the dog.

  Pinned by `a_failed_worker_records_more_than_the_fact_that_it_failed`. All
  three dead buttons found this session — Release, Force-complete and
  Re-drive — would have announced themselves on the first click.
- **Prior status:** Open
- **Found:** 2026-09-21, while diagnosing FAM-BUG-062.
- **Detail:** `control_worker` spawns every dispatched command with
  `.stdout(Stdio::null()).stderr(Stdio::null())`. When the owner clicked
  Re-drive, the child failed with `cannot acquire mutating orchestrator
  ownership` and that message went nowhere — not to the daemon log, not to
  the UI, not to the execution record beyond `state = failed`.
- **Impact:** this is why FAM-BUG-062 survived. It is also the third
  instance of the same pattern in two days: the tray's Release and
  Force-complete buttons failed validation on every click since they
  shipped, Re-drive failed on every click, and all three were
  indistinguishable from a button that does nothing.
- **Expected fix:** capture the child's output into the execution record so a
  failed execution can say why, and surface that in the window. PRD-102 put a
  count on the icon and a signpost in the menu, but an action taken *inside*
  the window still reports nothing at all.

## Backfilled entries

These two were recorded only as bullets inside dated narrative below, so no
count following this file's own convention could see them. The narrative is
left where it is; these give them a findable entry.

### FAM-BUG-027 — `worker_lock` simultaneous-fallback flake was a real claim race

- **Status:** Fixed 2026-09-01 — `WorkerLock::create` wrote claim JSON into an `O_EXCL` file non-atomically, so a concurrent claimant read a half-written file, judged it corrupt, deleted the live winner's claim and claimed too. Two owners of an exclusive lock. Surfaced 2026-08-31 as a flaky test; upgraded to a product race and fixed. Narrative under *2026-09-01 — PRD-076 first drive attempt*.

### FAM-BUG-028 — Evidence-erasing redaction

- **Status:** Fixed 2026-09-01 — retained preflight output was redacted wholesale, so a failing check's evidence was destroyed before anyone could read it. Now a line-level contract: a failing-test line survives redaction. Narrative under *2026-09-01 — PRD-076 first drive attempt*.

## 2026-09-21 — the tray's Re-drive button cannot succeed

### FAM-BUG-062 — Re-drive dispatches a command that needs a lock the daemon holds

- **Status:** Fixed 2026-09-21 — a command the owner dispatched now runs as
  the owner's delegate. `control_worker` passes `FAMILIAR_AI_DELEGATED_BY`
  with its own pid when it spawns, and `WorkerLock` yields to a process that
  names the live owner exactly. Exclusion is preserved on both sides:
  anything that is not the owner's child is still refused, and two delegates
  still exclude each other through their own `O_EXCL` lock — the property
  the claim exists for, which a naive bypass would have thrown away.
  Verified against the live daemon: the same command refuses without the
  variable and proceeds with it. Four regressions in `gate_escalation_ui.rs`
  cover the delegate, the stranger, a wrong pid, and two delegates.
- **Note:** `Action::StartPrd` had the identical defect — it dispatches
  `familiar-ai run`, which takes the same lock — so the tray's two primary
  actions were both dead. Both are fixed by the same change.
- **Prior status:** Open


`Action::ResumePrd` submits `["familiar-ai", "resume", <prd>]` to the
control plane as a detached execution (`tray_data.rs`). That command needs
exclusive mutating orchestrator ownership — and the daemon dispatching it
already owns it:

    cannot acquire mutating orchestrator ownership:
    Familiar control-plane owner pid 3867955 is live;
    socket state must be diagnosed and explicit recovery used

So the button can never work while the daemon is running, which is the only
time the tray exists to be clicked. Observed 2026-09-21: the owner pressed
Re-drive on PRD-100 at 00:56:16, the execution ran and failed at 01:04:24,
and the UI showed nothing at any point. Reproduced directly from a terminal,
where it fails immediately with the same error.

Two defects, and the second is why the first survived:

1. The dispatch shape is self-defeating. Either the daemon performs the
   resume in-process, or the spawned command has to run under the owner's
   authority rather than contending with it. That is a design decision, not
   a patch.
2. **Nothing reports it.** No toast, no spinner, no row, no change to the
   button — success and failure are indistinguishable from a click that did
   nothing. PRD-102 put a count on the icon and a signpost in the menu, but
   an action taken from the window still reports nothing at all. This is the
   same class as the tray's Release and Force-complete buttons, which failed
   validation on every click since they shipped and were only found on
   2026-09-20 by reading the code.

Until it is fixed, `familiar-ai resume <prd>` from a terminal works only
with the daemon stopped.

## 2026-09-19 — round 1: one infrastructure bug voided a wave, one review bug blocked a candidate

### FAM-BUG-059 — Verification leaked a Docker network and a cargo-cache volume per PRD

- **Status:** Fixed 2026-09-19 — every check pinned to one compose project (`-p familiar-ai-verify`)


Every `[[review.verification]]` check ran `docker compose run` from a
worktree directory, so Compose derived a project name from that directory
(`prd-90`) and created `prd-90_default` plus `prd-90_cargo-cache`. `--rm`
removes the container and neither of those. Docker's default pool holds
about 31 networks.

Round 1 failed 4 of 4 with `verification_failed` and the evidence read
`failed to create network prd-90_default: all predefined address pools have
been fully subnetted`. Cost: $52.02 and 70 minutes, proving nothing about
the PRDs.

Leaking since at least 2026-09-04 — `prd-71_default` was still present.
Some share of the historical 39 attempts likely died of this and were
attributed to the PRDs or the models.

Two things kept it invisible for six weeks:

- The taxonomy records infrastructure failure and candidate failure with
  the same code. `verification_failed` cannot distinguish "this code is
  wrong" from "the host has no subnets left".
- The per-project volume leak gave every run a cold cargo cache, so
  1,045-4,292s runs looked like ordinary slowness rather than a system
  recompiling the world every time. After the fix: 181s.

Fixed by pinning every verification check to one Compose project
(`-p familiar-ai-verify`): one network, one warm shared cache, reused by
every check, worktree and worker. Recorded in
`docs/contracts/verification-gate.md`.

### FAM-BUG-060 — The independent reviewer is given the static scope ceiling, not the authorised scope

- **Status:** Fixed 2026-09-20 — `review_allowed_paths` includes the PRD contract when expansion is enabled


`compile_scope_policy` deliberately builds `allowed_paths` from the
configured ceiling only and applies the PRD's contract at adjudication
time. But the review task was built from that ceiling alone, so a reviewer
was told scope was `["crates/"]` while the PRD legitimately declared
`README.md`.

PRD-090 was blocked by a `scope_violation` finding
(`readme-outside-allowed-paths`) on a file its own `expected_files`
declares. The inconsistency is provable: PRD-096 declares
`docs/contracts/command-model.md`, also outside the ceiling, and its scope
*adjudication* recorded `contained`.

This would have blocked much of the queue: 092, 098, 099 and 101 declare
`README.md`; 088, 089, 094, 097, 100 and 101 declare `config/default.toml`;
088 and 092 declare `docker-compose.yml`.

Fixed in `run.rs`: the review task's `allowed_paths` now includes the PRD's
contract when `allow_prd_expected_file_expansion` is set. Pinned by
`the_reviewer_receives_prd_declared_paths_not_only_the_static_ceiling`.

### FAM-BUG-061 — No independent review has ever been costed

- **Status:** Open — owner PRD-086, whose subject this is


Only two sites write usage observations: `run.rs` (stage `implementation`)
and `batch_review.rs` (stage `review`). The ledger contains `implementation`
and `execution` rows and **zero `review` rows**, because the batch-review
path has never run. The independent reviewer's model calls — every one in
the project's history — are unaccounted.

The `resume` of round 1 ran independent reviewers for four PRDs and
recorded no cost at all. This is a direct contributor to 19 of 39 attempts
carrying no cost and to the $359 lifetime total being an undercount.

Owner: PRD-086, which exists to make `CostUnmeasured` the exception. Not
fixed here because instrumenting the review path is that PRD's subject.

**Open: an unidentified flake.** One unreproducible red on 2026-09-19
(`9a8d970`), green on immediate re-run. FAM-BUG-027's `worker_lock` race
was fixed on 2026-09-01 so it is probably not that. The verdict detail was
too coarse to name the test, which is itself now fixed: `scripts/gate.sh`
records failing test names, so the next occurrence will identify itself.

## 2026-09-01 — PRD-076 first drive attempt: two bugs found, both fixed

The first post-bug-gate drive (PRD-076) failed in preflight after 861s of
workspace tests (exit 101) with `output=[REDACTED] omitted_bytes=20819`.
Two defects, both fixed and pushed:

- **FAM-BUG-027 — UPGRADED from flake to product race, fixed.**
  `WorkerLock::create` wrote claim JSON into an O_EXCL file
  non-atomically; a concurrent claimant reading the half-written file
  judged it corrupt, "recovered" (deleted) the live winner's claim, and
  claimed too — two owners of an exclusive lock. This is what failed the
  workspace suite under load, and in production it could put two drivers
  on one repository. Fixed: claims are written to a unique temp, synced,
  and hard-linked into place (atomic appearance, AlreadyExists on loss);
  the regression runs 25 iterations of the 8-thread claim storm.
- **FAM-BUG-028 — evidence-erasing redaction, fixed.** Retained preflight
  failure output redacted all-or-nothing: one credential-shaped string
  anywhere (the repo's own auth-test fixtures print them) replaced the
  entire capture with `[REDACTED]`, hiding the failing test's name — the
  precise diagnosability FAM-BUG-024's fix promised. Redaction is now per
  line; a failing-test name provably survives beside a redacted token.

Operational note: heartbeats (078) worked as designed throughout — the
861s run was visible the whole way, and the failure arrived classified.
The rerun requires a FRESH BUILD on the operator machine: pull, rebuild,
reinstall the binary, then rerun the 076 drive.

### FAM-BUG-029 — Workspace verification self-collides with the orchestrator's singleton lock

- **Status:** Fixed 2026-09-01
- **Observed:** The rebuilt PRD-076 drive failed preflight again (exit 101,
  731s) — and the fixed per-line redaction named the cause exactly:
  `cli_run` spawned the real `familiar-ai` binary, which tried to acquire
  the control-plane lock and found "owner pid 48485 is live" — the pid of
  the drive session running the suite. The workspace-tests preflight can
  NEVER pass inside a drive session while any spawned-CLI test resolves the
  shared runtime directory. This also retro-explains wave 4's five silent
  exit-101 preflights ("direct cargo test passed on the same revision" —
  manually there is no live drive holding the lock) and a standing source
  of parallel-suite flakiness: 15 of 16 spawned-CLI invocations across the
  cli test files never isolated `XDG_RUNTIME_DIR`, so they also raced each
  other's locks under cargo's parallel execution.
- **Fix:** every test that spawns the CLI binary (cli_run, cli_next,
  cli_recovery, cli_bootstrap, cli_record_complete, cli_stewardship,
  driver_hygiene, identity_continuity, stewardship_cross_surface) now sets
  a per-test `XDG_RUNTIME_DIR`, making the suite hermetic with respect to
  live Familiar processes and with itself. The 078 redaction pin was
  updated to the FAM-BUG-028 line-level contract (a failing-test line must
  survive beside a `[REDACTED LINE]`).
- **Credit where due:** this diagnosis was only possible because of the
  chain landed hours earlier — the atomic lock made the collision
  deterministic instead of racy, and per-line redaction let the evidence
  name the pid.

### FAM-BUG-030 — Workspace suite hangs on macOS at/after the mcp integration binary

- **Status:** REOPENED 2026-09-21 — PRD-104 verification repeatedly imposed
  roughly 60–90 seconds before each freshly linked macOS test executable,
  both inside and outside the Codex sandbox. No individual test hung, and all
  suites reached were green, but three full-workspace attempts were stopped
  after progressing into daemon integration binaries because the per-binary
  delay made completion take hours. Focused PRD-104 tests and strict lint
  complete normally. This is build/test execution friction, not a desktop
  permission requirement; no Screen Recording, microphone, Accessibility, or
  UI automation is used.
- **Prior closure:** CLOSED 2026-09-01 — fix candidate 5f517db validated by a clean
  unattended Mac run (`mac-build-speed-20260901T113450Z`): build 3m50s warm,
  full suite completed in 281s, no stall. The historical 45-minute runs were
  cold-build time (41GB target/), not the hang. The suite's exit 101 on that
  run was the control_plane_boundaries path-scan test, fixed separately with
  the PRD-076 landing.
- **Prior status:** Open — fix candidate landed (`5f517db`) but UNVERIFIED; the
  2026-09-01 diagnosis run splits the defect into two questions (see below)
- **Observed:** The fourth PRD-076 preflight passed the previously-failing
  cli_run isolation, progressed through 20+ test binaries, then produced no
  further output and hit the enforced 30-minute timeout. Retained stderr's
  last line names `familiar-ai-mcp tests/integration.rs`, but output
  bounding may have dropped later `Running` lines — the hang is at or after
  that binary. Every test in that file is in-memory MockTransport
  (structurally cannot hang); the wave-one watcher crate (macOS FSEvents
  backend, documented unaudited debt) runs later and is the prime suspect.
- **Control evidence (Linux, same revision):** the identical suite completes
  in 16.8 seconds wall; the mcp, watcher, summary, review, and storage
  suites each finish in under two seconds. The hang is macOS-specific.
- **Fix candidate (`5f517db`, UNVERIFIED):** the daemon integration test
  now spawns the daemon with `.process_group(0)` and SIGKILLs the whole
  group before reading stderr — theory: a leaked grandchild (tray helper,
  Mac default features) inherited the stderr pipe, so `read_to_string`
  blocked forever after the daemon itself died. Unverified because the only
  Mac run since (`docs/diagnostics/suite-hang-20260901T093435Z.txt`)
  diagnosed HEAD `2b0b8713`, which PREDATES the fix.
- **2026-09-01 reframe — this is two questions now:** that run hit the 45 m
  cap with explicitly NO 180 s output stall ("very slow, not stuck"); the
  storage suite's test phase took 15.60 s, and the report's last line is an
  integration.rs binary starting. So the 45 minutes are dominated by cargo
  BUILD time, not test execution. Question 1: build throughput pathology
  (~44 min on the Mac for a workspace this Linux box builds and tests in
  seconds). Question 2: the original hang, possibly fixed by `5f517db`,
  unconfirmed either way.
- **Build-slowness hypotheses:** cold/invalidated `target/` — incremental
  cache busted per run, so every run recompiles the world; Spotlight
  indexing `target/` — mds_stores chasing thousands of fresh artifacts;
  XProtect/Gatekeeper assessment of every freshly linked test binary —
  macOS scans new unsigned binaries on first exec, and a workspace suite
  links dozens; memory pressure/swap if Codex or another drive session runs
  concurrently; thermal throttling; dsymutil debug-info cost per linked
  binary.
- **Next action:** run `./scripts/diagnose-mac-build-speed.sh` on the Mac —
  one command, unattended, self-updating; commits and pushes its own
  report to `docs/diagnostics/`.
- **Diagnose scripts' division of labor:** `diagnose-suite-hang.sh` answers
  "is it stuck" (180 s stall detector, stack `sample`, open files, 45 m
  cap). `diagnose-mac-build-speed.sh` answers "why is it slow": phase 1
  wall-clocks `cargo test --workspace --no-run` with per-crate cargo
  timings (when the installed cargo supports them) plus macOS suspect
  snapshots (Spotlight, memory, thermal, disk, APFS snapshots, concurrent
  processes, mid-build top-CPU); phase 2 reruns the suite on the now-warm
  build with the same stall detector and a 20 m cap, so the hang check —
  and `5f517db` validation — rides along in the same run. Every report now
  states whether `5f517db` is an ancestor of the HEAD under test, so no
  future report is ambiguous about whether the fix was being tested. A
  clean phase 2 completion is itself evidence (the hang would then be
  drive-context-specific or fixed).
- **Also fixed while establishing the control:** the third timing-margin
  flake in familiar-ai-agent (`bounded_execution_kills_a_timed_out_process`
  asserted a sub-2s kill; under 20-thread suite load the margin slipped;
  the bound is now 8s — well under the fake's 10s sleep ceiling, still
  proving enforcement).

### FAM-BUG-031 — `drive` exits 0 after a zero-work abnormal termination

- **Status:** Fixed (this commit)
- **Observed:** The first Linux-hosted PRD-076 session terminated
  `preflight_failed attempted=0 completed=0` — and exited 0. `logged.sh`
  recorded "exit 0" in the pushed log's commit message; any wrapper,
  cron, or CI gating on the exit code would have read the session as
  healthy. The daemon-supervised path already classified this correctly
  (`DriveTermination::worker_should_restart` names preflight failure
  crash-like so launchd retries), but the interactive `drive` subcommand
  discarded the summary: `Ok(_) => ExitCode::SUCCESS`.
- **Fix:** the `drive` CLI arm now consults the same predicate the
  supervisor uses: crash-like terminations (preflight failure, lost
  worker heartbeat, storage failure, interrupt, unclassified result)
  exit nonzero with the session id and reason on stderr; deliberate
  policy/budget stops still exit 0. One classification, both surfaces.
- **Deliberately unchanged:** `deterministic_failure_cascade` still
  exits 0 — the breaker is a designed stop that delivers a recovery
  plan, not a crash. Revisit if an operator script ever needs to
  distinguish it.

### FAM-FRICTION-005 — Session logs silently swallowed by `.gitignore`

- **Status:** Fixed (this commit)
- **Observed:** `logged.sh` finalize reported `PUSH FAILED - log saved
  locally` for the first Linux drive session. Nothing was wrong with the
  push: `.gitignore`'s blanket `*.log` (line 136) made the quiet
  `git add "$LOG"` a no-op, so the commit had nothing staged and failed.
  The whole point of the wrapper — logs that reach origin unattended —
  was defeated by a rule from before session logs existed.
- **Fix:** `git add -f "$LOG"` with a comment naming this entry. The
  stranded log from the failed session is committed alongside.

### FAM-BUG-032 — One legacy cycle row wedges every attempt at startup

- **Status:** Fixed (this commit)
- **Observed:** Session 3 on the Linux box passed preflight, claimed
  PRD-76, then failed instantly — before spawning the worker — with
  `execution history failed: database error: verification evidence
  requires repository identity`, terminating `unclassified_result`.
  Deterministic: every future attempt on this machine would fail the
  same way.
- **Root cause:** attempt start runs `recover_incomplete()`, which
  re-persisted every non-terminal cycle through `save_cycle`. This
  machine's database holds one August-era cycle (state
  `awaiting_review`, 8 verification-history entries) whose JSON predates
  `repository_key`; serde defaults the key to empty, and `save_cycle`'s
  evidence invariant (rightly) refuses keyless verification evidence.
  Recovery inherited an invariant meant for new evidence and turned one
  stale row into a permanent startup wedge — same defect class as
  FAM-BUG-016 (stale persisted state blocks all new work).
- **Fix:** recovery now marks cycles interrupted with a targeted UPDATE
  of the cycle row (state, disposition, cycle_json, ended_at) instead of
  a full `save_cycle`. This also stops recovery from wholesale
  rewriting evidence/finding tables it has no new information about.
  Regression: legacy keyless cycle with verification history recovers,
  existing evidence rows preserved, second recovery is a no-op.
- **Note:** no manual database surgery — the next session's recovery
  marks the stale row interrupted and moves on, which is the point.

### FAM-BUG-033 — Version-probe flake fails preflight (ETXTBSY class)

- **Status:** Fixed (this commit); watch for recurrence at other spawn sites
- **Observed:** Session 4's preflight failed `verification.tests-green-crates`
  on `executes_fake_claude_streaming_output_and_mapping_results`:
  `agent_version` was `None` while every other assertion in the same test —
  including the main spawn of the same fake executable — passed. Session 3
  ran the identical suite green in the identical Docker image: a flake,
  not a regression.
- **Probable cause:** the version probe is the first exec of the fake the
  test just wrote; under parallel tests a sibling's fork can still hold
  the script's write handle at exec time, and Linux refuses with ETXTBSY.
  The probe swallowed the spawn error into `None`.
- **Fix:** `probe_version` retries exec up to 5×10 ms on os error 26 only;
  all other spawn errors still conclude the executable is unavailable.
  If another fake-spawning test ever shows the same one-spawn-fails
  signature, generalize the retry to a shared spawn helper — narrowly
  fixed here first, per policy.
- **Cost note:** each such flake burns an entire drive session at
  preflight. Flakes in required verification checks are session killers
  and get fixed immediately, not waived.

### FAM-BUG-034 — One stray stdout line voided a finished $14 run

- **Status:** Fixed (this commit)
- **Observed:** Session 5's sonnet worker completed PRD-076 end-to-end —
  19 minutes, 69 turns, 65/65 test blocks green, valid single terminal
  result with full usage — and the adapter rejected it:
  `malformed_output`, attempt retained, session's PRD budget consumed.
  Cost: $14.16 of work discarded at the last step.
- **Root cause (two defects):** (1) any single non-JSON stdout line set
  a `malformed_seen` flag that voided the whole execution even when a
  valid single terminal was parsed — the CLI emitted one stray plain
  line during a long sub-agent session; (2) the rejection message
  conflated that case with duplicate terminals and named neither the
  count nor the line, and because forwarded anomalies are
  indistinguishable from echoed narration in the session log, the
  offending line is untraceable after the fact.
- **Fix:** the stream now counts unparseable lines and keeps a bounded
  sample of the first; a valid single terminal tolerates noise and
  surfaces `stream: tolerated N unparseable stdout line(s) ... first:
  "..."` in the output. Duplicate terminals remain a hard rejection
  (authority between results is genuinely ambiguous) and now report
  their count; EOF-before-result now includes the noise sample too.
- **Class note:** this is the overzealous-gate class — fail-closed
  belongs on ambiguity about durable facts, not on cosmetic stream
  noise. Codex adapter reviewed: its malformed handling is
  contract-distinct and was left untouched.

### FAM-BUG-035 — Phantom scope decisions for a fully approved candidate

- **Status:** Fixed (this commit)
- **Observed:** Session 6's scope evaluation approved all 45 changed
  files (25 `allowed_change`, 20 `justified_expected_file_change` — zero
  undecided), yet the drive printed 24 "scope decision pending" commands
  and enrolled them in the checkpoint's decision ledger, gating the
  candidate on human approvals the policy had already granted.
- **Fix:** only `prohibited_change`, `undeclared_scope_expansion`, and
  `ambiguous_human_review` findings enroll as pending decisions.

### FAM-BUG-036 — Review package self-defeats on large refactors

- **Status:** Fixed (this commit); root cause of session 6's
  `evidence_failure`
- **Observed:** Session 6's completed PRD-076 candidate ($24.79, 51 min)
  reached packaging and died with `EvidenceFailure` → human review, with
  no detail (the coordinator swallowed the error). Probing the retained
  worktree reproduced it: `RequiredEvidenceOverBudget` for a 470KB diff
  against the 250KB/60k-token budget — which the omission machinery
  exists to handle.
- **Root causes (three):** (1) greedy file-order packing disclosed the
  195KB whole-file `config.rs` deletion hunk first, starving 53
  substantive hunks; (2) each omitted hunk appends a ~530-byte duplicate
  retained-ref to the manifest, so heavy omission pushed even a
  one-hunk package over the token ceiling, and the final render check
  hard-failed instead of shedding load — the packer defeated by its own
  bookkeeping; (3) `RequiredEvidenceOverBudget` also masked the
  unrelated base-revision-mismatch condition, and three coordinator
  `Err(_)` arms discarded error detail entirely.
- **Fix:** hunks pack smallest-first (document order preserved in the
  disclosed diff), the complete rendered request is the fit arbiter
  with largest-hunk eviction as backstop, base mismatch got its own
  named error, and every coordinator evidence arm now prints its error.
  The retained session-6 candidate packages at 225,899 bytes with 2
  omissions (56 of 58 hunks disclosed) under the unchanged budget.
- **Cost note:** sessions 5 and 6 together spent ~$39 on completed
  implementations voided by gate defects (034, 036). Both defects were
  in machinery, not the work; both now carry regressions.

### FAM-BUG-037 — Every re-freeze over an existing checkpoint FK-fails

- **Status:** Fixed (this commit)
- **Observed:** Session 7's worker completed PRD-076 (third green
  implementation) and died at freeze: `checkpoint_failed`,
  `FOREIGN KEY constraint failed`. The checkpoint upsert keeps the
  existing row's checkpoint_id on conflict (`UNIQUE(repository_key,
  prd_id)`, id not in the update set) — but the created-event insert
  cited the superseding attempt's fresh id, which has no parent row.
  First trigger: session 7 was the first freeze over a live prior
  checkpoint (session 6's blocked one).
- **Fix:** `put` resolves the surviving checkpoint_id inside the
  transaction and cites it in the event; the (repository, prd)
  checkpoint identity is durable across attempts, which is also what
  keeps scope_decisions' FK references and PRD-080's durable human
  decisions coherent. On supersede, pending (undecided) scope rows for
  a different candidate_hash are retired — a superseding candidate
  re-derives its own findings — while decided rows remain as audit.
  This also retires session 6's 45 phantom pending rows (FAM-BUG-035
  artifacts) automatically at session 8's freeze; no manual surgery.
- **Regression:** refreeze over a checkpoint with pending + decided
  scope rows succeeds, keeps the durable id, updates the candidate,
  retires the stale pending row, preserves the human decision.

### FAM-BUG-038 — Interim terminal events voided a fourth completed run

- **Status:** Fixed (this commit); supersedes FAM-BUG-034's
  duplicate-terminal hard rejection
- **Observed:** Session 8's worker completed PRD-076 (fourth green
  implementation, 12.7 min) and the adapter rejected the stream:
  "1 duplicate terminal event(s)". Live evidence overturned 034's
  assumption that duplicates are ambiguous: both results shared the
  main session_id; the FIRST was a degenerate interim (2,578 in /
  18 out tokens — sub-agent session artifact), the LAST was the real
  final ($28.70, 66 turns). First-kept chose the wrong authority AND
  rejected; the forensics were also inverted (the kept result was
  silent, the discarded final was logged as the anomaly).
- **Fix:** the stream's last result event is the terminal, per the
  CLI's own contract that the stream ends with its result. Later
  results supersede earlier captures (unconditional overwrite),
  interim count is surfaced as a warning line, never fatal. EOF
  without any result stays fatal.

### FAM-BUG-039 — Durable checkpoint identity replays event ids

- **Status:** Fixed (this commit); collateral of the FAM-BUG-037 fix
- **Observed:** Session 9 passed freeze (037 validated live), passed
  format/lint/tests-green-crates verification against the candidate,
  then died advancing the checkpoint: `UNIQUE constraint failed:
  execution_checkpoint_events.event_id`. Transition event ids were
  `{checkpoint_id}:{phase}` — unique per checkpoint lifetime, and 037
  made checkpoint identity span attempts, so a later attempt revisiting
  a phase replays the id (session 6's lifecycle already wrote it).
- **Fix:** transition and approval event ids carry a per-checkpoint
  sequence (`{checkpoint_id}:{phase}:{n}`) — unique per occurrence.
  The two drive-side completion writers already used INSERT OR IGNORE
  and stay as-is.

### FAM-BUG-040 — Self-amending PRDs could never complete

- **Status:** Fixed (this commit); the fix landed PRD-076
- **Observed:** Resume attempts 3-4 reached a clean opus review
  (ReadyForHumanApproval, CleanReview, independence verified) and then
  failed the completion transition with the absurd "expected
  in_progress, found in_progress". Instrumentation (predicate now named
  in the error, hashes printed) revealed a content-hash conflict: the
  row held the claim-time hash of main's PRD-076.md while the
  completion target hashed the WORKTREE's copy — which PRD-076 amends
  by design.
- **Root cause:** `resume` passed the candidate worktree as the
  repository root to `resume_implemented_checkpoint`, conflating
  repository identity (backlog discovery, completion target) with
  candidate content (context, verification, capture). The function
  already reads candidate content from the checkpoint's own worktree;
  identity now comes from the primary checkout.
- **Validated live:** the next resume completed PRD-76 and landed it —
  `landed PRD-76 9ca865a`. Follow-up: a fake-agent regression for the
  self-amending-PRD shape belongs in autonomous_delivery.rs.

### FAM-FRICTION-006 — Unattended report pushes race origin and strand

- **Status:** Fixed (this commit)
- **Observed:** the Mac's first mac-build-speed report (the one that
  validated the FAM-BUG-030 fix) failed to push: origin had advanced
  during its 10-minute run. Same latent race in logged.sh and
  diagnose-suite-hang.sh.
- **Fix:** all three scripts commit first, then `git pull --rebase
  --autostash` before pushing, with commit and push failures reported
  distinctly.

### FAM-BUG-041 — Scope approval stamped `reviewed` on never-reviewed candidates

- **Status:** Fixed (this commit)
- **Observed:** After the owner approved wave 5's four scope findings,
  the final-approval path in `decide_scope` set both checkpoints to
  phase `reviewed` — but scope pauses fire BEFORE independent review
  (both cycles: `scope_ambiguous`, no review_result). The phantom phase
  wedged resume ("checkpoint phase reviewed cannot start review") and
  let the CLI's completion continuation fire on unreviewed cycles,
  surfacing as the opaque "persisted review cycle columns are
  inconsistent". The decisions themselves recorded correctly.
- **Fix:** full approval re-opens the pipeline at `implemented` —
  verification and review are still owed and resume re-enters from
  there; rejections still block. Pin updated. The two live checkpoints
  were repaired through the audited transition API
  (`operator_set_phase` example, event trail kept).
- **Also noted:** the drive's printed scope-decisions template omits
  `--finding-hash` (not paste-runnable), and PRD-53's completed
  integration commit stays on the session branch under disabled
  delivery — operator-merged to main this session. Both queued as
  polish.

### FAM-BUG-042 — Remediation vandalized a coherent candidate on a misinformed scope claim

- **Status:** Fixed (this commit)
- **Observed:** Opus reviewed PRD-38's candidate and alleged a scope
  violation for `docs/` and `tests/fixtures/` paths — paths the scope
  policy had already adjudicated as justified expected-file expansions
  (the reviewer's package carries no scope-authority context).
  `scope_violation` is a default blocking category, so policy recomputed
  the reviewer's `blocking:false` into blocking, remediation ran, and
  the remediation agent `git mv`-ed the declared fixtures and acceptance
  doc into `crates/` — moving a coherent candidate's files OUT of their
  declared scope. The post-remediation capture then flagged the new
  paths ambiguous and wedged the cycle. Recovered by resetting the
  worktree to the frozen implementation commit and rebinding.
- **Fix:** the scope policy engine is the single authority on scope. A
  reviewer scope-violation claim whose evidenced paths this cycle's own
  evaluation adjudicated (allowed or justified) is downgraded from the
  blocking set; claims citing unadjudicated paths still block. Package-
  level scope-summary disclosure for reviewers is the follow-up so the
  claim isn't made in the first place.

### FAM-BUG-043 — Docker test image has no git identity

- **Status:** Fixed (this commit)
- **Observed:** every git-exercising test fails in the tester image
  with "fatal: unable to auto-detect email address" — the drive's merge
  queue commits during integration. This single gap is why
  tests-workspace-advisory was permanently red in Docker (898 tests
  pass around it), why PRD-38's acceptance proof had no passing
  verification (opus's finding), and part of the historical "seven
  known pre-existing Docker failures".
- **Fix:** tester stage configures a git identity and default branch.
  If the advisory check goes green in Docker, promoting it to required
  closes opus's acceptance-verification gap properly.

### FAM-BUG-044 — Waivers key on reviewer-chosen finding ids, which rotate

- **Status:** FIXED 2026-09-01 evening; **strengthened 2026-09-02** —
  waivers store the claim substance (migration 056 also drops the FK that
  blocked save_cycle's findings rewrite), completion matches by id or
  substance, and the coordinator carries durable waivers forward into each
  fresh attempt's snapshot.
  **Second iteration (wave 6, PRD-59):** an exact-set substance hash was
  still too strict — the reviewer re-issued the same scope claim as
  `F9-…` then `F5-…` citing a DIFFERENT SUBSET of paths each time, so the
  hashes differed and completion refused a waiver the owner had granted.
  Substance is now stored as data (category + cited paths/checks) and a
  waiver covers a later finding when the category matches and its
  citations are a subset of what the human actually saw. A claim citing
  anything new still blocks. Regression pins covered/subset/superset/
  wrong-category.
- **Original status:** Open (workaround: manual completion override, used
  for PRD-38)
- **Observed:** opus issued the same misinformed scope claim under a
  different finding_id on every attempt (`scope-out-of-allowed-paths`,
  then `scope-outside-crates`), so the durable human waiver never
  matched the newest attempt's open finding and completion-evidence kept
  refusing. Additionally the waiver row FK-blocked `save_cycle`'s
  findings rewrite on the next attempt ("persistence failed: FOREIGN
  KEY constraint failed") — waivers must survive finding replacement.
- **Also in this class:** the review package should carry the scope
  engine's adjudication so the reviewer stops alleging violations for
  declared Expected Files (three attempts, three re-claims). PRD-38 was
  completed by the designed human-only manual override with a full
  audit reason after three clean reviews and fully green verification.

### FAM-FRICTION-007 — Scope-decisions pause template is not paste-runnable

- **Status:** Fixed 2026-09-01 evening — template prints `--finding-hash`
  and `--approve`.
- **Original status:** Open
- The drive prints `familiar-ai scope-decisions sha256:… --candidate-hash …`
  but the CLI requires `--finding-hash`; the printed command fails with
  an argument error.

### FAM-FRICTION-008 — waive_finding has no CLI surface

- **Status:** Fixed 2026-09-01 evening — `familiar-ai waive --cycle-id …
  --finding-id … --actor human:… --reason …`.
- **Original status:** Open (interim: `operator_waive` example)
- Completion-evidence demands durable human waivers, but no CLI can
  create one; the examples/operator_waive tool is the stopgap.

### FAM-BUG-045 — Durable approvals ignored on the scope-Broadened path

- **Status:** Fixed (this commit)
- **Observed:** wave 6's PRD-59 stopped `scope_broadened` after the owner
  had approved all seven of its findings, including the two
  `undeclared_scope_expansion` ones (`config/default.toml`,
  `docs/contracts/providers-index.md`). The absorption added earlier
  guarded only the `HumanReviewRequired` disposition; `Broadened` — the
  disposition undeclared expansions produce — never consulted approvals,
  so a fully approved candidate still halted.
- **Fix:** both `Broadened` arms (initial and post-remediation) now take
  the same absorption guard, which already treats
  `UndeclaredScopeExpansion` findings as absorbable when durably
  approved. Prohibited changes remain fatal on every path.

### FAM-BUG-046 — Tool results carry no capability name, so wire formats synthesize invalid calls

- **Status:** Fixed (this commit); found by opus reviewing PRD-61
- **Observed:** OpenAI-compatible wire formats require the assistant's
  originating tool call to precede each tool result. PRD-61's first
  remediation reconstructed that entry at the serialization layer from
  the following results — but `ToolResultPayload` carries only
  `call_id`, so every synthesized entry had `"name": ""`, which the
  provider rejects. The reviewer caught both the missing entry (cycle 2)
  and the empty name its fix produced (cycle 3); no fixture caught
  either, because every mock matched method and path only.
- **Root cause:** the shared `MessageContent::ToolResult` contract
  (PRD-058) omitted the capability identity that OpenAI-shaped wire
  formats need to rebuild a transcript.
- **Fix:** `ToolResultPayload` carries `capability_name`, populated at
  every construction site in the raw runtime (validated capability, or
  the model's raw requested name on the validation-refusal path).
  PRD-060's landed `openai_api` adapter — which solved this correctly
  with a per-call stream cache — now uses the field as its cache-miss
  fallback instead of silently omitting the item (fresh adapter or
  resumed transcript); regression pinned.
- **Reviewer credit:** three independent reviews found a real protocol
  defect, its remediation's follow-on defect, and the test blindness
  that let both pass. This is the review loop paying for itself.

### FAM-BUG-047 — Intermittent workspace test failure

- **Status:** Fixed 2026-09-03 (reopened 2026-09-02 after an incorrect
  closure) — I closed this wrongly. My
  sixteen "clean" runs were all standalone; the failure only appears under
  the Docker gate's contended parallel load, which I never reproduced. It
  then failed PRD-063's verification for real. The offender is
  `daemon_starts_and_stops_on_sigterm`: it allowed 5 seconds for a COLD
  daemon start that opens SQLite and applies every migration (48 and
  growing) while the whole workspace suite runs beside it. Budget raised to
  30s with the reasoning recorded in the test. **Lesson:** "not
  reproducible" needs the conditions that produced it — standalone runs do
  not falsify a load-dependent flake, and closing on them cost a session.
- **Prior (incorrect) closure:** CLOSED 2026-09-02 — not reproducible; the original sighting
  was operator measurement error. Sixteen clean consecutive runs (three
  ad-hoc plus a 10-run hunt with nothing else touching `target/`), zero
  failures. Two causes for the false alarm, both worth remembering:
  (1) the suite legitimately PRINTS `FAILED` and `panicked at` as
  fake-agent fixture content, so `grep -c FAILED` is a meaningless check
  — only the exit code is authoritative; (2) the failing observations
  happened while a second `cargo test` ran concurrently against the same
  `target/`, which is a build-contention artifact, not a test defect.
  Reopen with a captured failing test name if it recurs.
- **Original status:** Open — needs identification
- **Observed:** during wave 6, `cargo test --workspace --no-default-features`
  reported one failure, then passed twice on identical trees (both on main
  and in the PRD-59 worktree). The failing test was not captured.
- **Why it matters:** `tests-workspace-advisory` is now a REQUIRED check.
  A flake in a required gate kills a whole unattended session, exactly as
  FAM-BUG-033 did. Priority is disproportionate to its apparent size.
- **Next action:** run the suite in a loop capturing failures
  (`for i in $(seq 20); do cargo test --workspace --no-default-features
  2>&1 | grep -A3 'FAILED'; done`), then fix or serialize the offender.

### FAM-BUG-048 — Operator tools bypass the control-plane lock

- **Status:** FIXED 2026-09-03 — `worker_lock::refuse_while_driver_owns` is the read-only counterpart to the claim the drive respects, and all three `operator_*` tools call it before touching durable state: they exit 2 naming the live owner pid rather than mutating checkpoints under a running session. An unreadable claim also refuses rather than proceeding on a guess.
- **Original status:** Open
- **Observed:** on 2026-09-02 the operator wrote checkpoint rows
  (`operator_rebind`, `operator_set_phase`) and rebased candidate
  worktrees while a `resume all` session held the control-plane claim.
  The lock refused the second *session* launch — correctly — but the
  example tools write to SQLite directly and are subject to no such
  check, so nothing prevented mutation underneath live work.
- **Impact:** none confirmed this time (the session finished and both
  candidates landed), but the class is checkpoint/worktree corruption
  under a running driver.
- **Fix direction:** the `operator_*` examples should acquire (or at
  minimum assert absence of) the repository control-plane claim before
  mutating, and refuse with the owning pid when it is live — the same
  courtesy the drive extends to itself.

### FAM-BUG-049 — Agent-loop history omits the assistant's tool-call turn

- **Status:** FIXED 2026-09-02 — `MessageContent::ToolCalls(Vec<ToolCallPayload>)`
  carries call_id, capability, and verbatim arguments; the raw runtime pushes
  the assistant's call turn into history immediately before the results that
  answer it. All three adapters consume it: xAI serializes it directly and
  skips its synthesis path, the OpenAI Responses adapter emits the recorded
  `function_call` items and suppresses its cache/fallback for those ids, and
  the Anthropic adapter seeds its `tool_use_registry` from the turn so
  reconstructed blocks carry real names AND arguments instead of the
  empty-input fallback. Regression pins that a recorded turn is serialized
  verbatim with no synthesized duplicate. FAM-BUG-046's `capability_name` is
  now belt-and-braces for transcripts that predate this.
- **Original status:** Open — root cause behind FAM-BUG-046 and three adapter defects
- **Observed:** `Message` history records tool RESULTS
  (`MessageContent::ToolResult`) but never the assistant turn that
  ISSUED the calls. Every wire format requires the originating call to
  accompany its result, so each adapter reconstructs that turn on its
  own — and each got it wrong differently: `openai_api` silently omits
  the item when its stream cache misses, `xai_api` emitted an empty
  `function.name`, `anthropic` emits an orphan `tool_result` on the
  refusal path. All three were caught by independent review, none by a
  fixture.
- **Fix direction (proposed by PRD-61's reviewer):** add
  `MessageContent::ToolCalls(Vec<ToolCallPayload>)` carrying call_id,
  capability, and arguments, and have the raw runtime push the
  assistant's call turn into history. Adapters then SERIALIZE a complete
  transcript instead of inventing one; FAM-BUG-046's `capability_name`
  becomes belt-and-braces rather than the only signal.
- **Urgency:** PRD-072 adds another runtime; every future provider hits
  the same wall. Fix before it lands.

### FAM-BUG-050 — Daemon shutdown is unbounded and unsignalled before readiness

- **Status:** Fixed 2026-09-21 — the fourth layer no longer reproduces.
  `daemon_starts_and_stops_on_sigterm` passed three consecutive times under
  the full contended load of `tests-green-crates` in Docker, which is the
  condition that produced it. FAM-BUG-047's lesson was applied deliberately:
  standalone runs do not falsify a load-dependent failure, and that entry was
  closed wrongly once on sixteen of them. These were loaded runs.

  **The gate's only blind spot is gone with it.** `--skip
  daemon_starts_and_stops_on_sigterm` has been in the required check since
  2026-09-03, tied by name to this entry; it is removed, so the daemon crate
  is now gated in full.
- **Prior status:** Open — three layers fixed, one remaining
- **Found:** while diagnosing PRD-063's verification failure, which turned
  out to be innocent: `daemon_starts_and_stops_on_sigterm` fails in Docker
  on `main` too. Each fix revealed the next layer, and each is a real
  product defect, not a test artifact:
  1. **FIXED — SIGTERM had default disposition during startup.** The
     handler was registered inside `shutdown_signal()`, which is not
     awaited until the runtime is fully assembled — long after the PID
     file is written. A supervisor that read the PID file and signalled
     (systemd, launchd, an operator) killed the daemon outright: no
     graceful shutdown, no PID cleanup, no in-flight work finished.
     `TerminationSignals::register()` now runs BEFORE bootstrap writes the
     PID file, so that file means "ready, including to stop".
  2. **FIXED — worst-case graceful shutdown was 15s.** Three independent
     subsystems were drained sequentially at 5s each, overrunning any
     supervisor TERM budget for no reason; nothing depended on anything
     else finishing. They now drain concurrently, bounding shutdown at one
     timeout.
  3. **FIXED — runtime teardown was unbounded.** Dropping a multi-thread
     tokio runtime waits for blocking tasks with no limit, so a parked
     `spawn_blocking` kept the process alive after every graceful step had
     completed. Teardown is now bounded.
  4. **OPEN — the daemon still does not exit within 10s of SIGTERM in
     Docker.** It starts correctly (the PID file appears) and then does
     not stop.
     **Dead end, recorded so it is not repeated:** dumping the child's
     STDERR does not show the shutdown. `bootstrap()` installs a
     `LogGuard` that redirects tracing to a rolling file in the state
     directory, so stderr stops at `applied database migrations` by
     design — everything after (PID file written, socket bound, received
     SIGTERM) goes to the log file. The next investigation should read
     `<state_dir>/` logs from the test's temp directory, not the pipe.
     The stderr dumps added to both failure paths still beat "PID file
     was not created", so they stay.
- **Coverage repair 2026-09-03:** demoting the advisory left NOTHING
  required exercising the daemon crate — an independent review of PRD-071
  caught that. `tests-green-crates` now includes `-p familiar-ai-daemon`
  with `--skip daemon_starts_and_stops_on_sigterm`: the crate is gated
  again and exactly one test is excluded by name, tied to this entry.
  Verified: 51 suites green in Docker.
- **Gate impact (my error, recorded):** I promoted
  `tests-workspace-advisory` to REQUIRED on 2026-09-01 while it contained
  this Docker-failing test. A required gate that is red on main blocks
  every landing, so it is demoted to advisory until this test is green,
  then re-promoted. The bug stays open; the demotion is not a waiver.

### FAM-BUG-051 — A directory in `expected_files` grants unchecked scope authority

- **Status:** Fixed 2026-09-21. The root cause named below — overlap decided
  on equality rather than containment — is gone: `scope_entries_overlap` in
  `drive.rs` now matches `Directory` against `ExactFile` and `Directory`
  against `Directory` by prefix, so a PRD declaring `crates/` conflicts with
  everything beneath it instead of appearing disjoint. Only PRD-092 still
  carries a bare directory (`scripts/`), and it is now correctly read as the
  prefix claim it is.

  The narrow harm is closed separately, because the scheduler deliberately
  exempts `crates/familiar-ai-storage/migrations/` from overlap detection
  (PRD-066 allocates those numbers instead), so containment alone would not
  catch it. `migration_allocation.rs` now fails the build when two queued
  PRDs claim the same migration number, as well as when one claims a number
  already applied. Verified by pointing PRD-086 at PRD-085'"'"'s number and
  watching it fail with `066 claimed by PRD-085.md, PRD-086.md`.
- **Prior status:** Open — mitigated for the current wave, root cause unfixed
- **Found:** 2026-09-05, planning the audit-remediation wave. Eight PRDs
  declared `crates/familiar-ai-storage/migrations/` — a directory, not a
  file — and `achievable_width()` reported them as mutually disjoint.
- **Reproduction (the scheduler answering its own question):**

  ```
  $ compute_width PRD-85 PRD-91     # their ONLY shared expected_file is migrations/
  graph_width=2 achievable_width=2
  conflicts: none — every pair is scope-disjoint
  ```

- **Why it matters, in two ways:**
  1. **The narrow harm.** Next free migration is `059`. Admitting five
     migration-claimants simultaneously means five PRDs each create
     `059_*.sql` and collide at merge. This is exactly the PRD-71/72
     collision on `057` that cost a session, except five ways — and the
     scheduler reports zero conflicts on the way in, so nothing warns.
  2. **The real defect.** Overlap detection compares `expected_files`
     entries without treating a trailing-slash entry as a prefix claim.
     A PRD declaring `crates/` therefore holds authority over the whole
     workspace while appearing disjoint from everything. Scope policy is
     the mechanism the entire unattended-safety argument rests on; an
     entry form that silently disables overlap checking is a hole in that
     mechanism, not a migration-numbering nuisance.
- **Mitigation applied:** PRD-085/086/087/091/093 now declare concrete
  filenames (`migrations/059_autonomy_measurement.sql` … `063_…`), so the
  scheduler sees distinct files and the numbers cannot collide.
  PRD-063/071/073 still carry directory entries and were left alone —
  071 is already integrated and 063 has an in-flight candidate whose
  migration would no longer match a rewritten declaration.
- **Fix (not yet written):** decide overlap on path *containment* rather
  than equality, so a directory entry conflicts with every entry beneath
  it, and either reject directory entries at admission or record them as
  the prefix claims they are. Deserves its own PRD; PRD-093 (admission
  quality) is the natural home.

### FAM-FRICTION-009 — Preflight runs every gate before reporting the first failure

- **Status:** Open
- **Found:** 2026-09-04, driving PRD-081. `verification.format` failed at
  5.2s; preflight then ran `verification.lint` (34s) and
  `verification.tests-green-crates` (170s) anyway, and only afterwards
  reported `session preflight failed: verification.format`.
- **Cost:** ~205 seconds burned after the answer was already known, with
  `attempted=0 completed=0` — no work was ever going to start. Only the
  one failure was reported, so this is not collect-all-failures behavior
  that trades time for a fuller diagnosis; the later gates' results were
  discarded.
- **Fix:** stop at the first failing preflight gate, or report each
  failure as it happens so the operator can act while the rest run. In an
  overnight session this is 3.5 minutes per stumble, and a stumble as
  small as an unformatted file is enough to trigger it.

### FAM-BUG-052 — A nested `target/` escapes the root ignore and destroys review evidence

- **Status:** Fixed (ignore rule); diagnostic still poor
- **Found:** 2026-09-05, resuming PRD-92. The review died with:

  ```
  review: diff capture failed: diff contains 442136446 bytes,
      exceeding evidence limit 4000000
  Review disposition: HumanReviewRequired; stop reasons: [EvidenceFailure]
  ```

- **Cause:** `.gitignore` line 5 was `/target/`. The leading slash anchors
  the rule to the repository root, so `crates/familiar-ai-tray/target/`
  was never ignored. That crate is **workspace-excluded** (`Cargo.toml`
  `exclude = ["crates/familiar-ai-tray"]`), so building it *always*
  produces its own nested `target/` — 367MB of it here.
- **Why it was certain to fire:** PRD-92's whole job was adding a
  `build.rs` to the tray crate. Testing a build script means building
  that crate, in that directory. The task and the landmine were the same
  action.
- **Blast radius:** any PRD whose implementer runs cargo inside a crate
  directory poisons its own review diff, and the failure arrives at the
  *end* — after implementation and verification have already been paid
  for. PRD-92 burned a full resume cycle on it.
- **Fix:** added a bare `target/` rule, which matches at any depth. The
  root-anchored `/target/` is left in place; it is now redundant but
  harmless.
- **Still open — the diagnostic.** The error reports only a byte count.
  It should name the largest contributing paths, because "442MB" gives an
  operator nothing to act on while "crates/familiar-ai-tray/target/ —
  367MB" is self-explanatory. Filed as FAM-FRICTION-010.

### FAM-FRICTION-010 — Evidence-limit failures report a byte count and nothing else

- **Status:** Open
- **Found:** 2026-09-05, alongside FAM-BUG-052.
- **Detail:** `diff contains 442136446 bytes, exceeding evidence limit
  4000000` names the symptom and withholds every fact needed to fix it.
  Diagnosing it took a `du` sweep of the candidate worktree.
- **Fix:** when the diff exceeds the budget, list the top few paths by
  contributed bytes. The information is already in hand at the point the
  check fails.

### FAM-BUG-053 — RETRACTED: scope approvals are not voided by a rebind

- **Status:** Closed (retracted) 2026-09-05, same day. The diagnosis was wrong.
- **What I claimed:** that `scope_decisions` rows are keyed by
  `(finding_hash, candidate_hash)` and that `operator_rebind` therefore
  silently invalidates every approval on the candidate.
- **What is actually true:** `approved_scope_findings` queries
  `WHERE repository_key=?1 AND decision='approved'` — there is no
  `candidate_hash` predicate — and it keys each row by
  `scope_finding_substance_hash`, which blanks `policy_snapshot_hash`
  before hashing precisely so unrelated landings cannot orphan a human
  decision (PRD-080). Approvals survive both rebinds and policy rotation.
- **What actually happened:** `human_review_absorbed` is all-or-nothing —

  ```rust
  evaluation.findings.iter().all(|f| match f.decision {
      ProhibitedChange => false,
      AmbiguousHumanReview | UndeclaredScopeExpansion =>
          approved.contains(&scope_finding_substance_hash(f)),
      AllowedChange | JustifiedExpectedFileChange => true,
  })
  ```

  A third finding appeared that the owner had never approved (an
  `AmbiguousHumanReview` on `crates/familiar-ai-tray/Cargo.lock`, itself
  an artifact of the in-crate build behind FAM-BUG-052). One unapproved
  finding makes `.all()` false, so the attempt stopped and the printout
  re-listed every finding — including the two already decided.
- **How I got it wrong:** I read the scope *evaluation* printout, which
  lists every changed path and its disposition, as a list of undecided
  findings. Then I compared the approval rows' `candidate_hash` to the
  post-rebind `diff_hash`, saw they differed, and treated a coincidence
  as the cause without reading the lookup query. Two minutes of reading
  `approved_scope_findings` would have refuted it.
- **The real defect, filed as FAM-FRICTION-011:** nothing in the output
  distinguishes an already-approved finding from a pending one, so a
  single new finding looks identical to every prior decision being lost.

### FAM-FRICTION-011 — Scope output cannot distinguish decided findings from pending ones

- **Status:** Open
- **Found:** 2026-09-05, while misdiagnosing FAM-BUG-053.
- **Detail:** When absorption fails, the evaluation prints every finding
  with its disposition and no decision state. An operator who approved
  two findings and then sees three listed has no way to tell whether one
  is new or all three came back. It cost a wrong bug report, a wrong fix
  plan, and a retraction.
- **Fix:** mark each finding with whether a durable approval already
  covers it, and when absorption fails say which findings blocked it —
  "1 of 3 findings is undecided: crates/familiar-ai-tray/Cargo.lock"
  rather than reprinting all three identically.


### FAM-BUG-054 — A failing required check dead-ends; remediation is unreachable

- **Status:** Fixed 2026-09-20 by PRD-096, integrated by Familiar's own
  merge queue in `5ce0755`/`f182492`. `coordinator.rs` now synthesises a
  blocking finding from the failing check — `synthesize_check_failure_finding`
  at line 1330 — and populates `RemediationRequest::verification_failures`
  with it, instead of stopping at `VerificationUnsuccessful` before any
  review runs. The field existed and was passed `vec![]` at every call site,
  which is exactly what this entry described. 80 review-crate tests green.
- **Prior status:** Open
- **Found:** 2026-09-06, driving PRD-63 with a hand-written failing test.
- **Detail:** When a required verification check fails, the coordinator
  stops before the reviewer runs:

  ```rust
  // coordinator.rs:305
  } else {
      ReviewStopReason::VerificationUnsuccessful
  };
  return self.stop(cycle, reason);
  ```

  Remediation is only ever invoked on findings a *review* produced. A
  review never happens, so the implementer is never asked to fix the
  failing test. The attended pause then offers `[r]etry remediation`,
  which calls `resume_implemented_checkpoint`, which re-runs the same
  verification, which fails identically, and returns to the same prompt.
- **Observed:** the owner pressed `r` repeatedly on PRD-63 with the
  implementation provably unchanged between attempts (worktree HEAD
  static, `acquire_with_unknown_capacity_policy` byte-identical). The
  only forward options were to accept a known-bad landing or stop.
- **Why it matters:** a red required test is the most common way work is
  unfinished, and it is precisely the case the loop cannot act on. The
  system can remediate a reviewer's *opinion* but not a compiler's or a
  test runner's *fact* — which inverts the "Determinism Before
  Intelligence" principle the verification gate exists to serve.
- **Consequence for the oracle pattern:** PRD-081 succeeded because its
  tests were `#[ignore]`d, letting verification pass so review and
  remediation could run. That attribute was load-bearing and I had
  recorded it as housekeeping. A failing-by-design test is not a work
  item to this architecture; it is a wall.
- **Fix:** route a failed required check into remediation the same way a
  blocking review finding is routed — synthesise a finding carrying the
  check id and the captured failure, hand it to the implementer, and
  re-verify. Failing that, the pause must not offer `[r]` when it cannot
  change the outcome.
- **Queued as:** PRD-096 (`docs/prds/PRD-096.md`, status `draft`, awaiting
  owner approval). The routing fix is mostly wiring: `RemediationRequest`
  already carries a `verification_failures` field that every call site
  passes `vec![]`, and the remediation loop already bounds attempts and
  reserves budget. One open design question is carried in the PRD — whether
  a failed required check deserves its own `FindingCategory`.

### FAM-BUG-055 — A hard link walks straight out of the worktree

- **Status:** FIXED 2026-09-16 — committed in `9debdcc`
- **Found:** 2026-09-16, adversarially reviewing PR #7 (PRD-081) before
  merge. Confirmed by probe against the merged tree, both directions.
- **Detail:** PRD-081 decided containment on the *resolved* path, which
  closes the symlink escape completely and the hard-link escape not at
  all. A hard link is not a symlink — the link *is* the file — so
  `canonicalize` returns the in-worktree path and `symlink_metadata`
  reports an ordinary regular file. Every check PRD-081 installed passes
  it.

  ```
  read-file  {"path": "innocent.txt"} -> "PRIVATE KEY BYTES"
  apply-edit {"path": "innocent.txt"} -> a file outside the worktree
                                        now reads "OVERWRITTEN"
  ```

- **Why it matters:** identical in class and severity to the hole PRD-081
  was written to close. The write side turns `apply-edit` into an
  arbitrary-file overwrite for anything the daemon's uid owns, which is
  the blast-radius assumption scope policy, review gates and the merge
  queue all rest on. `fs.protected_hardlinks` is not a mitigation: it
  refuses only to link a file the caller does not own, and the daemon's
  uid owns its own `~/.ssh`. `ln` without `-s` is a hard link, and `ln`
  is the command the runtime's own containment test allowlists.
- **Fix:** refuse a regular file with `nlink > 1` at the chokepoint.
  `nlink == 1` *proves* the file has one name and that name is the
  resolved, contained one; `nlink > 1` means other names exist and no
  portable syscall enumerates them, so containment is undecidable rather
  than unchecked. Undecidable fails closed. This also refuses a hard link
  whose every name is inside the worktree — deliberate, pinned by its own
  test, and costless because git cannot represent a hard link at all.
- **Note:** the merged PRD-081 contract text asserted containment more
  confidently than the code earned. `docs/contracts/agent-loop.md` now
  names this refusal alongside the residual TOCTOU window.

### FAM-BUG-056 — `search-list` aborts the daemon on a deep tree

- **Status:** FIXED 2026-09-16 — committed in `05f44f6`
- **Found:** 2026-09-16, same review as FAM-BUG-055.
- **Detail:** `collect_matches` recurses once per directory level with no
  depth bound. `limit` only short-circuits once *matches* accumulate, so
  a query matching nothing descends to the bottom of whatever tree
  exists. A Rust stack overflow aborts rather than unwinding:

  ```
  search-list {"query": "zzz-matches-nothing"}
  -> thread has overflowed its stack
  -> fatal runtime error: stack overflow, aborting  (signal 6, SIGABRT)
  ```

- **Why it matters:** `mkdir -p` plus one `search-list` is the whole
  exploit and both are capabilities a worker already has. It is a crash,
  not a slow search — uncatchable, and it takes the daemon with it.
- **Consequence for the record:** PRD-081's commit message claimed the
  no-traverse rule "bounds the walk". It bounds *cycles*; depth was
  still open. A claim in a commit message is an assertion, not evidence.
- **Fix:** `MAX_WALK_DEPTH = 64` caps the descent — deep enough that no
  real checkout reaches it, shallow enough that recursion cannot exhaust
  the stack. Pinned by a regression that builds a 2000-level tree.

### FAM-BUG-057 — Heartbeat regression races the wall clock

- **Status:** FIXED 2026-09-16 — committed in `359225d`
- **Found:** 2026-09-16, running the full workspace suite while verifying
  PRD-081. Failed once during `cargo test --workspace`, passed 6/6 when
  run in isolation.
- **Detail:** `progress_heartbeat_observes_latest_durable_phase_on_later_tick`
  raced a 25ms writer thread against a 75ms sleep
  (`drive.rs:2836-2856`). The margin holds on an idle machine and loses
  under full-suite parallel load.
- **Why it matters:** a flake in a required verification gate is a session
  killer — the same class as FAM-BUG-033, where one flaky probe burned an
  entire drive session at preflight. It is also the exact failure mode
  that makes a green suite untrustworthy as evidence.
- **Fix:** the test proves a later tick observes a phase written by a
  *different connection*; it never needed to prove that within some number
  of milliseconds. Joining the writer before asserting keeps the
  cross-connection visibility and removes every wall-clock assumption.
- **Evidence:** with the original code, raising the writer's delay to
  400ms — what CPU contention does — fails deterministically
  (`left: "preflight", right: "review_complete"`). With the join and that
  same 400ms delay still injected, it passes 8/8 and no longer sleeps.

### FAM-BUG-058 — Unreproduced `security_burn_in` failure under suite load

- **Status:** Open — observed once, not reproduced, cause unknown
- **Found:** 2026-09-16, same full-suite run as FAM-BUG-057.
- **Detail:** `familiar-ai-agent`'s `security_burn_in` target reported
  `3 passed; 1 failed` during a `cargo test --workspace` run. Which of the
  four failed was not captured before the run was superseded.
- **Not diagnosed:** the target passes 4/4 in isolation, and 6/6 under
  48-way synthetic CPU load on a 24-core box. The tests spawn shell
  fixtures under `timeout_ms: Some(2_000)`, so a load-induced timeout is
  plausible and so is the ETXTBSY class of FAM-BUG-033 — but neither is
  evidenced, and this entry deliberately stops short of naming a cause.
- **Next step:** capture the failing test name and its output the next
  time a workspace run reports it; do not "fix" it before then.
- **2026-09-21:** that capture now happens automatically. `scripts/gate.sh`
  records the names of failing tests in the verdict, so the next occurrence
  identifies itself instead of vanishing. This is how the XDG environment
  race was found the same day — it had flaked twice and never reproduced,
  because the test restored the variable it had deleted. 058 is still open
  and still undiagnosed; what changed is that it can now be caught rather
  than waited for.

### FAM-BUG-066 — The default tray surface was not verified on macOS

- **Status:** Fixed 2026-09-21 — native macOS release build and all-target checks pass
- **Found:** 2026-09-21, following the platform warning in `docs/prds/SITREP.md`.
- **Detail:** `familiar-ai-tray` enabled `muda`'s GTK feature in its common
  dependency declaration, while its GTK crates and window implementation were
  Linux-only. The Linux-only Xvfb shutdown integration and GTK visual-preview
  example were also reachable from macOS all-target builds. Operators therefore
  installed `--no-default-features` binaries and the shipped macOS menu-bar path
  was never part of the verified artifact. The first native build exposed the
  runtime defect hidden behind that build gap: macOS constructed its
  `NSStatusItem` and then slept in a plain Rust polling loop without pumping
  AppKit events, so the daemon stayed healthy while no icon was presented.
- **Fix:** the direct GTK feature is now declared only in the Linux dependency
  section; Linux keeps GTK windows and its Xvfb shutdown test. macOS builds and
  links the native AppKit menu-bar path, opens dashboard/configuration surfaces
  through the browser or editor, compiles a harmless stub for the GTK-only
  visual fixture, and explicitly pumps AppKit's event queue on the main thread.
  Regressions pin both the dependency boundary and native event dispatch.
- **UI follow-up:** The first running build exposed that macOS still omitted
  `Open Dashboard` and sent both Settings actions to the TOML file because the
  dashboard default was off. The dashboard now defaults on only for macOS,
  remains loopback-bound, and both Settings and Configure Local LLM open its
  graphical inference page. Explicit `dashboard.enabled` configuration still
  wins on every platform; Linux continues to use its native GTK windows.
- **Evidence:** the tray-enabled macOS release links; macOS all-target checking
  passes; all 104 tray unit tests pass; strict tray Clippy passes; and the Linux
  dependency graph still contains `gtk` and `muda/gtk`. Docker was unavailable
  on this Mac, so the existing Linux runtime/Xvfb test was preserved but not
  rerun locally.

### FAM-BUG-067 — The shipped graphical product is not cross-platform

- **Status:** In progress — PRD-104's Tauri shell is packaged and running on
  macOS; the real Linux graphical smoke gate and final GTK/default cutover are
  still required before closure.
- **Found:** 2026-09-21, during live macOS verification after FAM-BUG-066.
- **Detail:** Linux owns substantial in-process GTK windows, while macOS tray
  actions open the loopback dashboard in an external browser. Earlier macOS
  behavior opened raw TOML in an editor. The current browser route makes the
  actions graphical, but does not provide the native application windows or
  Linux feature parity the operator expected. AppKit cannot reuse GTK widgets,
  and maintaining independent GTK and AppKit screen implementations would
  preserve the drift that exposed this gap.
- **Required fix:** PRD-104 replaces both platform-specific presentation paths
  with one Tauri desktop process over a typed same-user daemon protocol. It
  keeps the daemon authoritative and independently alive, requires complete
  GTK behavior parity and real macOS/Linux packaged-app smoke gates, forbids
  browser/editor/Docker fallbacks as completion evidence, and removes GTK only
  after the cross-platform gates pass.
- **Evidence required to close:** the installed macOS and Linux artifacts show
  the cat tray icon and open Dashboard, Settings, and Configure Local LLM as
  application windows; the parity inventory, security boundary, lifecycle,
  packaging, and fresh-install tests in PRD-104 all pass.
- **2026-09-21 implementation evidence:** added the versioned typed operator
  protocol, bounded snapshot/event transport, idempotent Rust-side mutations,
  Tauri tray and singleton Dashboard/Settings/Local LLM windows, independent
  launchd/systemd definitions, and non-Docker packaging instructions. A
  `Familiar.app` bundle was built, ad-hoc signed, passed strict `codesign`
  verification, and ran under its independent launchd service. PID-based
  lifecycle checks proved that restarting the idle daemon left the desktop
  unchanged and restarting the desktop left the daemon unchanged. No screen,
  microphone, Accessibility, or UI-automation permission is used or required.
  GTK remains intentionally available pending the real Linux graphical smoke
  gate required by the fail-closed migration plan.
- **2026-09-21 parity correction:** the first Tauri presentation was a thin,
  generic table/form shell and did not faithfully carry forward the mature GTK
  information architecture. The desktop now serializes and consumes the same
  tested `familiar-ai-tray::view` models used by GTK for inference health,
  gates, dependency blockers, progress, rounds, executions, sessions, attempt
  and review detail, budgets, configuration sections, and Gantt data. Its
  controls are contextual rather than requiring operators to type internal
  PRD, execution, or session identifiers. A desktop contract test pins that
  shared-builder inventory so the two presentations cannot silently drift
  again while GTK remains the migration oracle.

### FAM-BUG-068 — Desktop installation can leave two tray owners active

- **Status:** Open — mitigated 2026-09-22; durable installer guard still required.
- **Found:** 2026-09-22, when `Open Dashboard` opened the legacy loopback web
  page after the Tauri desktop had been installed.
- **Detail:** The independently supervised Tauri desktop owned its tray as
  intended, but the daemon had been installed from a default-feature binary
  that also owned the legacy tray. The two visually similar menus exposed
  different Dashboard behavior.
- **Mitigation:** Rebuilt and installed `familiar-ai-daemon` with
  `--no-default-features`, leaving the Tauri process as the sole tray owner.
  The desktop migration installer must make this invariant explicit so a later
  default-feature daemon replacement cannot recreate the duplicate menu.

### FAM-BUG-069 — Status pills inherit unreadable dark-mode foregrounds

- **Status:** Fixed 2026-09-22.
- **Found:** 2026-09-22 from a live macOS screenshot of the dependency Gantt.
- **Detail:** Pills used a hard-coded near-white background but inherited the
  surrounding dark-mode foreground. Multiword values such as `not found` also
  became two CSS classes, preventing a reliable status-specific override.
- **Fix:** Status names are normalized to stable CSS identifiers and every
  status family now has an explicit high-contrast foreground/background pair.
  A desktop contract regression pins completed, missing, in-progress, and
  unregistered states. The same update adds an operator-controlled
  `Hide completed` filter to the dependency Gantt.

### FAM-BUG-070 — Configured repositories vanish when no durable backlog row exists

- **Status:** Fixed 2026-09-22.
- **Found:** 2026-09-22 after rebuilding and relaunching the macOS desktop;
  its repository selector was empty even though Familiar remained enrolled in
  `config.toml`.
- **Detail:** The operator `repositories` query returned only repository keys
  inferred from durable backlog rows. An empty ledger therefore hid valid
  configured repositories, especially when the filesystem watcher was
  disabled. Configuration enrollment and watcher discovery were incorrectly
  treated as the same source of truth.
- **Fix:** Operator repository discovery now unions durable repository rows
  with canonically resolved configured repositories, deduplicates them by
  worktree, and sorts the result. Regression tests cover both an empty durable
  ledger and duplicate configured/durable enrollment.

### FAM-BUG-071 — Desktop never retries repository discovery after a startup race

- **Status:** Fixed 2026-09-23.
- **Found:** 2026-09-23 after restarting the daemon and desktop launch agents
  together; the desktop again displayed no projects even though the daemon's
  configured-repository response was correct.
- **Detail:** The desktop queried repositories once during startup. If it won
  the race against the daemon's control socket, that request failed and the
  later heartbeat only restored the connection indicator. It never reloaded
  the repository selector, leaving the process permanently empty until a
  manual desktop restart.
- **Fix:** Repository discovery now records successful completion, retries on
  the first connected heartbeat after a failure, and reloads after a daemon
  generation change. A desktop contract test pins the retry path.

### FAM-BUG-085 — Overlay reinstall leaves the macOS app bundle visibly stale (same finding as FAM-BUG-084, recorded from the Mac)

- **Status:** Fixed 2026-09-24 operationally on the Mac; a packaged installer remains
  desirable so manual installs cannot regress this.
- **Found:** 2026-09-24 after repeated rebuild/reinstall cycles still showed
  Familiar.app as three days old in Finder.
- **Detail:** The rebuilt executable was current, and launchd was running that
  executable, but `ditto` had repeatedly copied the new application *over* the
  installed bundle. That preserved the destination bundle directory's
  September 21 modification time and initially retained a stale
  `_CodeSignature`, making the installation both misleading and fragile.
- **Fix:** Stopped both launch agents, moved the prior bundle to a recoverable
  backup, copied the build into a nonexistent destination, ad-hoc signed the
  resulting bundle, verified it with strict `codesign`, and only then
  relaunched. Future macOS installation must replace the bundle atomically;
  it must never overlay an existing `.app` directory.

## 2026-09-05 — PRD-087: identity and event-sequence invariants

Three invariants now enforce facts that previously existed only as prose or
convention: **`repository-identity-single-mint`** (repository identity is
computed by exactly one function, `familiar_ai_core::repository_path`, so
identity derived inside a worktree resolves to that worktree's origin
repository), **`checkpoint-event-sequence-allocator`** (every writer of
`execution_checkpoint_events` mints its id and sequence through
`familiar_ai_storage::next_checkpoint_event_id`, so a checkpoint re-entered
across occurrences can never collide or drop an event), and
**`review-recovery-schema-tolerance`** (review recovery accepts a cycle row
written before `repository_key` existed, and records the tolerance in
`identity_invariant_tolerances` rather than refusing or silently rewriting
it). Each is pinned by a regression in
`crates/familiar-ai-daemon/tests/durable_invariants.rs`, which also owns the
linkage check below.

### Invariant coverage ledger

One line per prior entry in these three families: which invariant covers it
now, or why it stays uncovered. This list is checked byte-for-byte against
`durable_invariants.rs` so it cannot silently drift from the code.

- FAM-BUG-040: covered by repository-identity-single-mint
- FAM-BUG-037: covered by checkpoint-event-sequence-allocator
- FAM-BUG-039: covered by checkpoint-event-sequence-allocator
- FAM-BUG-032: covered by review-recovery-schema-tolerance
- FAM-BUG-016: uncovered: PRD-id spelling identity (canonical vs zero-padded), not repository-origin identity
- FAM-BUG-026: uncovered: migration tolerance for worker_specs recovery, a different recovery path than review recovery; already remediated by migration 051 itself
- FAM-BUG-041: uncovered: checkpoint phase-value bug in the scope-approval write path, not event-id minting
- FAM-BUG-035: uncovered: scope-decision enrollment policy, not identity or event-sequencing
- FAM-BUG-051: uncovered: file-path prefix scope authority, deferred to PRD-093
- FAM-BUG-018: uncovered: candidate-revision rebinding durability, not event-id minting
- FAM-BUG-021: uncovered: checkpoint hash/candidate durability across remediation, not event-id minting
