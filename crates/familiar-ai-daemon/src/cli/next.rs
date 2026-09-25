//! `familiar-ai next` — select the next eligible repository PRD without
//! executing it.

use familiar_ai_core::{
    admission_quality, load_manifest, validate_graph, AppPaths, BacklogDiscovery, BacklogManager,
    BacklogStatusStore, BootstrapApplyResult, FilesystemBacklogDiscovery,
    ProfiledFilesystemBacklogDiscovery,
};
use familiar_ai_storage::{ReviewRepository, SqliteBacklogRepository, SqliteBootstrapRepository};

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
    // PRD-083: computed before `manager` borrows `db` mutably, so a "backlog
    // is empty" error can still say why when the real blocker is a scope
    // decision waiting on a human, not an actually-empty backlog.
    let pending_scope = familiar_ai_storage::OrchestrationRepository::new(db.conn())
        .pending_scope_decisions(&repository.key)
        .map_err(|e| e.to_string())?;
    let store = SqliteBacklogRepository::new(db.conn_mut());
    let mut manager = BacklogManager::new(
        ProfiledFilesystemBacklogDiscovery {
            layout: repository_config.layout(),
        },
        store,
    );
    let selected = match manager.next(&cwd) {
        Ok(selected) => selected,
        Err(error) => {
            for notice in crate::stewardship::scope_pause_notices(&pending_scope, "next") {
                eprintln!("{notice}");
            }
            return Err(error.to_string());
        }
    };
    drop(manager);
    let selected_prd = discovered
        .iter()
        .find(|prd| prd.path == selected.path)
        .expect("selection came from discovered backlog");
    let quality = admission_quality(&repository, &discovered, selected_prd);
    ReviewRepository::new(db.conn())
        .record_admission_quality(&repository.key, &selected_prd.content_hash, &quality)
        .map_err(|error| error.to_string())?;
    if let Some(refusal) = quality.refusal() {
        return Err(format!(
            "admission quality refused {}: {refusal}",
            selected.id
        ));
    }
    // PRD-109: the derived lifecycle beside the raw ledger status. A PRD
    // `next` just selected has no live attempt, so the file and the row are
    // the only inputs that can apply here.
    let file_status = discovered
        .iter()
        .find(|prd| prd.path == selected.path)
        .and_then(|prd| prd.metadata.status.clone());
    let lifecycle = familiar_ai_core::derive_lifecycle(&familiar_ai_core::LifecycleInputs {
        file_status,
        ledger_status: Some(selected.status.as_str().to_string()),
        ..Default::default()
    });
    println!(
        "{}\t{}\t{}\t{}\t{}",
        selected.id,
        selected.path,
        selected.status.as_str(),
        lifecycle.lifecycle,
        selected.title
    );
    Ok(())
}
