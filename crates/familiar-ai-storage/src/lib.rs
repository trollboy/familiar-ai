pub mod db;
pub mod migrate;
pub mod repos;
pub(crate) mod sql;

pub use db::Database;
pub use repos::backlog::SqliteBacklogRepository;
pub use repos::bootstrap::SqliteBootstrapRepository;
pub use repos::decision::DecisionRepository;
pub use repos::driver::{DriverAttempt, DriverRepository, DriverSession};
pub use repos::execution_history::{
    ExecutionFinalization, ExecutionHistoryRepository, ExecutionRecord, ExecutionStart,
    UsageSummary,
};
pub use repos::file_summary::{
    FileSummaryReconciliationResult, FileSummaryRepository, FileSummaryRollbackResult,
    ReconciliationReason,
};
pub use repos::lifecycle::{
    LifecycleChange, LifecycleOutcome, LifecycleRepository, PendingSummaryWork, RetirementReason,
    ScanRun, ScanStatus,
};
pub use repos::project::ProjectRepository;
pub use repos::review::ReviewRepository;
pub use repos::session_rollup::SessionRollupRepository;
