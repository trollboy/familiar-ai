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
    admit_run_prd, resolve_run_prd, validate_graph, validate_recovery_attribution, BacklogConflict,
    BacklogConflictKind, BacklogDiscovery, BacklogDiscoveryOutcome, BacklogEntry, BacklogError,
    BacklogManager, BacklogProfile, BacklogProfileKind, BacklogRecoveryAction, BacklogStatus,
    BacklogStatusStore, BacklogStoreError, DiscoveredPrd, FilesystemBacklogDiscovery,
    IneligibilityReason, NextPrd, PrdId, PrdLocation, RepositoryIdentity, RepositoryPath,
    ACTIVE_PRD_DIR, ARCHIVED_LOCATION_ACTOR, ARCHIVED_PRD_DIR,
};
pub use bootstrap::*;
pub use config::{
    validate_repositories, AgentAdapterKind, AgentEffort, AgentEntryConfig, AgentPermissionMode,
    AgentsConfig, BacklogProfileName, BudgetProfile, Config, ConfigScope, DashboardConfig,
    DriverConfig, ExecutionContextConfig, ExecutionHistoryConfig, ExecutionPrice, InferenceConfig,
    InferenceMode, PackerConfig, ProhibitedChangeConfig, ProviderKind, ReferenceRootConfig,
    ReferenceRootKind, RepositoryEntryConfig, ResolvedProhibitedRule, ReviewConfig,
    ReviewScopeConfig, RollupConfig, ScopeClassPolicyConfig, ScopeClassificationConfig,
    ScopeDeclarationModeConfig, ScopeFileClassName, SummaryConfig, TrayConfig,
    TypedProhibitedChange, WatcherConfig,
};
pub use error::{FamiliarError, Result};
pub use paths::AppPaths;
pub use repository_path::{CanonicalFileIdentity, PathIdentityError};
pub use status::AppStatus;
pub use version::VersionInfo;
