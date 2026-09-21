use std::sync::{Arc, Mutex};

use familiar_ai_core::control_plane::{
    Authority, CapabilityScope, ClientClass, SchedulingPolicy, CONTROL_PROTOCOL_VERSION,
};
use familiar_ai_core::operator_ui::{
    OperatorAction, OperatorDataSource, OperatorMutation, OperatorQuery,
};
use familiar_ai_daemon::control_plane::ControlPlaneService;
use familiar_ai_daemon::local_transport::{
    ClientHello, ControlRequest, ControlResponse, LocalClient, LocalHost,
};
use familiar_ai_daemon::operator_ui::OperatorDispatcher;
use familiar_ai_storage::Database;
use serde_json::json;

struct FixtureSource(Mutex<usize>);

impl OperatorDataSource for FixtureSource {
    fn query(&self, query: OperatorQuery) -> Result<serde_json::Value, String> {
        Ok(json!({"query": format!("{query:?}")}))
    }

    fn act(&self, _: OperatorAction) -> Result<serde_json::Value, String> {
        let mut count = self.0.lock().unwrap();
        *count += 1;
        Ok(json!({"count": *count}))
    }
}

fn service() -> (ControlPlaneService, String) {
    let db = Database::open_in_memory().unwrap();
    db.run_migrations().unwrap();
    let service =
        ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 11);
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
                project_id: None,
                execution_id: None,
                attempt: None,
                worker_id: None,
                authorities: vec![Authority::Observe, Authority::Control],
            },
            60,
        )
        .unwrap();
    (service, grant.credential)
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_transport_requires_authority_and_deduplicates_mutations() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("operator.sock");
    let (service, credential) = service();
    let source = Arc::new(FixtureSource(Mutex::new(0)));
    let dispatcher = Arc::new(OperatorDispatcher::new(source.clone(), 11));
    let _host =
        LocalHost::bind_with_operator(&socket, "owner-nonce".into(), service, Some(dispatcher))
            .await
            .unwrap();

    let hello = |session_reference| ClientHello {
        protocol_version: CONTROL_PROTOCOL_VERSION,
        request_id: "desktop-test".into(),
        session_reference,
        owner_nonce: Some("owner-nonce".into()),
    };
    let mut denied = LocalClient::connect(&socket, hello(None)).await.unwrap();
    assert!(matches!(
        denied
            .call(ControlRequest::OperatorQuery {
                query: OperatorQuery::Repositories
            })
            .await
            .unwrap(),
        ControlResponse::Error(error) if error.contains("minted session")
    ));

    let mut client = LocalClient::connect(&socket, hello(Some(credential)))
        .await
        .unwrap();
    let mutation = OperatorMutation {
        request_id: "one".into(),
        idempotency_key: "one-click".into(),
        action: OperatorAction::SetProjectPaused {
            repo: "/repo".into(),
            paused: true,
        },
    };
    let first = client
        .call(ControlRequest::OperatorMutate {
            mutation: mutation.clone(),
        })
        .await
        .unwrap();
    let second = client
        .call(ControlRequest::OperatorMutate { mutation })
        .await
        .unwrap();
    assert!(matches!(first, ControlResponse::Operator(Ok(ref reply)) if !reply.duplicate));
    assert!(matches!(second, ControlResponse::Operator(Ok(ref reply)) if reply.duplicate));
    assert_eq!(*source.0.lock().unwrap(), 1);

    let events = client
        .call(ControlRequest::OperatorObserve {
            after: 0,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(
        matches!(events, ControlResponse::OperatorEvents(Ok(ref rows)) if rows.len() == 1 && rows[0].sequence == 1)
    );
}
