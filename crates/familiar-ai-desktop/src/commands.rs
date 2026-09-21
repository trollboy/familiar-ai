use std::sync::Arc;

use familiar_ai_core::operator_ui::{OperatorMutation, OperatorQuery, OperatorReply};
use tokio::sync::Mutex;

use crate::client::{ConnectionStatus, DesktopClient};

pub type ClientState = Arc<Mutex<DesktopClient>>;

#[tauri::command]
pub async fn connection_status(
    state: tauri::State<'_, ClientState>,
) -> Result<ConnectionStatus, String> {
    Ok(state.lock().await.status())
}

#[tauri::command]
pub async fn operator_query(
    state: tauri::State<'_, ClientState>,
    query: OperatorQuery,
) -> Result<OperatorReply, String> {
    state.lock().await.query(query).await
}

#[tauri::command]
pub async fn operator_mutate(
    state: tauri::State<'_, ClientState>,
    mutation: OperatorMutation,
) -> Result<OperatorReply, String> {
    state.lock().await.mutate(mutation).await
}

#[tauri::command]
pub async fn operator_observe(
    state: tauri::State<'_, ClientState>,
    after: u64,
) -> Result<Vec<familiar_ai_core::operator_ui::OperatorEvent>, String> {
    state.lock().await.observe(after).await
}
