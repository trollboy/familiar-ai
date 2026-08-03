use std::sync::{Arc, Mutex};

use familiar_llm::InferenceRouter;
#[cfg(feature = "tray")]
use familiar_tray::TrayCommand;
use tokio::sync::mpsc;

use familiar_core::AppStatus;

/// Daemon-internal command type. Mirrors TrayCommand variants we care about
/// but doesn't depend on the tray crate so it works in headless builds.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DaemonCommand {
    EnableLlm,
    DisableLlm,
    PauseHeavyTasks,
    ResumeHeavyTasks,
    Quit,
}

#[cfg(feature = "tray")]
impl From<TrayCommand> for Option<DaemonCommand> {
    fn from(cmd: TrayCommand) -> Self {
        match cmd {
            TrayCommand::EnableLlm => Some(DaemonCommand::EnableLlm),
            TrayCommand::DisableLlm => Some(DaemonCommand::DisableLlm),
            TrayCommand::PauseHeavyTasks => Some(DaemonCommand::PauseHeavyTasks),
            TrayCommand::ResumeHeavyTasks => Some(DaemonCommand::ResumeHeavyTasks),
            TrayCommand::Quit => Some(DaemonCommand::Quit),
            // OpenSettings and OpenProject are handled by the tray itself via opener.
            TrayCommand::OpenSettings | TrayCommand::OpenProject(_) => None,
        }
    }
}

pub struct CommandState {
    pub paused: bool,
}

impl CommandState {
    pub fn new() -> Self {
        Self { paused: false }
    }
}

impl Default for CommandState {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn handle_commands(
    mut rx: mpsc::Receiver<DaemonCommand>,
    status: Arc<Mutex<AppStatus>>,
    command_state: Arc<Mutex<CommandState>>,
    router: Arc<InferenceRouter>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) {
    while let Some(cmd) = rx.recv().await {
        match &cmd {
            DaemonCommand::EnableLlm => {
                tracing::info!("enabling local LLM");
                match router.enable().await {
                    Ok(()) => {
                        let mut s = status.lock().unwrap();
                        s.local_llm_enabled = true;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to enable LLM");
                        let mut s = status.lock().unwrap();
                        s.local_llm_enabled = false;
                    }
                }
            }
            DaemonCommand::DisableLlm => {
                tracing::info!("disabling local LLM");
                router.disable().await;
                let mut s = status.lock().unwrap();
                s.local_llm_enabled = false;
            }
            DaemonCommand::PauseHeavyTasks | DaemonCommand::ResumeHeavyTasks => {
                apply_sync_command(&cmd, &status, &command_state);
            }
            DaemonCommand::Quit => {
                apply_sync_command(&cmd, &status, &command_state);
                tracing::info!("quit command received from tray");
                let _ = shutdown_tx.send(true);
                break;
            }
        }
    }
}

/// Handles the sync-only commands (pause/resume/quit). LLM commands go
/// through the async path in `handle_commands` directly.
pub fn apply_sync_command(
    cmd: &DaemonCommand,
    _status: &Arc<Mutex<AppStatus>>,
    command_state: &Arc<Mutex<CommandState>>,
) {
    match cmd {
        DaemonCommand::PauseHeavyTasks => {
            tracing::info!("pausing heavy background tasks");
            let mut state = command_state.lock().unwrap();
            state.paused = true;
        }
        DaemonCommand::ResumeHeavyTasks => {
            tracing::info!("resuming heavy background tasks");
            let mut state = command_state.lock().unwrap();
            state.paused = false;
        }
        DaemonCommand::Quit => {
            tracing::info!("quit requested");
        }
        // LLM commands handled separately in handle_commands (async path)
        DaemonCommand::EnableLlm | DaemonCommand::DisableLlm => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_core::config::InferenceConfig;

    fn make_status() -> Arc<Mutex<AppStatus>> {
        Arc::new(Mutex::new(AppStatus::new()))
    }

    fn make_state() -> Arc<Mutex<CommandState>> {
        Arc::new(Mutex::new(CommandState::new()))
    }

    fn make_router() -> Arc<InferenceRouter> {
        Arc::new(InferenceRouter::new(&InferenceConfig::default()))
    }

    #[tokio::test]
    async fn enable_llm_with_default_config_succeeds() {
        let status = make_status();
        let state = make_state();
        let router = make_router();
        let (tx, rx) = mpsc::channel(8);
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);

        tx.send(DaemonCommand::EnableLlm).await.unwrap();
        tx.send(DaemonCommand::Quit).await.unwrap();
        drop(tx);

        handle_commands(rx, status.clone(), state, router.clone(), shutdown_tx).await;

        // Default config is disabled mode — enable on disabled is a no-op
        // that doesn't error, but doesn't set local_llm_enabled either
        // because there are no backends to load.
        let health = router.health().await;
        assert_eq!(health.text_mode, "disabled");
    }

    #[tokio::test]
    async fn disable_llm_clears_status() {
        let status = make_status();
        let state = make_state();
        let router = make_router();
        status.lock().unwrap().local_llm_enabled = true;

        let (tx, rx) = mpsc::channel(8);
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);

        tx.send(DaemonCommand::DisableLlm).await.unwrap();
        tx.send(DaemonCommand::Quit).await.unwrap();
        drop(tx);

        handle_commands(rx, status.clone(), state, router.clone(), shutdown_tx).await;

        assert!(!status.lock().unwrap().local_llm_enabled);
    }

    #[test]
    fn pause_sets_paused() {
        let status = make_status();
        let state = make_state();
        apply_sync_command(&DaemonCommand::PauseHeavyTasks, &status, &state);
        assert!(state.lock().unwrap().paused);
    }

    #[test]
    fn resume_clears_paused() {
        let status = make_status();
        let state = make_state();
        state.lock().unwrap().paused = true;
        apply_sync_command(&DaemonCommand::ResumeHeavyTasks, &status, &state);
        assert!(!state.lock().unwrap().paused);
    }

    #[test]
    fn quit_does_not_modify_state() {
        let status = make_status();
        let state = make_state();
        apply_sync_command(&DaemonCommand::Quit, &status, &state);
        assert!(!status.lock().unwrap().local_llm_enabled);
        assert!(!state.lock().unwrap().paused);
    }

    #[tokio::test]
    async fn handle_commands_quit_triggers_shutdown() {
        let status = make_status();
        let state = make_state();
        let router = make_router();
        let (tx, rx) = mpsc::channel(8);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        let shutdown_tx_keep = shutdown_tx.clone();

        tx.send(DaemonCommand::Quit).await.unwrap();
        drop(tx);

        handle_commands(rx, status, state, router, shutdown_tx).await;

        assert!(*shutdown_rx.borrow_and_update());
        drop(shutdown_tx_keep);
    }

    #[tokio::test]
    async fn handle_commands_processes_multiple() {
        let status = make_status();
        let state = make_state();
        let router = make_router();
        let (tx, rx) = mpsc::channel(8);
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);

        tx.send(DaemonCommand::EnableLlm).await.unwrap();
        tx.send(DaemonCommand::PauseHeavyTasks).await.unwrap();
        tx.send(DaemonCommand::Quit).await.unwrap();
        drop(tx);

        handle_commands(rx, status.clone(), state.clone(), router, shutdown_tx).await;

        assert!(status.lock().unwrap().local_llm_enabled);
        assert!(state.lock().unwrap().paused);
    }
}
