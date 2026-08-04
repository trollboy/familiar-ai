use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use thiserror::Error;

use crate::{evidence::content_hash, EvidenceRef, VerificationEvidence, VerificationStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCheck {
    pub check_id: String,
    pub argv: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
    pub timeout_ms: u64,
    pub required: bool,
    pub path_prefixes: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationPlan {
    pub plan_id: String,
    pub checks: Vec<VerificationCheck>,
    pub full_after_remediation: bool,
}
pub trait VerificationRunner {
    fn run(
        &self,
        repository: &Path,
        check: &VerificationCheck,
        tested_identity: &str,
    ) -> Result<VerificationEvidence, VerificationError>;
}
#[derive(Debug, Clone)]
pub struct CommandVerificationRunner {
    artifact_directory: PathBuf,
    max_log_bytes: usize,
}
impl CommandVerificationRunner {
    pub fn new(artifact_directory: PathBuf, max_log_bytes: usize) -> Self {
        Self {
            artifact_directory,
            max_log_bytes,
        }
    }
}
impl VerificationRunner for CommandVerificationRunner {
    fn run(
        &self,
        repository: &Path,
        check: &VerificationCheck,
        tested_identity: &str,
    ) -> Result<VerificationEvidence, VerificationError> {
        let executable = check.argv.first().ok_or(VerificationError::EmptyArgv)?;
        let started = Utc::now();
        let timer = Instant::now();
        let mut command = Command::new(executable);
        command
            .args(&check.argv[1..])
            .current_dir(repository.join(&check.working_directory))
            .env_clear()
            .envs(&check.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let mut child_stdout = child.stdout.take().ok_or(VerificationError::MissingPipe)?;
        let mut child_stderr = child.stderr.take().ok_or(VerificationError::MissingPipe)?;
        let stdout_thread = thread::spawn(move || {
            let mut bytes = Vec::new();
            child_stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr_thread = thread::spawn(move || {
            let mut bytes = Vec::new();
            child_stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Duration::from_millis(check.timeout_ms);
        let (exit, timed_out) = loop {
            if let Some(status) = child.try_wait()? {
                break (status, false);
            }
            if timer.elapsed() >= deadline {
                child.kill()?;
                break (child.wait()?, true);
            }
            thread::sleep(Duration::from_millis(5));
        };
        let output_stdout = stdout_thread
            .join()
            .map_err(|_| VerificationError::ReaderPanicked)??;
        let output_stderr = stderr_thread
            .join()
            .map_err(|_| VerificationError::ReaderPanicked)??;
        let ended = Utc::now();
        fs::create_dir_all(&self.artifact_directory)?;
        let (stdout, stdout_redacted) = artifact(
            &self.artifact_directory,
            "stdout",
            &output_stdout,
            repository,
            tested_identity,
            self.max_log_bytes,
        )?;
        let (stderr, stderr_redacted) = artifact(
            &self.artifact_directory,
            "stderr",
            &output_stderr,
            repository,
            tested_identity,
            self.max_log_bytes,
        )?;
        let status = if stdout_redacted || stderr_redacted {
            VerificationStatus::Inconclusive
        } else if timed_out {
            VerificationStatus::TimedOut
        } else if exit.success() {
            VerificationStatus::Passed
        } else if signal(&exit).is_some() {
            VerificationStatus::Signaled
        } else {
            VerificationStatus::Failed
        };
        Ok(VerificationEvidence {
            check_id: check.check_id.clone(),
            argv: check.argv.clone(),
            working_directory: check.working_directory.clone(),
            environment_identity: check
                .environment
                .iter()
                .map(|(k, v)| (k.clone(), content_hash(v.as_bytes())))
                .collect(),
            tool_identity: None,
            tested_identity: tested_identity.into(),
            started_at: started.to_rfc3339(),
            ended_at: ended.to_rfc3339(),
            duration_ms: u64::try_from(timer.elapsed().as_millis())
                .map_err(|_| VerificationError::Overflow)?,
            exit_code: exit.code(),
            signal: signal(&exit),
            status,
            required: check.required,
            summary: if stdout_redacted || stderr_redacted {
                "Inconclusive: deterministic secret marker redacted from verification output".into()
            } else {
                format!("{status:?}")
            },
            stdout: Some(stdout),
            stderr: Some(stderr),
            truncated: output_stdout.len() > self.max_log_bytes
                || output_stderr.len() > self.max_log_bytes
                || stdout_redacted
                || stderr_redacted,
        })
    }
}
fn artifact(
    dir: &Path,
    label: &str,
    bytes: &[u8],
    repo: &Path,
    id: &str,
    max: usize,
) -> Result<(EvidenceRef, bool), VerificationError> {
    let redacted = crate::evidence::contains_secret(bytes);
    let retained_bytes: &[u8] = if redacted {
        b"[REDACTED: deterministic secret marker]\n"
    } else {
        bytes
    };
    let hash = content_hash(retained_bytes);
    let path = dir.join(format!("{}-{}", hash.replace(':', "-"), label));
    fs::write(&path, &retained_bytes[..retained_bytes.len().min(max)])?;
    Ok((
        EvidenceRef {
            content_hash: hash,
            media_type: "text/plain".into(),
            byte_size: u64::try_from(retained_bytes.len())
                .map_err(|_| VerificationError::Overflow)?,
            repository: repo.to_string_lossy().into(),
            revision: id.into(),
            storage_ref: path.to_string_lossy().into(),
            truncated: retained_bytes.len() > max || redacted,
            omitted_bytes: u64::try_from(if redacted {
                bytes.len()
            } else {
                bytes.len().saturating_sub(max)
            })
            .map_err(|_| VerificationError::Overflow)?,
        },
        redacted,
    ))
}
#[cfg(unix)]
fn signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}
#[cfg(not(unix))]
fn signal(_: &std::process::ExitStatus) -> Option<i32> {
    None
}
#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("verification argv is empty")]
    EmptyArgv,
    #[error("verification process pipe unavailable")]
    MissingPipe,
    #[error("verification output reader panicked")]
    ReaderPanicked,
    #[error("verification I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("verification arithmetic overflow")]
    Overflow,
}

pub fn relevant_checks<'a>(
    plan: &'a VerificationPlan,
    changed: &[String],
    cited: &[String],
    unsuccessful: &[String],
) -> Vec<&'a VerificationCheck> {
    plan.checks
        .iter()
        .filter(|c| {
            plan.full_after_remediation
                || c.required
                || cited.contains(&c.check_id)
                || unsuccessful.contains(&c.check_id)
                || c.path_prefixes
                    .iter()
                    .any(|p| changed.iter().any(|f| f.starts_with(p)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secret_output_is_redacted_and_inconclusive() {
        let repository = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let runner = CommandVerificationRunner::new(artifacts.path().into(), 1024);
        let check = VerificationCheck {
            check_id: "secret".into(),
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'Authorization: Bearer secret'".into(),
            ],
            working_directory: ".".into(),
            environment: BTreeMap::new(),
            timeout_ms: 1_000,
            required: true,
            path_prefixes: vec![],
        };
        let evidence = runner.run(repository.path(), &check, "diff").unwrap();
        assert_eq!(evidence.status, VerificationStatus::Inconclusive);
        let retained = fs::read_to_string(evidence.stdout.unwrap().storage_ref).unwrap();
        assert!(retained.contains("REDACTED"));
        assert!(!retained.contains("Bearer secret"));
    }
}
