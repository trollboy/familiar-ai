# Familiar Running Bugs and Friction

This is the live operator-facing defect and friction log. Entries remain open
until a verified fix is committed; a fix records its evidence and disposition
instead of deleting the history.

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

- **Status:** CLOSED 2026-09-02 — waves 5 and 6 delivered autonomously
- **Audit evidence:** Wave 5 (038/053/058) and wave 6 (059/060/061) ran claim → implement → verify → independent review → merge-queue integration without manual git operations. Remaining human touchpoints are DESIGNED gates (scope decisions on undeclared dependency changes, waivers for reviewer claims), not delivery collapse. The operator role is now approval, not integration.
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

- **Status:** CLOSED 2026-09-02 — see FAM-BUG-019
- **Audit evidence:** Same closure evidence: two consecutive waves integrated through the merge queue.
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

- **Status:** CLOSED 2026-09-01 — fix candidate 5f517db validated by a clean
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

- **Status:** CLOSED 2026-09-02 — not reproducible; the original sighting
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

- **Status:** Open
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
