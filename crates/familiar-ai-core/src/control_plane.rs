use serde::{Deserialize, Serialize};

pub const CONTROL_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Detached,
    Attached,
    ForegroundOnly,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Detached => "detached",
            Self::Attached => "attached",
            Self::ForegroundOnly => "foreground_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    Active,
    Paused,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    ForegroundEnded,
    AmbiguousLiveOrphan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub execution_id: String,
    pub project_id: String,
    pub idempotency_key: String,
    pub mode: ExecutionMode,
    pub priority: i64,
    /// Host-interpreted, never model-visible execution specification.
    #[serde(default)]
    pub command_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub execution_id: String,
    pub project_id: String,
    pub mode: ExecutionMode,
    pub state: ExecutionState,
    pub attempt: i64,
    pub worker_identity: Option<String>,
    pub command_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    /// Returned only to the trusted host adapter which requested the grant.
    pub credential: String,
    pub scope: CapabilityScope,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCapabilityView {
    pub project_id: String,
    pub execution_id: String,
    pub attempt: i64,
    pub worker_id: String,
    pub state: ExecutionState,
    pub warrant_json: Option<String>,
    pub remaining_reservations_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionAck {
    pub execution_id: String,
    pub duplicate: bool,
    pub event_cursor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlEvent {
    pub cursor: i64,
    pub event_id: String,
    pub execution_id: String,
    pub kind: String,
    pub payload_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientClass {
    Operator,
    Observer,
    Mcp,
    Worker,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Observe,
    Control,
    ReadWarrant,
    ReadAccountingLabels,
    ReportProgress,
    SubmitEvidence,
    RequestEscalation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityScope {
    pub client_class: ClientClass,
    pub project_id: Option<String>,
    pub execution_id: Option<String>,
    pub attempt: Option<i64>,
    pub worker_id: Option<String>,
    pub authorities: Vec<Authority>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipClaim {
    pub installation_id: String,
    pub owner_nonce: String,
    pub owner_pid: u32,
    pub process_start_identity: String,
    pub boot_identity: Option<String>,
    pub socket_path: String,
    pub protocol_version: u32,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulingPolicy {
    pub global_ceiling: usize,
    pub default_project_ceiling: usize,
}

impl Default for SchedulingPolicy {
    fn default() -> Self {
        Self {
            global_ceiling: 1,
            default_project_ceiling: 1,
        }
    }
}
