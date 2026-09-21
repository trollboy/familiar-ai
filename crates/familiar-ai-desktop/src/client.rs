use std::path::{Path, PathBuf};

use familiar_ai_core::control_plane::CONTROL_PROTOCOL_VERSION;
use familiar_ai_core::operator_ui::{
    OperatorEvent, OperatorMutation, OperatorQuery, OperatorReply, OPERATOR_PROTOCOL_VERSION,
};
use familiar_ai_core::AppPaths;
use familiar_ai_daemon::local_transport::{
    ClientHello, ControlRequest, ControlResponse, LocalClient,
};
use familiar_ai_daemon::worker_lock::{ClaimState, WorkerLock};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectionStatus {
    pub state: &'static str,
    pub message: String,
    pub generation: Option<u64>,
}

pub struct DesktopClient {
    paths: AppPaths,
    generation: Option<u64>,
}

impl DesktopClient {
    pub fn resolve() -> Result<Self, String> {
        Ok(Self {
            paths: AppPaths::resolve().map_err(|e| e.to_string())?,
            generation: None,
        })
    }

    pub fn status(&self) -> ConnectionStatus {
        match WorkerLock::inspect(&self.paths.runtime_dir) {
            Ok(ClaimState::Live(claim)) => ConnectionStatus {
                state: "connected",
                message: format!("daemon pid {} is available", claim.owner_pid),
                generation: Some(claim.generation),
            },
            Ok(ClaimState::Absent) => disconnected("daemon is not running"),
            Ok(ClaimState::Stale(_)) => disconnected("daemon ownership is stale"),
            Ok(ClaimState::Invalid(detail)) => ConnectionStatus {
                state: "incompatible",
                message: detail,
                generation: None,
            },
            Err(error) => disconnected(&format!("cannot inspect daemon: {error}")),
        }
    }

    async fn connect(&mut self) -> Result<LocalClient, String> {
        let claim = match WorkerLock::inspect(&self.paths.runtime_dir).map_err(|e| e.to_string())? {
            ClaimState::Live(claim) => claim,
            ClaimState::Absent => return Err("daemon is not running".into()),
            ClaimState::Stale(_) => {
                return Err("daemon ownership is stale; restart Familiar".into())
            }
            ClaimState::Invalid(detail) => {
                return Err(format!("daemon ownership is invalid: {detail}"))
            }
        };
        let credential_path = self.paths.runtime_dir.join("operator.credential");
        let credential = read_private(&credential_path)?;
        let client = LocalClient::connect(
            Path::new(&claim.socket_path),
            ClientHello {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                request_id: format!("desktop-{}", std::process::id()),
                session_reference: Some(credential),
                owner_nonce: Some(claim.owner_nonce),
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        self.generation = Some(claim.generation);
        Ok(client)
    }

    pub async fn query(&mut self, query: OperatorQuery) -> Result<OperatorReply, String> {
        let mut client = self.connect().await?;
        decode(
            client
                .call(ControlRequest::OperatorQuery { query })
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    pub async fn mutate(&mut self, mutation: OperatorMutation) -> Result<OperatorReply, String> {
        let mut client = self.connect().await?;
        decode(
            client
                .call(ControlRequest::OperatorMutate { mutation })
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    pub async fn observe(&mut self, after: u64) -> Result<Vec<OperatorEvent>, String> {
        let mut client = self.connect().await?;
        match client
            .call(ControlRequest::OperatorObserve { after, limit: 100 })
            .await
            .map_err(|e| e.to_string())?
        {
            ControlResponse::OperatorEvents(Ok(events)) => Ok(events),
            ControlResponse::OperatorEvents(Err(error)) => {
                Err(format!("{}: {}", error.code, error.message))
            }
            ControlResponse::Error(error) => Err(error),
            _ => Err("daemon returned the wrong operator event response".into()),
        }
    }
}

fn decode(response: ControlResponse) -> Result<OperatorReply, String> {
    match response {
        ControlResponse::Operator(Ok(reply))
            if reply.protocol_version == OPERATOR_PROTOCOL_VERSION =>
        {
            Ok(reply)
        }
        ControlResponse::Operator(Ok(reply)) => Err(format!(
            "operator protocol mismatch: desktop {OPERATOR_PROTOCOL_VERSION}, daemon {}",
            reply.protocol_version
        )),
        ControlResponse::Operator(Err(error)) => Err(error.message),
        ControlResponse::Error(error) => Err(error),
        _ => Err("daemon returned the wrong operator response".into()),
    }
}

fn read_private(path: &PathBuf) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .map_err(|e| format!("cannot read operator credential metadata: {e}"))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("operator credential permissions are not private".into());
        }
    }
    let value = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read operator credential: {e}"))?;
    if value.trim().is_empty() {
        return Err("operator credential is empty".into());
    }
    Ok(value)
}

fn disconnected(message: &str) -> ConnectionStatus {
    ConnectionStatus {
        state: "disconnected",
        message: message.into(),
        generation: None,
    }
}
