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

### FAM-BUG-002 — OpenAI-compatible endpoints were treated as Ollama

- **Status:** Fixed locally; uncommitted
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

### FAM-BUG-004 — Claude CLI false-positive authentication

- **Status:** Fixed locally; uncommitted
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

- **Status:** Fixed locally; uncommitted
- **Observed:** Familiar's hand-written TCP/HTTP parser attempted to parse a
  valid chunked `/api/tags` body as plain JSON and reported `provider returned
  malformed discovery`.
- **Impact:** A healthy Ollama installation with eight models could not be
  registered.
- **Local fix:** Replaced the hand-written response parser with the existing
  standards-compliant HTTP client.
- **Evidence:** Live registration succeeded and discovered all eight models.

### FAM-BUG-006 — First model enable can create an invalid mixed configuration

- **Status:** Corruption guard fixed locally; migration UX remains open
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

### FAM-BUG-008 — Claude model enable selected the Codex adapter

- **Status:** Fixed locally; uncommitted
- **Observed:** Provider registration correctly admitted authenticated Claude,
  but `config model enable claude/claude` mapped every provider other than the
  literal name `ollama` to `adapter = "codex"`.
- **Impact:** Familiar would invoke Codex while displaying a Claude provider
  and model identity, corrupting routing and provenance.
- **Local fix:** Provider-to-adapter selection now maps `claude` to
  `claude-code`, with a configuration round-trip regression test. The unsafe
  worker entry was removed before any execution.

### FAM-FRICTION-001 — Provider registration does not imply execution readiness

- **Status:** Open design/UX gap
- **Observed:** Unsloth can be authenticated and discovered, but its model
  cannot be enabled until the local raw-inference agent runtime is implemented.
- **Impact:** `provider list` looks successful while the model remains unusable
  for PRD execution; `model list` is empty without explaining why.
- **Expected fix:** Surface the blocked transition and dependency in inventory
  output, and finish the neutral local runtime rather than routing Unsloth
  through Codex.

### FAM-FRICTION-002 — Provider CLI discovery for subscription CLIs is not real model discovery

- **Status:** Open
- **Observed:** Codex and Claude provider probes return synthetic model IDs
  (`codex`, `claude`) derived from the login command rather than identifying the
  selected or available model.
- **Impact:** Routing provenance cannot honestly answer which hosted model will
  execute work.
- **Expected fix:** Represent CLI-default/unknown model identity explicitly and
  capture the provider-reported model from execution; never present a command
  name as a discovered model.

### FAM-FRICTION-003 — Capability declarations are manual and unverified

- **Status:** Open
- **Observed:** `config model enable` requires the operator to assert planning,
  implementation, review, remediation, or narrow-task capabilities without a
  qualification probe or evidence record.
- **Impact:** Enabling all installed models risks optimistic routing claims.
- **Expected fix:** Start conservatively, distinguish declared from verified
  capabilities, and promote models through deterministic qualification and
  empirical history.

### FAM-FRICTION-004 — Capability display is not canonical

- **Status:** Open
- **Observed:** `config model list` renders `narrow-task` as `narrowtask`.
- **Impact:** Display output does not round-trip to the accepted CLI value and
  invites invalid copy/paste commands.
- **Expected fix:** Use the canonical serialized capability spelling on every
  CLI and dashboard surface.
