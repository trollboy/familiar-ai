//! Pinned trust-boundary regressions for PRD-067.  These live in their own
//! integration target so concurrent daemon PRDs do not share a fixture scope.

use std::collections::BTreeMap;

use familiar_ai_review::{
    CommandVerificationRunner, VerificationCheck, VerificationRunner, VerificationStatus,
};
use familiar_ai_storage::Database;

#[test]
fn environment_denial_is_durable_and_names_the_check_and_environment() {
    let repository = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    let runner = CommandVerificationRunner::new(artifacts.path().into(), 1024);
    let check = VerificationCheck {
        check_id: "focused-tests".into(),
        argv: vec!["definitely-not-an-installed-verification-tool".into()],
        working_directory: ".".into(),
        environment: BTreeMap::new(),
        timeout_ms: 100,
        required: true,
        path_prefixes: vec![],
    };
    let evidence = runner.run(repository.path(), &check, "candidate").unwrap();
    assert_eq!(evidence.status, VerificationStatus::EnvironmentDenied);
    assert!(evidence.summary.contains("focused-tests"));
    assert_eq!(evidence.environment_identity["executor"], "local-process");
    assert!(evidence.environment_identity.contains_key("repository"));
}

#[test]
fn migration_032_persists_waiver_and_verification_truth_dimensions() {
    let database = Database::open_in_memory().unwrap();
    database.run_migrations().unwrap();
    let waiver_table: i64 = database.conn().query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='review_finding_waivers'",
        [], |row| row.get(0)).unwrap();
    assert_eq!(waiver_table, 1);
    let columns = database
        .conn()
        .prepare("PRAGMA table_info(review_verification_evidence)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.contains(&"repository_key".into()));
    assert!(columns.contains(&"environment_identity_json".into()));
    assert!(columns.contains(&"classification".into()));
}
