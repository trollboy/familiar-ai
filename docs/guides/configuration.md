# Familiar configuration reference

## Machine configuration

Familiar reads `~/.config/familiar-ai/config.toml` (XDG; `Familiar-AI` on macOS).
Environment overrides use the `FAMILIAR_AI_` prefix — stale `FAMILIAR_` variables
fail closed rather than being silently ignored.

```toml
[driver]                                  # the unattended warrant
max_prds_per_session   = 3
max_session_duration_ms = 28800000

[agents.implementation]                   # who writes the code
adapter = "claude-code"
executable = "claude"
model = "sonnet"

[agents.reviewer]                         # must differ from the implementer
adapter = "claude-code"
executable = "claude"
model = "opus"

[review]
enabled = true
max_review_attempts = 3
max_total_tokens = 400000
allowed_paths = ["crates/"]

[execution_context]
hard_ceiling_tokens = 60000               # caps the compiled prompt
```

Agent selection is deterministic config, never a model choosing a model.
Same-model review is refused: independence is the point.

---
