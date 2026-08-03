use tokio::signal;

#[cfg(unix)]
pub async fn shutdown_signal() {
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");

    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("received SIGINT (Ctrl+C)");
        }
        _ = sigterm.recv() => {
            tracing::info!("received SIGTERM");
        }
    }
}

#[cfg(not(unix))]
pub async fn shutdown_signal() {
    signal::ctrl_c().await.expect("failed to listen for ctrl_c");
    tracing::info!("received Ctrl+C");
}
