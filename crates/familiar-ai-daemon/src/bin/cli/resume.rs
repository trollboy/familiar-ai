use super::run::handle_attached_review;

pub fn resume_command(prd: &str, dry_run: bool) -> Result<(), String> {
    let lines = familiar_ai_daemon::resume::execute_configured(
        prd,
        dry_run,
        |error, worktree, config, paths, agents| {
            handle_attached_review(Err(error), worktree, config, paths, agents)
        },
    )?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
