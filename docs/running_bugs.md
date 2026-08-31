# Familiar Running Bugs and Friction

This is the live operator-facing defect and friction log. Entries remain open
until a verified fix is committed; a fix records its evidence and disposition
instead of deleting the history.

## 2026-08-31 — Provider and model registration

### FAM-BUG-001 — Model inventory does not distinguish installed, registered, enabled, and routable

- **Status:** Open
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

- **Status:** Open
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

- **Status:** Corruption guard fixed (committed in `4f0305e`); migration UX remains open
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

- **Status:** Open
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

- **Status:** Open; unsafe worker disabled and claims recovered
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

- **Status:** Open
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

- **Status:** Open
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

- **Status:** Open; observed during Wave 3
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

- **Status:** Open; review blocked for PRD-052 and PRD-054
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

- **Status:** Open; PRD-050 retained
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

- **Status:** Open; PRD-050/057 verification affected
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

- **Status:** Open; Wave 3 recovery requires manual integration
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

- **Status:** Open; live configuration was protected by validation
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

- **Status:** Open; PRD-050 required an audited manual completion override
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

- **Status:** Open design/UX gap
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

- **Status:** Open
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

- **Status:** Open
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

- **Status:** Open
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

- **Status:** Open; systemic release-blocking dogfood failure
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

- **Status:** Open; blocks autonomous Wave 4 recovery
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

- **Status:** Open; concrete second reproduction of FAM-BUG-019
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

### FAM-BUG-023 — Valid config cannot be edited because disabled delivery defaults active

- **Status:** Open; machine config repaired manually
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
