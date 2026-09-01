//! `familiar-ai next` — select the next eligible repository PRD without
//! executing it.

use familiar_ai_core::{
    load_manifest, validate_graph, AppPaths, BacklogDiscovery, BacklogManager, BacklogStatusStore,
    BootstrapApplyResult, FilesystemBacklogDiscovery, ProfiledFilesystemBacklogDiscovery,
};
use familiar_ai_storage::{SqliteBacklogRepository, SqliteBootstrapRepository};

use super::shared::{database, effective_repository_config};

pub fn next() -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let cwd =
        std::env::current_dir().map_err(|e| format!("current-directory lookup failed: {e}"))?;
    let config = effective_repository_config(&paths, &cwd)?;
    // Resolve Git before opening or migrating storage, preserving the domain's
    // required operation order for invalid working directories.
    let repository = FilesystemBacklogDiscovery
        .resolve(&cwd)
        .map_err(|e| e.to_string())?;
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|e| e.to_string())?;
    let discovered = FilesystemBacklogDiscovery
        .discover_with_layout(&repository, &repository_config.layout())
        .map_err(|e| e.to_string())?;
    if discovered.is_empty() {
        return Err("backlog is empty".into());
    }
    validate_graph(&discovered).map_err(|e| e.to_string())?;
    let mut db = database()?;
    SqliteBacklogRepository::new(db.conn_mut())
        .reconcile_and_snapshot(&repository, &discovered)
        .map_err(|e| e.to_string())?;
    let manifest = load_manifest(&repository, &discovered).map_err(|e| e.to_string())?;
    let applied = SqliteBootstrapRepository::new(db.conn_mut())
        .apply(&repository, &discovered, manifest.as_ref())
        .map_err(|e| e.to_string())?;
    if let BootstrapApplyResult::Applied(run) = applied {
        eprintln!(
            "historical backlog bootstrap applied: run={} items={} manifest={}",
            run.run_id, run.item_count, run.canonical_hash
        );
    }
    let store = SqliteBacklogRepository::new(db.conn_mut());
    let mut manager = BacklogManager::new(
        ProfiledFilesystemBacklogDiscovery {
            layout: repository_config.layout(),
        },
        store,
    );
    let selected = manager.next(&cwd).map_err(|e| e.to_string())?;
    println!(
        "{}\t{}\t{}\t{}",
        selected.id,
        selected.path,
        selected.status.as_str(),
        selected.title
    );
    Ok(())
}
