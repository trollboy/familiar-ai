pub mod backlog;
pub mod bootstrap;
pub mod config;
pub mod error;
pub mod models;
pub mod paths;
pub mod repository_path;
pub mod status;
pub mod version;

pub use backlog::{
    admit_run_prd, resolve_run_prd, structured_prd_metadata, validate_graph,
    validate_recovery_attribution, BacklogDiscovery, BacklogEntry, BacklogError, BacklogLayout,
    BacklogManager, BacklogProfile, BacklogRecoveryAction, BacklogStatus, BacklogStatusStore,
    BacklogStoreError, DiscoveredPrd, FilesystemBacklogDiscovery, IneligibilityReason, NextPrd,
    PrdId, PrdLocation, PrdMetadata, PrdMetadataPolicy, ProfiledFilesystemBacklogDiscovery,
    RepositoryIdentity, RepositoryPath,
};
pub use bootstrap::*;
pub use config::{
    AgentAdapterKind, AgentEffort, AgentEntryConfig, AgentPermissionMode, AgentsConfig,
    BudgetProfile, Config, DashboardConfig, DeliveryConfig, DeliveryMode, DriverConfig,
    DriverModelRouteConfig, ExecutionHistoryConfig, ExecutionPrice, InferenceConfig, InferenceMode,
    PackerConfig, PlannerConfig, PocSelfApprovalWarrant, PreflightCommandConfig, PreflightConfig,
    ProhibitedChangeConfig, ProviderKind, ReferenceKind, ReferenceRootConfig, RepositoryConfig,
    ResolvedProhibitedRule, ReviewGateConfig, ReviewScopeConfig, RollupConfig,
    ScopeClassPolicyConfig, ScopeClassificationConfig, ScopeDeclarationModeConfig,
    ScopeFileClassName, SummaryConfig, TrayConfig, TypedProhibitedChange, WatcherConfig,
};
pub use error::{FamiliarError, Result};
pub use paths::AppPaths;
pub use repository_path::{CanonicalFileIdentity, PathIdentityError};
pub use status::AppStatus;
pub use version::VersionInfo;
