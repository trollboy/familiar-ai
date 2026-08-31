use familiar_ai_daemon::context_service::ContextService;
use familiar_ai_watcher::WatcherEvent;
use std::fs;

#[test]
fn one_watch_event_reindexes_only_one_file_and_locations_follow_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let a = repo.join("a.rs");
    let b = repo.join("b.rs");
    fs::write(&a, "pub fn alpha() {}\n").unwrap();
    fs::write(&b, "pub fn beta() {}\n").unwrap();
    let service = ContextService::default();
    service.index_file(repo, &a).unwrap();
    service.index_file(repo, &b).unwrap();
    let before = service.map(repo).unwrap();
    fs::write(&a, "\npub fn renamed() {}\n").unwrap();
    service.apply(&WatcherEvent::FileChanged {
        path: a.clone(),
        repo_root: Some(repo.to_owned()),
    });
    let after = service.map(repo).unwrap();
    assert_eq!(after.reindex_count("a.rs"), 2);
    assert_eq!(after.reindex_count("b.rs"), 1);
    assert_eq!(before.files()["b.rs"], after.files()["b.rs"]);
    let symbol = &after.files()["a.rs"].symbols[0];
    assert_eq!(symbol.name, "renamed");
    assert_eq!(symbol.line, 2);
    assert!(fs::read_to_string(repo.join(&after.files()["a.rs"].path))
        .unwrap()
        .lines()
        .nth((symbol.line - 1) as usize)
        .unwrap()
        .contains(&symbol.name));
}

#[test]
fn restart_bytes_are_stable_and_degradation_is_named() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("lib.rs");
    fs::write(&file, "pub struct Stable;\n").unwrap();
    let first = ContextService::default();
    first.index_file(temp.path(), &file).unwrap();
    let second = ContextService::default();
    second.index_file(temp.path(), &file).unwrap();
    assert_eq!(
        first.serialized(temp.path(), 100),
        second.serialized(temp.path(), 100)
    );
    let uncovered = familiar_ai_repomap::RepositoryMap::new(false);
    assert!(uncovered
        .missing_coverage()
        .any(|m| m.reason.contains("no watch coverage")));
    let bad = temp.path().join("asset.bin");
    fs::write(&bad, "text").unwrap();
    assert!(second.index_file(temp.path(), &bad).is_err());
    assert!(second
        .map(temp.path())
        .unwrap()
        .missing_coverage()
        .any(|m| m.path.as_deref() == Some("asset.bin")));
}
