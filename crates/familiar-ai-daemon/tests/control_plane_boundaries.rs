use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}
fn source(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap()
}
fn function<'a>(source: &'a str, name: &str, next: &str) -> &'a str {
    let start = source.find(name).unwrap();
    let end = source[start..]
        .find(next)
        .map(|offset| start + offset)
        .unwrap();
    &source[start..end]
}

#[test]
fn mcp_binary_has_no_sqlite_or_domain_policy_implementation() {
    let binary = source("crates/familiar-ai-mcp/src/bin/familiar-ai-mcp.rs");
    assert!(!binary.contains("Database::open"));
    assert!(!binary.contains("run_migrations"));
    let tools = source("crates/familiar-ai-mcp/src/tools/control_plane.rs");
    assert!(!tools.contains("rusqlite"));
    assert!(!tools.contains("ReservationRepository"));
    assert!(!tools.contains("WorkerRegistry"));
}

#[test]
fn cli_control_adapter_contains_no_queue_sql_or_scheduler() {
    let cli = source("crates/familiar-ai-daemon/src/bin/familiar-ai.rs");
    assert!(!cli.contains("INSERT INTO control_plane_executions"));
    assert!(!cli.contains("UPDATE control_plane_projects SET last_claim_sequence"));
    assert!(!cli.contains("claim_next("));
}

#[test]
fn claim_precedes_database_open_and_socket_binding_in_daemon_bootstrap() {
    let main = source("crates/familiar-ai-daemon/src/main.rs");
    let claim = main.find("acquire_with_socket").unwrap();
    let database = main.find("Database::open(&db_path)").unwrap();
    let socket = main.find("LocalHost::bind").unwrap();
    assert!(claim < database && database < socket);
}

#[test]
fn legacy_cli_mutation_handlers_are_rendering_adapters_only() {
    let cli = source("crates/familiar-ai-daemon/src/bin/familiar-ai.rs");
    let handlers = [
        function(&cli, "fn resume_command", "fn scope_decisions"),
        function(&cli, "fn deliver_command", "fn preflight_command"),
        function(&cli, "fn run(prd_path", "fn handle_attached_review"),
        function(&cli, "fn drive_command", "fn report_command"),
    ];
    for handler in handlers {
        for forbidden in [
            "Database::open",
            "WorkerLock",
            "ReservationRepository",
            "AccountingRepository",
            "DriveWarrant",
            "plan_waves",
            "resolved_agent_entries",
            "build_agent",
        ] {
            assert!(
                !handler.contains(forbidden),
                "CLI handler contains shared-service concern {forbidden}:\n{handler}"
            );
        }
    }
    assert!(source("crates/familiar-ai-daemon/src/run.rs").contains("pub struct PreparedRun"));
    assert!(source("crates/familiar-ai-daemon/src/resume.rs").contains("pub fn execute_configured"));
    assert!(source("crates/familiar-ai-daemon/src/drive.rs").contains("pub fn execute_configured"));
    assert!(
        source("crates/familiar-ai-daemon/src/delivery.rs").contains("pub fn execute_configured")
    );
}

#[test]
fn scheduler_claim_transaction_owns_prd064_reservation_admission() {
    let repository = source("crates/familiar-ai-storage/src/repos/control_plane.rs");
    let claim = function(&repository, "pub fn claim_next", "pub fn recover_running");
    assert!(claim.contains("transaction_with_behavior(TransactionBehavior::Immediate)"));
    assert!(claim.contains("acquire_control_plane_reservation"));
    let acquire = function(
        &repository,
        "fn acquire_control_plane_reservation",
        "fn release_control_plane_reservation",
    );
    assert!(acquire.contains("INSERT INTO resource_reservations"));
    assert!(acquire.contains("INSERT INTO resource_reservation_items"));
    assert!(!acquire.contains("transaction("));
}
