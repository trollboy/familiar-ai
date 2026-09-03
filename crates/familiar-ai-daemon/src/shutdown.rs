use tokio::signal;

/// Termination signals, registered BEFORE the daemon advertises readiness.
///
/// The handler used to be installed inside `shutdown_signal()`, which is not
/// awaited until the whole runtime is assembled — long after the PID file is
/// written. In that window SIGTERM kept its default disposition, so anything
/// that read the PID file and signalled (systemd, launchd, an operator, the
/// integration test) killed the daemon outright: no graceful shutdown, no PID
/// cleanup, no chance to finish in-flight work. Registering here and passing
/// the guard to `shutdown_signal` makes the PID file mean what it claims —
/// "I am ready, including to stop" (FAM-BUG-050).
#[cfg(unix)]
pub struct TerminationSignals {
    sigterm: signal::unix::Signal,
}

#[cfg(unix)]
impl TerminationSignals {
    pub fn register() -> std::io::Result<Self> {
        Ok(Self {
            sigterm: signal::unix::signal(signal::unix::SignalKind::terminate())?,
        })
    }
}

#[cfg(not(unix))]
pub struct TerminationSignals;

#[cfg(not(unix))]
impl TerminationSignals {
    pub fn register() -> std::io::Result<Self> {
        Ok(Self)
    }
}

#[cfg(unix)]
pub async fn shutdown_signal(signals: &mut TerminationSignals) {
    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("received SIGINT (Ctrl+C)");
        }
        _ = signals.sigterm.recv() => {
            tracing::info!("received SIGTERM");
        }
    }
}

#[cfg(not(unix))]
pub async fn shutdown_signal(_signals: &mut TerminationSignals) {
    signal::ctrl_c().await.expect("failed to listen for ctrl_c");
    tracing::info!("received Ctrl+C");
}
