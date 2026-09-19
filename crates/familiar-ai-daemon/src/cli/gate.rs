//! `familiar-ai gate` — PRD-099.
//!
//! An operator capability that only exists in a web interface is not a
//! capability this project has (PRD-084). The gate's verdict for a commit is
//! therefore a shipped command, answerable by a person and by the daemon that
//! will eventually want to know whether the branch is green before it drives.
//!
//! The four answers are deliberate. `absent` is not `red`: a commit nothing
//! ever verified is a different fact from a commit that failed, and conflating
//! them is how a project convinces itself that untested code is merely
//! unlucky. `unreadable` is not `green`: when the verdict cannot be read, the
//! answer is that it cannot be read. Only `green` is a pass.

use std::process::Command as ProcessCommand;
use std::time::Duration;

use clap::Subcommand;
use serde_json::Value;

use familiar_ai_core::AppPaths;
use familiar_ai_storage::{Database, GateOverrideRepository};

use super::shared::effective_repository_config;

/// The job name in `.github/workflows/gate.yml`. A check run with this name is
/// the gate; anything else on the commit is some other workflow's business and
/// must not be able to turn the gate green or red on its behalf.
const GATE_CHECK_NAME: &str = "gate";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    Green,
    Red,
    Absent,
    Unreadable,
}

impl GateVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Red => "red",
            Self::Absent => "absent",
            Self::Unreadable => "unreadable",
        }
    }

    /// Only green is a pass. An incomplete gate is a failed gate.
    pub fn is_pass(self) -> bool {
        matches!(self, Self::Green)
    }
}

#[derive(Debug, Subcommand)]
pub enum GateCommand {
    /// Answer green, red, absent or unreadable for a commit. Exits non-zero
    /// for anything but green.
    Status {
        /// Defaults to the current HEAD.
        #[arg(long)]
        commit: Option<String>,
    },
    /// Refuse unless the commit is green or carries a recorded override.
    /// This is what a merge path asks before proceeding.
    Require {
        #[arg(long)]
        commit: Option<String>,
    },
    /// Record an override of a non-green verdict. Both an actor and a reason
    /// are mandatory, and the schema rejects an empty one, so an override that
    /// names nobody cannot be written.
    Override {
        #[arg(long)]
        commit: Option<String>,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
}

/// Map GitHub's check-runs payload for one commit onto a verdict.
///
/// Pure, so the four answers are pinned by regressions that never touch a
/// network. Everything that is not a completed success is a failure: queued,
/// in progress, cancelled, timed out, skipped, or completed with no conclusion
/// at all. That is the "incomplete is failed" rule, and it is applied here
/// rather than at the call site so there is one place to read it.
pub fn verdict_from_check_runs(body: &str) -> GateVerdict {
    let Ok(payload) = serde_json::from_str::<Value>(body) else {
        return GateVerdict::Unreadable;
    };
    let Some(runs) = payload.get("check_runs").and_then(Value::as_array) else {
        return GateVerdict::Unreadable;
    };
    let gate_runs: Vec<&Value> = runs
        .iter()
        .filter(|run| run.get("name").and_then(Value::as_str) == Some(GATE_CHECK_NAME))
        .collect();
    if gate_runs.is_empty() {
        return GateVerdict::Absent;
    }
    for run in gate_runs {
        let status = run.get("status").and_then(Value::as_str);
        let conclusion = run.get("conclusion").and_then(Value::as_str);
        if status != Some("completed") || conclusion != Some("success") {
            return GateVerdict::Red;
        }
    }
    GateVerdict::Green
}

/// Whether a merge may proceed, given the verdict and whatever override is on
/// record. Pure, so the refusal is pinned by a regression rather than by
/// reading the command and hoping.
///
/// Green proceeds. Anything else proceeds only on a recorded override, and an
/// override that does not exist is not a silent pass — it is the refusal.
pub fn merge_decision(
    commit: &str,
    verdict: GateVerdict,
    recorded: Option<&familiar_ai_storage::GateOverride>,
) -> Result<String, String> {
    if verdict.is_pass() {
        return Ok(format!("commit={commit} gate=green"));
    }
    match recorded {
        Some(record) => Ok(format!(
            "commit={commit} gate={} overridden by {} at {}: {}",
            verdict.as_str(),
            record.actor,
            record.created_at,
            record.reason
        )),
        None => Err(format!(
            "refusing {commit}: gate is {} and no override is recorded — record one with `familiar-ai gate override --actor <who> --reason <why>`",
            verdict.as_str()
        )),
    }
}

/// `owner/repo` from the origin remote. Derived from the repository itself
/// rather than from configuration, so no setting outside this tree can point
/// the verdict at a different repository.
fn repository_slug() -> Result<String, String> {
    let output = ProcessCommand::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err("no origin remote to resolve the forge repository from".into());
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let tail = url
        .rsplit_once(':')
        .map(|(_, tail)| tail.to_string())
        .filter(|tail| !tail.starts_with("//"))
        .unwrap_or_else(|| {
            url.trim_end_matches('/')
                .rsplitn(3, '/')
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("/")
        });
    let slug = tail.trim_end_matches(".git").trim_matches('/').to_string();
    if slug.split('/').count() != 2 || slug.split('/').any(str::is_empty) {
        return Err(format!("could not read owner/repo from origin url {url}"));
    }
    Ok(slug)
}

fn resolve_commit(commit: Option<String>) -> Result<String, String> {
    if let Some(sha) = commit {
        return Ok(sha);
    }
    let output = ProcessCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err("could not resolve HEAD".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Ask the forge. Any failure to reach or parse it is `Unreadable` — never a
/// pass, and never silently a failure of the code under test either.
fn fetch_verdict(slug: &str, commit: &str) -> GateVerdict {
    let url = format!("https://api.github.com/repos/{slug}/commits/{commit}/check-runs");
    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("familiar-ai-gate")
        .build()
    else {
        return GateVerdict::Unreadable;
    };
    let mut request = client
        .get(url)
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
        .filter(|token| !token.trim().is_empty())
    {
        request = request.bearer_auth(token);
    }
    let Ok(response) = request.send() else {
        return GateVerdict::Unreadable;
    };
    if !response.status().is_success() {
        return GateVerdict::Unreadable;
    }
    let Ok(body) = response.text() else {
        return GateVerdict::Unreadable;
    };
    verdict_from_check_runs(&body)
}

pub fn gate(command: GateCommand) -> Result<(), String> {
    match command {
        GateCommand::Status { commit } => {
            let commit = resolve_commit(commit)?;
            let slug = repository_slug()?;
            let verdict = fetch_verdict(&slug, &commit);
            println!("commit={commit} gate={}", verdict.as_str());
            if verdict.is_pass() {
                Ok(())
            } else {
                Err(format!(
                    "gate is {} for {commit} — only green is a pass",
                    verdict.as_str()
                ))
            }
        }
        GateCommand::Require { commit } => {
            let commit = resolve_commit(commit)?;
            let slug = repository_slug()?;
            let verdict = fetch_verdict(&slug, &commit);
            if verdict.is_pass() {
                println!("commit={commit} gate=green");
                return Ok(());
            }
            let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
            let current = std::env::current_dir().map_err(|e| e.to_string())?;
            let config = effective_repository_config(&paths, &current)?;
            let db = Database::open(&config.database.resolve_path(&paths.data_dir))
                .map_err(|e| e.to_string())?;
            db.run_migrations().map_err(|e| e.to_string())?;
            let recorded = GateOverrideRepository::new(&db)
                .for_commit(&commit)
                .map_err(|e| e.to_string())?;
            let line = merge_decision(&commit, verdict, recorded.as_ref())?;
            println!("{line}");
            Ok(())
        }
        GateCommand::Override {
            commit,
            actor,
            reason,
        } => {
            let commit = resolve_commit(commit)?;
            let slug = repository_slug()?;
            let verdict = fetch_verdict(&slug, &commit);
            if verdict.is_pass() {
                return Err(format!(
                    "refusing to record an override for {commit}: the gate is green, so there is nothing to override"
                ));
            }
            let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
            let current = std::env::current_dir().map_err(|e| e.to_string())?;
            let config = effective_repository_config(&paths, &current)?;
            let db = Database::open(&config.database.resolve_path(&paths.data_dir))
                .map_err(|e| e.to_string())?;
            db.run_migrations().map_err(|e| e.to_string())?;
            let record = GateOverrideRepository::new(&db)
                .record(&commit, verdict.as_str(), &actor, &reason)
                .map_err(|e| e.to_string())?;
            println!(
                "recorded override {} for {commit} ({}) by {}: {}",
                record.override_id, record.verdict, record.actor, record.reason
            );
            Ok(())
        }
    }
}
