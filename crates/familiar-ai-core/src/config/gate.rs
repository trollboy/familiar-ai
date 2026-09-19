use serde::{Deserialize, Serialize};

/// PRD-099. Which callers of the single definition fire automatically.
///
/// This toggles a *trigger*, never a step. What verification means stays in
/// `scripts/gate.sh` and is changeable only by a reviewed change to the
/// repository; nothing here can add, remove or weaken what the gate runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateConfig {
    /// Whether the `pre-push` hook runs the gate before a push leaves the
    /// machine. Defaults to on.
    ///
    /// Turning it off exists for one known case: Familiar's own driver pushes
    /// candidate branches, and a hook-refused push aborts delivery as an
    /// unclassified command error rather than a recorded verdict — which
    /// produces a retry loop and no usable taxonomy. Until the delivery path
    /// asks `gate require` itself, an unattended wave wants this off.
    ///
    /// A commit pushed with the hook off simply has no recorded verdict, so
    /// `familiar-ai gate status` answers `absent` for it. That is the honest
    /// answer and is never a pass.
    #[serde(default = "default_pre_push_hook")]
    pub pre_push_hook: bool,
}

fn default_pre_push_hook() -> bool {
    true
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            pre_push_hook: default_pre_push_hook(),
        }
    }
}
