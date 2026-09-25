pub mod backlog;
pub mod bootstrap;
pub mod config;
pub mod control_plane;
pub mod error;
pub mod git_env;
pub mod lifecycle;
pub mod models;
pub mod onboarding;
pub mod operator_ui;
pub mod paths;
pub mod probation;
pub mod repository_path;
pub mod reservation;
pub mod status;
pub mod version;

pub use backlog::{
    admission_quality, admit_run_prd, resolve_run_prd, structured_prd_metadata, validate_graph,
    validate_recovery_attribution, AdmissionQualityCheck, AdmissionQualityReport, BacklogDiscovery,
    BacklogEntry, BacklogError, BacklogLayout, BacklogManager, BacklogProfile,
    BacklogRecoveryAction, BacklogStatus, BacklogStatusStore, BacklogStoreError, DiscoveredPrd,
    FilesystemBacklogDiscovery, IneligibilityReason, MetadataCheckMode, NextPrd, PrdId,
    PrdLocation, PrdMetadata, PrdMetadataPolicy, ProfiledFilesystemBacklogDiscovery,
    RepositoryIdentity, RepositoryPath,
};
pub use bootstrap::*;
pub use config::{
    AgentAdapterKind, AgentEffort, AgentEntryConfig, AgentPermissionMode, AgentsConfig,
    BudgetProfile, Config, DashboardConfig, DeliveryConfig, DeliveryMode, DeployRecipeConfig,
    DriverConfig, DriverModelRouteConfig, EndpointProviderKind, ExecutionHistoryConfig,
    ExecutionPrice, FamiliarToml, Forge, InferenceConfig, InferenceMode, PackerConfig,
    PlannerConfig, PocSelfApprovalWarrant, PreflightCommandConfig, PreflightConfig,
    ProhibitedChangeConfig, ProviderKind, ReferenceKind, ReferenceRootConfig, RepositoryConfig,
    ResolvedProhibitedRule, ReviewGateConfig, ReviewScopeConfig, RollupConfig,
    ScopeClassPolicyConfig, ScopeClassificationConfig, ScopeDeclarationModeConfig,
    ScopeFileClassName, SummaryConfig, TrayConfig, TypedProhibitedChange, WatcherConfig,
};
pub use error::{FamiliarError, Result};
pub use lifecycle::{
    derive as derive_lifecycle, is_human_gate_reason, AttemptFacts, DerivedLifecycle,
    LifecycleInputs, PrdLifecycle, HUMAN_GATE_REASONS,
};
pub use operator_ui::{
    ConfigEdit as OperatorConfigEdit, OperatorAction, OperatorDataSource, OperatorError,
    OperatorEvent, OperatorMutation, OperatorQuery, OperatorReply, OPERATOR_PROTOCOL_VERSION,
};
pub use paths::AppPaths;
pub use repository_path::{
    git_common_directory, repository_origin_key, CanonicalFileIdentity, PathIdentityError,
    RepositoryOriginError, INVARIANT_REPOSITORY_IDENTITY,
};
pub use reservation::{
    GrantMode, OwnerLiveness, OwnerLivenessEvidence, ReservationOwnerIdentity, ResourceRequest,
    ResourceType, UnknownConsumptionPolicy,
};
pub use status::AppStatus;
pub use version::VersionInfo;
