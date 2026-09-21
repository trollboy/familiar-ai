use familiar_ai_core::control_plane::CONTROL_PROTOCOL_VERSION;
use familiar_ai_core::{FamiliarError, Result};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::control_plane::ControlPlaneService;
use crate::operator_ui::OperatorDispatcher;
use crate::worker_lock::{ClaimState, WorkerLock};
use familiar_ai_core::control_plane::{
    AgentCapabilityView, ControlEvent, ExecutionRecord, Submission, SubmissionAck,
};
use familiar_ai_core::operator_ui::{
    OperatorEvent, OperatorMutation, OperatorQuery, OperatorReply,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientHello {
    pub protocol_version: u32,
    pub request_id: String,
    /// Opaque host-side session reference. It must never be included in a
    /// model-facing response, prompt, log, or accounting row.
    pub session_reference: Option<String>,
    /// Claim nonce is used only to identity-verify the advertised owner.
    pub owner_nonce: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    Hello(ClientHello),
    Health,
    RegisterProject {
        project_id: String,
        root: String,
        priority: i64,
        ceiling: Option<usize>,
    },
    SetProjectState {
        project_id: String,
        state: String,
    },
    Submit {
        submission: Submission,
    },
    Observe {
        execution_id: String,
        project_id: String,
        after: i64,
        limit: usize,
    },
    Execution {
        execution_id: String,
        project_id: String,
    },
    RequestEscalation {
        request_json: String,
    },
    Cancel {
        execution_id: String,
        project_id: String,
    },
    AgentView,
    ReportProgress {
        payload_json: String,
    },
    SubmitEvidence {
        payload_json: String,
    },
    OperatorQuery {
        query: OperatorQuery,
    },
    OperatorMutate {
        mutation: OperatorMutation,
    },
    OperatorObserve {
        after: u64,
        limit: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ControlResponse {
    Hello(ServerHello),
    Health(String),
    Submission(SubmissionAck),
    Events(Vec<ControlEvent>),
    Execution(Option<ExecutionRecord>),
    Gate(String),
    AgentView(AgentCapabilityView),
    Cursor(i64),
    Operator(std::result::Result<OperatorReply, familiar_ai_core::operator_ui::OperatorError>),
    OperatorEvents(
        std::result::Result<Vec<OperatorEvent>, familiar_ai_core::operator_ui::OperatorError>,
    ),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerHello {
    pub protocol_version: u32,
    pub request_id: String,
}

/// Running same-user Unix socket host. Removing the socket happens after all
/// accept tasks stop; ownership remains held by the caller for this lifetime.
pub struct LocalHost {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl LocalHost {
    #[cfg(unix)]
    pub async fn bind(
        path: &Path,
        owner_nonce: String,
        service: ControlPlaneService,
    ) -> Result<Self> {
        Self::bind_with_operator(path, owner_nonce, service, None).await
    }

    #[cfg(unix)]
    pub async fn bind_with_operator(
        path: &Path,
        owner_nonce: String,
        service: ControlPlaneService,
        operator: Option<std::sync::Arc<OperatorDispatcher>>,
    ) -> Result<Self> {
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if !meta.file_type().is_socket() {
                return Err(FamiliarError::Config(format!(
                    "refusing to replace non-socket control path {}",
                    path.display()
                )));
            }
            std::fs::remove_file(path).map_err(FamiliarError::Io)?;
        }
        let listener = tokio::net::UnixListener::bind(path).map_err(FamiliarError::Io)?;
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(FamiliarError::Io)?;
        let (shutdown, mut stop) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let service = service.clone();
                        let operator = operator.clone();
                        let nonce = owner_nonce.clone();
                        let mut connection_stop = stop.clone();
                        connections.spawn(async move {
                            tokio::select! {
                                result = serve_connection(stream, &nonce, service, operator) => { let _ = result; }
                                _ = connection_stop.changed() => {}
                            }
                        });
                    }
                    _ = stop.changed() => break,
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Ok(Self {
            path: path.to_path_buf(),
            task,
            shutdown,
        })
    }
}

impl Drop for LocalHost {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
async fn serve_connection(
    stream: tokio::net::UnixStream,
    nonce: &str,
    service: ControlPlaneService,
    operator: Option<std::sync::Arc<OperatorDispatcher>>,
) -> Result<()> {
    require_same_user(&stream)?;
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let first = read_bounded_line(&mut read)
        .await?
        .ok_or_else(|| FamiliarError::Config("control-plane hello is required".into()))?;
    let ControlRequest::Hello(hello) = serde_json::from_str::<ControlRequest>(&first)
        .map_err(|e| FamiliarError::Config(format!("invalid control request: {e}")))?
    else {
        return Err(FamiliarError::Config(
            "control-plane hello must be first".into(),
        ));
    };
    let response = match negotiate(&hello) {
        Ok(_h) if hello.owner_nonce.as_deref().is_some_and(|n| n != nonce) => ControlResponse::Error("control-plane owner nonce mismatch; treat the claim as stale and perform explicit recovery".into()),
        Ok(h) => ControlResponse::Hello(h), Err(e) => ControlResponse::Error(e.to_string()),
    };
    write_response(&mut write, &response).await?;
    if matches!(response, ControlResponse::Error(_)) {
        return Ok(());
    }
    let scope = hello
        .session_reference
        .as_deref()
        .map(|c| service.authenticate(c))
        .transpose();
    let mut foreground = Vec::new();
    while let Some(line) = read_bounded_line(&mut read).await? {
        let request = match serde_json::from_str::<ControlRequest>(&line) {
            Ok(r) => r,
            Err(e) => {
                write_response(
                    &mut write,
                    &ControlResponse::Error(format!("invalid control request: {e}")),
                )
                .await?;
                continue;
            }
        };
        if let ControlRequest::Submit { submission } = &request {
            if submission.mode == familiar_ai_core::control_plane::ExecutionMode::ForegroundOnly {
                foreground.push(submission.execution_id.clone());
            }
        }
        let response = match handle(
            request,
            scope.as_ref(),
            hello.session_reference.as_deref(),
            &service,
            operator.as_deref(),
        ) {
            Ok(r) => r,
            Err(e) => ControlResponse::Error(e.to_string()),
        };
        write_response(&mut write, &response).await?;
    }
    for execution in foreground {
        let _ = service.end_foreground(&execution);
    }
    Ok(())
}

fn handle(
    request: ControlRequest,
    scope: std::result::Result<
        &Option<familiar_ai_core::control_plane::CapabilityScope>,
        &FamiliarError,
    >,
    credential: Option<&str>,
    service: &ControlPlaneService,
    operator: Option<&OperatorDispatcher>,
) -> Result<ControlResponse> {
    let authorized = || {
        scope
            .map_err(|e| FamiliarError::Config(e.to_string()))?
            .as_ref()
            .ok_or_else(|| {
                FamiliarError::Config("authority denied: a valid minted session is required".into())
            })
    };
    match request {
        ControlRequest::Health => Ok(ControlResponse::Health("healthy".into())),
        ControlRequest::RegisterProject {
            project_id,
            root,
            priority,
            ceiling,
        } => {
            let auth = authorized()?;
            if !auth
                .authorities
                .contains(&familiar_ai_core::control_plane::Authority::Control)
            {
                return Err(FamiliarError::Config("authority denied: Control".into()));
            }
            service.register_project(&project_id, &root, priority, ceiling)?;
            Ok(ControlResponse::Health("registered".into()))
        }
        ControlRequest::SetProjectState { project_id, state } => {
            service.set_project_state(authorized()?, &project_id, &state)?;
            Ok(ControlResponse::Health(state))
        }
        ControlRequest::Submit { submission } => Ok(ControlResponse::Submission(
            service.submit(authorized()?, &submission)?,
        )),
        ControlRequest::Observe {
            execution_id,
            project_id,
            after,
            limit,
        } => Ok(ControlResponse::Events(service.observe(
            authorized()?,
            &execution_id,
            &project_id,
            after,
            limit,
        )?)),
        ControlRequest::Execution {
            execution_id,
            project_id,
        } => Ok(ControlResponse::Execution(service.execution(
            authorized()?,
            &execution_id,
            &project_id,
        )?)),
        ControlRequest::RequestEscalation { request_json } => {
            Ok(ControlResponse::Gate(service.request_escalation(
                credential.ok_or_else(|| {
                    FamiliarError::Config(
                        "authority denied: a valid minted session is required".into(),
                    )
                })?,
                &request_json,
            )?))
        }
        ControlRequest::Cancel {
            execution_id,
            project_id,
        } => {
            service.cancel(authorized()?, &execution_id, &project_id)?;
            Ok(ControlResponse::Health("cancelled".into()))
        }
        ControlRequest::AgentView => Ok(ControlResponse::AgentView(service.agent_view(
            credential.ok_or_else(|| {
                FamiliarError::Config("authority denied: a valid minted session is required".into())
            })?,
        )?)),
        ControlRequest::ReportProgress { payload_json } => {
            Ok(ControlResponse::Cursor(service.report_agent_event(
                credential.ok_or_else(|| {
                    FamiliarError::Config(
                        "authority denied: a valid minted session is required".into(),
                    )
                })?,
                familiar_ai_core::control_plane::Authority::ReportProgress,
                "agent_progress",
                &payload_json,
            )?))
        }
        ControlRequest::SubmitEvidence { payload_json } => {
            Ok(ControlResponse::Cursor(service.report_agent_event(
                credential.ok_or_else(|| {
                    FamiliarError::Config(
                        "authority denied: a valid minted session is required".into(),
                    )
                })?,
                familiar_ai_core::control_plane::Authority::SubmitEvidence,
                "agent_evidence",
                &payload_json,
            )?))
        }
        ControlRequest::OperatorQuery { query } => {
            let auth = authorized()?;
            if !auth
                .authorities
                .contains(&familiar_ai_core::control_plane::Authority::Observe)
            {
                return Err(FamiliarError::Config("authority denied: Observe".into()));
            }
            let dispatcher = operator
                .ok_or_else(|| FamiliarError::Config("operator UI is unavailable".into()))?;
            Ok(ControlResponse::Operator(dispatcher.query(query)))
        }
        ControlRequest::OperatorMutate { mutation } => {
            let auth = authorized()?;
            if !auth
                .authorities
                .contains(&familiar_ai_core::control_plane::Authority::Control)
            {
                return Err(FamiliarError::Config("authority denied: Control".into()));
            }
            let dispatcher = operator
                .ok_or_else(|| FamiliarError::Config("operator UI is unavailable".into()))?;
            Ok(ControlResponse::Operator(dispatcher.mutate(mutation)))
        }
        ControlRequest::OperatorObserve { after, limit } => {
            let auth = authorized()?;
            if !auth
                .authorities
                .contains(&familiar_ai_core::control_plane::Authority::Observe)
            {
                return Err(FamiliarError::Config("authority denied: Observe".into()));
            }
            let dispatcher = operator
                .ok_or_else(|| FamiliarError::Config("operator UI is unavailable".into()))?;
            Ok(ControlResponse::OperatorEvents(
                dispatcher.observe(after, limit),
            ))
        }
        ControlRequest::Hello(_) => Err(FamiliarError::Config(
            "duplicate control-plane hello".into(),
        )),
    }
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    write: &mut W,
    response: &ControlResponse,
) -> Result<()> {
    let mut bytes =
        serde_json::to_vec(response).map_err(|e| FamiliarError::Config(e.to_string()))?;
    if bytes.len() > familiar_ai_core::operator_ui::MAX_OPERATOR_FRAME_BYTES {
        return Err(FamiliarError::Config(
            "local control response exceeds the bounded frame size".into(),
        ));
    }
    bytes.push(b'\n');
    write.write_all(&bytes).await.map_err(FamiliarError::Io)
}

pub struct LocalClient {
    read: BufReader<tokio::net::unix::OwnedReadHalf>,
    write: tokio::net::unix::OwnedWriteHalf,
}

pub enum MutationOwner {
    Remote(LocalClient),
    InProcess(WorkerLock),
}

/// Resolve mutation authority without ever treating connection failure as
/// evidence that a live owner is absent.
pub async fn resolve_mutation_owner(
    runtime_dir: &Path,
    credential: Option<String>,
    timeout: std::time::Duration,
) -> Result<MutationOwner> {
    for _ in 0..2 {
        match WorkerLock::inspect(runtime_dir).map_err(FamiliarError::Io)? {
            ClaimState::Absent | ClaimState::Stale(_) => match WorkerLock::acquire(runtime_dir) {
                Ok(lock) => return Ok(MutationOwner::InProcess(lock)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(FamiliarError::Io(error)),
            },
            ClaimState::Invalid(detail) => return Err(FamiliarError::Config(format!(
                "control-plane claim is invalid ({detail}); inspect daemon status and logs, then perform explicit operator recovery"
            ))),
            ClaimState::Live(claim) => {
                let connect = LocalClient::connect(
                    Path::new(&claim.socket_path),
                    ClientHello {
                        protocol_version: CONTROL_PROTOCOL_VERSION,
                        request_id: format!("client-{}", std::process::id()),
                        session_reference: credential.clone(),
                        owner_nonce: Some(claim.owner_nonce.clone()),
                    },
                );
                return match tokio::time::timeout(timeout, connect).await {
                    Ok(Ok(client)) => Ok(MutationOwner::Remote(client)),
                    Ok(Err(error)) => Err(FamiliarError::Config(format!(
                        "control-plane owner pid {} is live but unusable ({error}); wait for startup, inspect daemon status and logs, upgrade the stale side, or perform explicit operator recovery",
                        claim.owner_pid
                    ))),
                    Err(_) => Err(FamiliarError::Config(format!(
                        "control-plane owner pid {} is live but unresponsive; inspect daemon status and logs, then perform explicit operator recovery",
                        claim.owner_pid
                    ))),
                };
            }
        }
    }
    Err(FamiliarError::Config(
        "control-plane ownership changed repeatedly; retry after inspecting daemon status".into(),
    ))
}
impl LocalClient {
    pub async fn connect(path: &Path, hello: ClientHello) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .map_err(FamiliarError::Io)?;
        let (read, write) = stream.into_split();
        let mut client = Self {
            read: BufReader::new(read),
            write,
        };
        match client.call(ControlRequest::Hello(hello)).await? {
            ControlResponse::Hello(_) => Ok(client),
            ControlResponse::Error(e) => Err(FamiliarError::Config(e)),
            _ => Err(FamiliarError::Config(
                "invalid control-plane handshake response".into(),
            )),
        }
    }
    pub async fn call(&mut self, request: ControlRequest) -> Result<ControlResponse> {
        let mut b =
            serde_json::to_vec(&request).map_err(|e| FamiliarError::Config(e.to_string()))?;
        if b.len() > familiar_ai_core::operator_ui::MAX_OPERATOR_FRAME_BYTES {
            return Err(FamiliarError::Config(
                "local control request exceeds the bounded frame size".into(),
            ));
        }
        b.push(b'\n');
        self.write.write_all(&b).await.map_err(FamiliarError::Io)?;
        let line = read_bounded_line(&mut self.read).await?.ok_or_else(|| {
            FamiliarError::Config("control-plane owner closed the connection".into())
        })?;
        serde_json::from_str(&line)
            .map_err(|e| FamiliarError::Config(format!("invalid control-plane response: {e}")))
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(read: &mut R) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = read.fill_buf().await.map_err(FamiliarError::Io)?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if bytes.len() + take > familiar_ai_core::operator_ui::MAX_OPERATOR_FRAME_BYTES {
            return Err(FamiliarError::Config(format!(
                "local control frame exceeds {} bytes",
                familiar_ai_core::operator_ui::MAX_OPERATOR_FRAME_BYTES
            )));
        }
        bytes.extend_from_slice(&available[..take]);
        read.consume(take);
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            break;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| FamiliarError::Config("local control frame is not UTF-8".into()))
}

pub fn negotiate(hello: &ClientHello) -> Result<ServerHello> {
    if hello.protocol_version != CONTROL_PROTOCOL_VERSION {
        return Err(FamiliarError::Config(format!(
            "control-plane protocol mismatch: client {}, owner {}; upgrade the stale {} and retry",
            hello.protocol_version,
            CONTROL_PROTOCOL_VERSION,
            if hello.protocol_version < CONTROL_PROTOCOL_VERSION {
                "client"
            } else {
                "daemon"
            }
        )));
    }
    if hello.request_id.is_empty() {
        return Err(FamiliarError::Config(
            "control-plane request id is required".into(),
        ));
    }
    Ok(ServerHello {
        protocol_version: CONTROL_PROTOCOL_VERSION,
        request_id: hello.request_id.clone(),
    })
}

#[cfg(unix)]
pub fn require_same_user(stream: &tokio::net::UnixStream) -> Result<()> {
    let peer = stream.peer_cred().map_err(FamiliarError::Io)?;
    if peer.uid() != unsafe { libc::geteuid() } {
        return Err(FamiliarError::Config(
            "local socket refused a foreign-user peer".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::control_plane::{
        Authority, CapabilityScope, ClientClass, SchedulingPolicy,
    };
    use familiar_ai_storage::Database;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn oversized_local_frame_is_rejected_before_allocation_can_grow_unbounded() {
        let input = vec![b'x'; familiar_ai_core::operator_ui::MAX_OPERATOR_FRAME_BYTES + 1];
        let mut reader = tokio::io::BufReader::new(input.as_slice());
        let error = read_bounded_line(&mut reader)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("exceeds"));
    }

    #[test]
    fn stale_protocol_names_remedy() {
        let e = negotiate(&ClientHello {
            protocol_version: 0,
            request_id: "r".into(),
            session_reference: None,
            owner_nonce: None,
        })
        .unwrap_err()
        .to_string();
        assert!(e.contains("upgrade the stale client"));
    }

    #[tokio::test]
    async fn bare_same_user_is_denied_and_scoped_observer_cannot_submit() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let service =
            ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 1);
        service.register_project("p", "/p", 0, None).unwrap();
        let host_scope = CapabilityScope {
            client_class: ClientClass::Internal,
            project_id: None,
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control],
        };
        let grant = service
            .mint_session(
                &host_scope,
                CapabilityScope {
                    client_class: ClientClass::Observer,
                    project_id: Some("p".into()),
                    execution_id: None,
                    attempt: None,
                    worker_id: None,
                    authorities: vec![Authority::Observe],
                },
                60,
            )
            .unwrap();
        let _host = LocalHost::bind(&socket, "nonce".into(), service)
            .await
            .unwrap();
        let hello = |session| ClientHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id: "r".into(),
            session_reference: session,
            owner_nonce: Some("nonce".into()),
        };
        let mut bare = LocalClient::connect(&socket, hello(None)).await.unwrap();
        let denied = bare
            .call(ControlRequest::Submit {
                submission: Submission {
                    execution_id: "e".into(),
                    project_id: "p".into(),
                    idempotency_key: "k".into(),
                    mode: familiar_ai_core::control_plane::ExecutionMode::Detached,
                    priority: 0,
                    command_json: "[]".into(),
                },
            })
            .await
            .unwrap();
        assert!(matches!(denied,ControlResponse::Error(ref e) if e.contains("minted session")));
        let mut observer = LocalClient::connect(&socket, hello(Some(grant.credential)))
            .await
            .unwrap();
        let denied = observer
            .call(ControlRequest::RegisterProject {
                project_id: "q".into(),
                root: "/q".into(),
                priority: 0,
                ceiling: None,
            })
            .await
            .unwrap();
        assert!(matches!(denied,ControlResponse::Error(ref e) if e.contains("Control")));
    }

    #[tokio::test]
    async fn resolver_claims_absence_and_fails_closed_for_live_unbound_owner() {
        let absent = tempfile::tempdir().unwrap();
        assert!(matches!(
            resolve_mutation_owner(absent.path(), None, std::time::Duration::from_millis(50))
                .await
                .unwrap(),
            MutationOwner::InProcess(_)
        ));

        let live = tempfile::tempdir().unwrap();
        let lock = WorkerLock::acquire(live.path()).unwrap();
        let error = resolve_mutation_owner(live.path(), None, std::time::Duration::from_millis(50))
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(&format!("owner pid {} is live", std::process::id())));
        assert!(error.contains("wait for startup"));
        drop(lock);
    }

    #[tokio::test]
    async fn disconnect_only_ends_explicit_foreground_work() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let service =
            ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 1);
        service.register_project("p", "/p", 0, None).unwrap();
        let internal = CapabilityScope {
            client_class: ClientClass::Internal,
            project_id: None,
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control],
        };
        let grant = service
            .mint_session(
                &internal,
                CapabilityScope {
                    client_class: ClientClass::Operator,
                    project_id: Some("p".into()),
                    execution_id: None,
                    attempt: None,
                    worker_id: None,
                    authorities: vec![Authority::Control, Authority::Observe],
                },
                60,
            )
            .unwrap();
        let _host = LocalHost::bind(&socket, "nonce".into(), service.clone())
            .await
            .unwrap();
        let mut client = LocalClient::connect(
            &socket,
            ClientHello {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                request_id: "disconnect-test".into(),
                session_reference: Some(grant.credential),
                owner_nonce: Some("nonce".into()),
            },
        )
        .await
        .unwrap();
        for (id, key, mode) in [
            (
                "attached",
                "attached-key",
                familiar_ai_core::control_plane::ExecutionMode::Attached,
            ),
            (
                "foreground",
                "foreground-key",
                familiar_ai_core::control_plane::ExecutionMode::ForegroundOnly,
            ),
        ] {
            assert!(matches!(
                client
                    .call(ControlRequest::Submit {
                        submission: Submission {
                            execution_id: id.into(),
                            project_id: "p".into(),
                            idempotency_key: key.into(),
                            mode,
                            priority: 0,
                            command_json: "[]".into(),
                        }
                    })
                    .await
                    .unwrap(),
                ControlResponse::Submission(_)
            ));
        }
        drop(client);
        for _ in 0..50 {
            if service
                .execution_internal("foreground")
                .unwrap()
                .is_some_and(|row| {
                    row.state == familiar_ai_core::control_plane::ExecutionState::ForegroundEnded
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            service
                .execution_internal("attached")
                .unwrap()
                .unwrap()
                .state,
            familiar_ai_core::control_plane::ExecutionState::Queued
        );
        assert_eq!(
            service
                .execution_internal("foreground")
                .unwrap()
                .unwrap()
                .state,
            familiar_ai_core::control_plane::ExecutionState::ForegroundEnded
        );
    }
}
