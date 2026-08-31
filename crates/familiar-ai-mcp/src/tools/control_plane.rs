use crate::tool::{Tool, ToolContext, ToolError, ToolRegistry};
use async_trait::async_trait;
use familiar_ai_daemon::local_transport::{ControlRequest, ControlResponse, LocalClient};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Copy)]
enum Operation {
    View,
    Progress,
    Evidence,
    Escalate,
}
struct CapabilityTool {
    name: &'static str,
    description: &'static str,
    operation: Operation,
    client: Arc<Mutex<LocalClient>>,
}

pub fn register(registry: &mut ToolRegistry, client: Arc<Mutex<LocalClient>>) {
    for (name, description, operation) in [
        (
            "control.get_assignment",
            "Read only this agent's assigned execution, warrant, and remaining reservations.",
            Operation::View,
        ),
        (
            "control.report_progress",
            "Append progress to this agent's assigned execution.",
            Operation::Progress,
        ),
        (
            "control.submit_evidence",
            "Append evidence to this agent's assigned execution.",
            Operation::Evidence,
        ),
        (
            "control.request_escalation",
            "Create a pending human gate; this never approves an escalation.",
            Operation::Escalate,
        ),
    ] {
        registry.register(Arc::new(CapabilityTool {
            name,
            description,
            operation,
            client: client.clone(),
        }));
    }
}

#[async_trait]
impl Tool for CapabilityTool {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn input_schema(&self) -> Value {
        match self.operation {
            Operation::View => json!({"type":"object","additionalProperties":false}),
            _ => {
                json!({"type":"object","properties":{"payload":{"type":"object"}},"required":["payload"],"additionalProperties":false})
            }
        }
    }
    async fn call(&self, args: Value, _: &ToolContext) -> Result<Value, ToolError> {
        let request = match self.operation {
            Operation::View => ControlRequest::AgentView,
            Operation::Progress => ControlRequest::ReportProgress {
                payload_json: payload(&args)?,
            },
            Operation::Evidence => ControlRequest::SubmitEvidence {
                payload_json: payload(&args)?,
            },
            Operation::Escalate => ControlRequest::RequestEscalation {
                request_json: payload(&args)?,
            },
        };
        match self
            .client
            .lock()
            .await
            .call(request)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?
        {
            ControlResponse::AgentView(view) => {
                serde_json::to_value(view).map_err(|e| ToolError::Internal(e.to_string()))
            }
            ControlResponse::Cursor(cursor) => Ok(json!({"accepted":true,"cursor":cursor})),
            ControlResponse::Gate(gate_id) => {
                Ok(json!({"accepted":true,"state":"pending_human","gate_id":gate_id}))
            }
            ControlResponse::Error(error) => Err(ToolError::Internal(error)),
            _ => Err(ToolError::Internal(
                "unexpected control-plane response".into(),
            )),
        }
    }
}
fn payload(args: &Value) -> Result<String, ToolError> {
    serde_json::to_string(
        args.get("payload")
            .ok_or_else(|| ToolError::InvalidParams("payload is required".into()))?,
    )
    .map_err(|e| ToolError::InvalidParams(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{storage::UnavailableStorage, tool::ToolContext};
    use familiar_ai_core::control_plane::{
        Authority, CapabilityScope, ClientClass, ExecutionMode, SchedulingPolicy, Submission,
        CONTROL_PROTOCOL_VERSION,
    };
    use familiar_ai_daemon::{
        control_plane::ControlPlaneService,
        local_transport::{ClientHello, LocalHost},
    };
    use familiar_ai_storage::Database;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    #[tokio::test]
    async fn model_surface_can_append_and_request_but_has_no_approval_tool() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("socket");
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let service = ControlPlaneService::new(
            StdArc::new(StdMutex::new(db)),
            SchedulingPolicy::default(),
            1,
        );
        service
            .register_project("p", temp.path().to_str().unwrap(), 0, None)
            .unwrap();
        let op = CapabilityScope {
            client_class: ClientClass::Operator,
            project_id: Some("p".into()),
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control],
        };
        service
            .submit(
                &op,
                &Submission {
                    execution_id: "e".into(),
                    project_id: "p".into(),
                    idempotency_key: "k".into(),
                    mode: ExecutionMode::Detached,
                    priority: 0,
                    command_json: "[]".into(),
                },
            )
            .unwrap();
        service.claim_next().unwrap();
        service
            .bind_worker("e", "w", std::process::id(), "test", "hash")
            .unwrap();
        let grant = service.mint_worker_session("e", "w").unwrap();
        let _host = LocalHost::bind(&socket, "nonce".into(), service)
            .await
            .unwrap();
        let client = LocalClient::connect(
            &socket,
            ClientHello {
                protocol_version: CONTROL_PROTOCOL_VERSION,
                request_id: "m".into(),
                session_reference: Some(grant.credential),
                owner_nonce: Some("nonce".into()),
            },
        )
        .await
        .unwrap();
        let mut registry = ToolRegistry::new();
        register(&mut registry, Arc::new(Mutex::new(client)));
        let ctx = ToolContext {
            storage: StdArc::new(UnavailableStorage),
            status: StdArc::new(StdMutex::new(familiar_ai_core::AppStatus::new())),
            config: StdArc::new(familiar_ai_core::config::Config::default()),
            router: None,
        };
        assert!(registry
            .call(
                "control.report_progress",
                json!({"payload":{"stage":"review"}}),
                &ctx
            )
            .await
            .is_ok());
        let gate = registry
            .call(
                "control.request_escalation",
                json!({"payload":{"capability":"network"}}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(gate["state"], "pending_human");
        assert!(!registry.list().iter().any(|t| t.name.contains("approve")));
    }
}
