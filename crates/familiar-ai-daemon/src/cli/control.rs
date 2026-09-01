//! `familiar-ai control` — submit and observe daemon-owned detached
//! executions.

use clap::Subcommand;
use familiar_ai_core::{AppPaths, Config};
use familiar_ai_storage::Database;

#[derive(Debug, Subcommand)]
pub enum ControlCommand {
    Register {
        project_id: String,
        #[arg(long, default_value = ".")]
        root: std::path::PathBuf,
        #[arg(long, default_value_t = 0)]
        priority: i64,
        #[arg(long)]
        ceiling: Option<usize>,
    },
    Submit {
        project_id: String,
        #[arg(long)]
        idempotency_key: String,
        #[arg(long, default_value_t = 0)]
        priority: i64,
        #[arg(long)]
        timeout_ms: Option<u64>,
        #[arg(long, conflicts_with = "attached")]
        foreground_only: bool,
        #[arg(long, conflicts_with = "foreground_only")]
        attached: bool,
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    Attach {
        project_id: String,
        execution_id: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    Show {
        project_id: String,
        execution_id: String,
    },
    ProjectState {
        project_id: String,
        #[arg(value_parser=["active","paused","archived"])]
        state: String,
    },
    Cancel {
        project_id: String,
        execution_id: String,
    },
}

pub fn control_command(command: ControlCommand) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        use crate::local_transport::{ClientHello,ControlRequest,ControlResponse,LocalClient,MutationOwner,resolve_mutation_owner};
        use familiar_ai_core::control_plane::{Authority,CapabilityScope,ClientClass,ExecutionMode,SchedulingPolicy,Submission,CONTROL_PROTOCOL_VERSION};
        let paths=AppPaths::resolve().map_err(|e|e.to_string())?;
        let persistent_installation=paths.data_dir.join("installation-id");
        if persistent_installation.exists(){std::fs::copy(&persistent_installation,paths.runtime_dir.join("installation-id")).map_err(|e|e.to_string())?;}
        let persistent_generation=paths.data_dir.join("control-plane.generation");
        if persistent_generation.exists(){std::fs::copy(&persistent_generation,paths.runtime_dir.join("control-plane.generation")).map_err(|e|e.to_string())?;}
        let base_config=Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e|e.to_string())?;
        let credential=std::fs::read_to_string(paths.runtime_dir.join("operator.credential")).ok().map(|v|v.trim().to_owned());
        let resolved=resolve_mutation_owner(&paths.runtime_dir,credential.clone(),std::time::Duration::from_millis(base_config.daemon.health_timeout_ms)).await.map_err(|e|e.to_string())?;
        let mut local_owner=None;let mut local_host=None;
        let mut client=match resolved {
            MutationOwner::Remote(client)=>client,
            MutationOwner::InProcess(lock)=>{
                if !persistent_installation.exists(){std::fs::write(&persistent_installation,format!("{}\n",lock.claim().installation_id)).map_err(|e|e.to_string())?;}
                std::fs::write(&persistent_generation,format!("{}\n",lock.claim().generation)).map_err(|e|e.to_string())?;
                if matches!(&command,ControlCommand::Submit{foreground_only:false,..}) {return Err("detached submission requires the resident daemon; start `familiar-ai-daemon` and retry".into());}
                let config=base_config.clone();let db=std::sync::Arc::new(std::sync::Mutex::new(Database::open(&config.database.resolve_path(&paths.data_dir)).map_err(|e|e.to_string())?));db.lock().unwrap().run_migrations().map_err(|e|e.to_string())?;
                let service=crate::control_plane::ControlPlaneService::new(db,SchedulingPolicy{global_ceiling:config.daemon.global_concurrency_ceiling,default_project_ceiling:config.daemon.default_project_concurrency_ceiling},lock.claim().generation);let internal=CapabilityScope{client_class:ClientClass::Internal,project_id:None,execution_id:None,attempt:None,worker_id:None,authorities:vec![Authority::Control]};let grant=service.mint_session(&internal,CapabilityScope{client_class:ClientClass::Operator,project_id:None,execution_id:None,attempt:None,worker_id:None,authorities:vec![Authority::Control,Authority::Observe]},300).map_err(|e|e.to_string())?;
                let socket=std::path::PathBuf::from(&lock.claim().socket_path);let host=crate::local_transport::LocalHost::bind(&socket,lock.claim().owner_nonce.clone(),service).await.map_err(|e|e.to_string())?;let connected=LocalClient::connect(&socket,ClientHello{protocol_version:CONTROL_PROTOCOL_VERSION,request_id:format!("cli-fallback-{}",std::process::id()),session_reference:Some(grant.credential),owner_nonce:Some(lock.claim().owner_nonce.clone())}).await.map_err(|e|e.to_string())?;local_host=Some(host);local_owner=Some(lock);connected
            }
        };
        match command {
            ControlCommand::Register{project_id,root,priority,ceiling}=>{
                let root=root.canonicalize().map_err(|e|format!("cannot resolve project root: {e}"))?;
                expect_ok(client.call(ControlRequest::RegisterProject{project_id,root:root.to_string_lossy().into_owned(),priority,ceiling}).await.map_err(|e|e.to_string())?)?;
            }
            ControlCommand::Submit{project_id,idempotency_key,priority,timeout_ms,foreground_only,attached,command}=>{
                let execution_id=format!("exec-{}-{}",chrono::Utc::now().timestamp_micros(),std::process::id());
                let command_json=serde_json::to_string(&serde_json::json!({"argv":command,"timeout_ms":timeout_ms})).map_err(|e|e.to_string())?;
                let mode=if foreground_only{ExecutionMode::ForegroundOnly}else if attached{ExecutionMode::Attached}else{ExecutionMode::Detached};
                match client.call(ControlRequest::Submit{submission:Submission{execution_id,project_id:project_id.clone(),idempotency_key,mode,priority,command_json}}).await.map_err(|e|e.to_string())? {
                    ControlResponse::Submission(ack)=>{
                        println!("execution_id={} duplicate={} cursor={}",ack.execution_id,ack.duplicate,ack.event_cursor);
                        if attached {
                            attach_control(&mut client, &project_id, &ack.execution_id, 0).await?;
                        }
                    }, other=>expect_ok(other)?,
                }
            }
            ControlCommand::Attach{project_id,execution_id,after}=>attach_control(&mut client,&project_id,&execution_id,after).await?,
            ControlCommand::Show{project_id,execution_id}=>match client.call(ControlRequest::Execution{execution_id,project_id}).await.map_err(|e|e.to_string())? {ControlResponse::Execution(row)=>println!("{}",serde_json::to_string(&row).map_err(|e|e.to_string())?),other=>expect_ok(other)?},
            ControlCommand::ProjectState{project_id,state}=>expect_ok(client.call(ControlRequest::SetProjectState{project_id,state}).await.map_err(|e|e.to_string())?)?,
            ControlCommand::Cancel{project_id,execution_id}=>expect_ok(client.call(ControlRequest::Cancel{project_id,execution_id}).await.map_err(|e|e.to_string())?)?,
        }
        drop(local_host);drop(local_owner);
        Ok(())
    })
}

async fn attach_control(
    client: &mut crate::local_transport::LocalClient,
    project_id: &str,
    execution_id: &str,
    mut after: i64,
) -> Result<(), String> {
    use crate::local_transport::{ControlRequest, ControlResponse};
    use familiar_ai_core::control_plane::ExecutionState;
    loop {
        match client
            .call(ControlRequest::Observe {
                execution_id: execution_id.to_owned(),
                project_id: project_id.to_owned(),
                after,
                limit: 1000,
            })
            .await
            .map_err(|e| e.to_string())?
        {
            ControlResponse::Events(events) => {
                for event in events {
                    after = event.cursor;
                    println!("{} {} {}", event.cursor, event.kind, event.payload_json);
                }
            }
            other => expect_ok(other)?,
        }
        match client
            .call(ControlRequest::Execution {
                execution_id: execution_id.to_owned(),
                project_id: project_id.to_owned(),
            })
            .await
            .map_err(|e| e.to_string())?
        {
            ControlResponse::Execution(Some(row))
                if !matches!(
                    row.state,
                    ExecutionState::Queued | ExecutionState::Running | ExecutionState::Paused
                ) =>
            {
                break
            }
            ControlResponse::Execution(None) => return Err("execution not found".into()),
            ControlResponse::Error(error) => return Err(error),
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Ok(())
}

fn expect_ok(response: crate::local_transport::ControlResponse) -> Result<(), String> {
    match response {
        crate::local_transport::ControlResponse::Error(e) => Err(e),
        _ => Ok(()),
    }
}
