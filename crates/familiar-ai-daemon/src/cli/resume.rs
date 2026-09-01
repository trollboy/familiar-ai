//! `familiar-ai resume` — continue one durable partial, or inspect/schedule
//! all durable partials.

pub fn resume_command(prd: &str, dry_run: bool) -> Result<(), String> {
    let lines = crate::resume::execute_configured(
        prd,
        dry_run,
        |error, worktree, config, paths, agents| {
            crate::cli::run::handle_attached_review(Err(error), worktree, config, paths, agents)
        },
    )?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
