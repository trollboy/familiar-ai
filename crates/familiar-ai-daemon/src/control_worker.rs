use std::process::Stdio;
use std::time::Duration;

use familiar_ai_core::control_plane::ExecutionState;
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::control_plane::ControlPlaneService;

#[derive(serde::Deserialize)]
struct CommandSpec {
    argv: Vec<String>,
    timeout_ms: Option<u64>,
}

/// Claims durable work and supervises each child independently of clients.
pub async fn run(
    service: ControlPlaneService,
    capability_dir: std::path::PathBuf,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        while let Ok(Some(id)) = service.claim_next() {
            let svc = service.clone();
            let capability_dir = capability_dir.clone();
            let worker_shutdown = shutdown.clone();
            tasks.spawn(async move {
                execute(svc, id, capability_dir, worker_shutdown).await;
            });
        }
        tokio::select! {
            _=tokio::time::sleep(Duration::from_millis(100))=>{},
            _=shutdown.changed()=>break,
            Some(_)=tasks.join_next(), if !tasks.is_empty()=>{},
        }
    }
    while tasks.join_next().await.is_some() {}
}

async fn execute(
    service: ControlPlaneService,
    id: String,
    capability_dir: std::path::PathBuf,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let Ok(Some(record)) = service.execution_internal(&id) else {
        return;
    };
    if *shutdown.borrow() {
        let _ = service.finish(
            &id,
            ExecutionState::Failed,
            "daemon_shutdown_before_worker_launch",
        );
        return;
    }
    let spec = serde_json::from_str::<CommandSpec>(&record.command_json).or_else(|_| {
        serde_json::from_str::<Vec<String>>(&record.command_json).map(|argv| CommandSpec {
            argv,
            timeout_ms: None,
        })
    });
    let Ok(spec) = spec else {
        let _ = service.finish(&id, ExecutionState::Failed, "invalid_command_specification");
        return;
    };
    let argv = spec.argv;
    let Some(program) = argv.first() else {
        let _ = service.finish(&id, ExecutionState::Failed, "empty_command_specification");
        return;
    };
    if std::fs::create_dir_all(&capability_dir).is_err() {
        let _ = service.finish(&id, ExecutionState::Failed, "worker_sandbox_setup_failed");
        return;
    }
    let denied = capability_dir.parent().unwrap_or(&capability_dir);
    let Ok(std_command) = familiar_ai_agent::isolated_command("/bin/sh", Some(denied)) else {
        let _ = service.finish(&id, ExecutionState::Failed, "worker_sandbox_unavailable");
        return;
    };
    let mut command = Command::from(std_command);
    command
        .args([
            "-c",
            "IFS= read -r launch_token; exec \"$@\" </dev/null",
            "familiar-ai-control-worker",
        ])
        .arg(program)
        .args(&argv[1..])
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(Some(root)) = service.project_root(&record.project_id) {
        command.current_dir(root);
    }
    #[cfg(unix)]
    {
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
    }
    let Ok(mut child) = command.spawn() else {
        let _ = service.finish(&id, ExecutionState::Failed, "worker_spawn_failed");
        return;
    };
    let mut token = [0u8; 32];
    if SystemRandom::new().fill(&mut token).is_err() {
        let _ = child.kill().await;
        let _ = service.finish(&id, ExecutionState::Failed, "worker_identity_failed");
        return;
    }
    let identity = format!("{}:{}", id, record.attempt);
    let hash = digest::digest(&digest::SHA256, &token)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let pid = child.id().unwrap_or(0);
    let start = process_start_identity(pid).unwrap_or_else(|| "unavailable".into());
    if service
        .bind_worker(&id, &identity, pid, &start, &hash)
        .is_err()
    {
        let _ = child.kill().await;
        return;
    }
    let grant = match service.mint_worker_session(&id, &identity) {
        Ok(value) => value,
        Err(_) => {
            let _ = child.kill().await;
            let _ = service.finish(&id, ExecutionState::Failed, "worker_capability_mint_failed");
            return;
        }
    };
    let path = capability_dir.join(format!("{}.session", identity.replace(':', "_")));
    #[cfg(unix)]
    let stored = {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        file.and_then(|mut file| {
            std::io::Write::write_all(&mut file, grant.credential.as_bytes())?;
            file.sync_all()
        })
    };
    #[cfg(not(unix))]
    let stored = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| {
            std::io::Write::write_all(&mut file, grant.credential.as_bytes())?;
            file.sync_all()
        });
    if stored.is_err() {
        let _ = child.kill().await;
        let _ = service.revoke_session(&grant.credential);
        let _ = service.finish(
            &id,
            ExecutionState::Failed,
            "worker_capability_store_failed",
        );
        return;
    }
    let mut session_cleanup = Some((path, grant.credential));
    let launch_token = token.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let gate_result = match child.stdin.take() {
        Some(mut input) => {
            input
                .write_all(format!("{launch_token}\n").as_bytes())
                .await
        }
        None => Err(std::io::Error::other(
            "worker launch gate stdin unavailable",
        )),
    };
    if gate_result.is_err() {
        let _ = child.kill().await;
        if let Some((path, credential)) = session_cleanup.take() {
            let _ = std::fs::remove_file(path);
            let _ = service.revoke_session(&credential);
        }
        let _ = service.finish(&id, ExecutionState::Failed, "worker_launch_gate_failed");
        return;
    }
    let outcome = tokio::select! {
        status=child.wait()=>Some(status),
        _=async {if *shutdown.borrow(){return;} let _=shutdown.changed().await;}=>{
            #[cfg(unix)] if pid<=i32::MAX as u32 {unsafe{libc::kill(-(pid as i32),libc::SIGTERM)};}
            let _=child.wait().await;
            None
        },
        _=async {match spec.timeout_ms {Some(ms)=>tokio::time::sleep(Duration::from_millis(ms)).await,None=>std::future::pending::<()>().await}}=>{
            #[cfg(unix)] if pid<=i32::MAX as u32 {unsafe{libc::kill(-(pid as i32),libc::SIGKILL)};}
            let _=child.wait().await;let _=service.finish(&id,ExecutionState::Failed,"worker_timeout");
            if let Some((path,credential))=session_cleanup {let _=std::fs::remove_file(path);let _=service.revoke_session(&credential);}return;
        },
    };
    match outcome {
        None => {
            let _ = service.finish(
                &id,
                ExecutionState::Failed,
                "daemon_shutdown_runner_interrupted",
            );
        }
        Some(Ok(status)) if status.success() => {
            let _ = service.finish(&id, ExecutionState::Completed, "worker_completed");
        }
        Some(Ok(_)) => {
            let _ = service.finish(&id, ExecutionState::Failed, "worker_failed");
        }
        Some(Err(_)) => {
            let _ = service.finish(&id, ExecutionState::Failed, "worker_wait_failed");
        }
    }
    if let Some((path, credential)) = session_cleanup {
        let _ = std::fs::remove_file(path);
        let _ = service.revoke_session(&credential);
    }
}

pub fn process_start_identity(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()?
            .split_whitespace()
            .nth(21)
            .map(str::to_owned)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let o = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    }
}

pub fn verified_live_workers(
    service: &ControlPlaneService,
) -> familiar_ai_core::Result<Vec<String>> {
    Ok(service
        .live_worker_candidates()?
        .into_iter()
        .filter(|(_, pid, start)| process_start_identity(*pid).as_deref() == Some(start))
        .map(|(id, _, _)| id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::control_plane::{
        Authority, CapabilityScope, ClientClass, ExecutionMode, SchedulingPolicy, Submission,
    };
    use familiar_ai_storage::Database;
    use std::sync::{Arc, Mutex};
    #[tokio::test]
    async fn detached_command_completes_without_a_client_and_is_queryable() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = temp.path().join("done");
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let svc =
            ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 7);
        svc.register_project("p", temp.path().to_str().unwrap(), 0, None)
            .unwrap();
        let op = CapabilityScope {
            client_class: ClientClass::Operator,
            project_id: Some("p".into()),
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control, Authority::Observe],
        };
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".into(),
            format!("printf complete > '{}'", output.display()),
        ];
        svc.submit(
            &op,
            &Submission {
                execution_id: "e".into(),
                project_id: "p".into(),
                idempotency_key: "k".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: serde_json::to_string(&argv).unwrap(),
            },
        )
        .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let worker = tokio::spawn(run(svc.clone(), runtime.path().join("capabilities"), rx));
        for _ in 0..100 {
            if svc
                .execution(&op, "e", "p")
                .unwrap()
                .is_some_and(|r| r.state == ExecutionState::Completed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tx.send(true).unwrap();
        worker.await.unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "complete");
        assert_eq!(
            svc.execution(&op, "e", "p").unwrap().unwrap().state,
            ExecutionState::Completed
        );
    }

    #[tokio::test]
    async fn orderly_shutdown_terminates_the_worker_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = temp.path().join("must-not-exist");
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let svc =
            ControlPlaneService::new(Arc::new(Mutex::new(db)), SchedulingPolicy::default(), 1);
        svc.register_project("p", temp.path().to_str().unwrap(), 0, None)
            .unwrap();
        let op = CapabilityScope {
            client_class: ClientClass::Operator,
            project_id: Some("p".into()),
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control, Authority::Observe],
        };
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("sleep 1; touch '{}'", output.display()),
        ];
        svc.submit(
            &op,
            &Submission {
                execution_id: "e".into(),
                project_id: "p".into(),
                idempotency_key: "k".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: serde_json::to_string(&argv).unwrap(),
            },
        )
        .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(run(svc.clone(), runtime.path().join("caps"), rx));
        for _ in 0..50 {
            if svc
                .execution(&op, "e", "p")
                .unwrap()
                .is_some_and(|r| r.worker_identity.is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tx.send(true).unwrap();
        task.await.unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!output.exists());
        assert_eq!(
            svc.execution(&op, "e", "p").unwrap().unwrap().state,
            ExecutionState::Failed
        );
    }

    #[tokio::test]
    async fn timeout_kills_process_group_and_records_unknown_usage() {
        let project = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let marker = project.path().join("late");
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(Mutex::new(db));
        let svc = ControlPlaneService::new(db.clone(), SchedulingPolicy::default(), 1);
        svc.register_project("p", project.path().to_str().unwrap(), 0, None)
            .unwrap();
        let op = CapabilityScope {
            client_class: ClientClass::Operator,
            project_id: Some("p".into()),
            execution_id: None,
            attempt: None,
            worker_id: None,
            authorities: vec![Authority::Control, Authority::Observe],
        };
        let spec = serde_json::json!({"argv":["/bin/sh","-c",format!("sleep 1; touch '{}'",marker.display())],"timeout_ms":50});
        svc.submit(
            &op,
            &Submission {
                execution_id: "e".into(),
                project_id: "p".into(),
                idempotency_key: "k".into(),
                mode: ExecutionMode::Detached,
                priority: 0,
                command_json: spec.to_string(),
            },
        )
        .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(run(svc.clone(), runtime.path().join("caps"), rx));
        for _ in 0..100 {
            if svc
                .execution(&op, "e", "p")
                .unwrap()
                .is_some_and(|r| r.state == ExecutionState::Failed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tx.send(true).unwrap();
        task.await.unwrap();
        assert!(!marker.exists());
        let payload:String=db.lock().unwrap().conn().query_row("SELECT payload_json FROM control_plane_events WHERE execution_id='e' AND kind='usage_finalized'",[],|r|r.get(0)).unwrap();
        assert_eq!(payload, "{\"known\":false}");
    }
}
