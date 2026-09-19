//! `familiar-ai gate` — PRD-099, amended 2026-09-19.
//!
//! Verification is local. This is a desktop application that runs on the
//! machine doing the work, not a service deployed from a pipeline, so the gate
//! runs here and its verdict is recorded in Familiar's own ledger. That answer
//! is available offline, costs no minutes of anyone's runner, and does not
//! couple verification to a forge whose independence PRD-101 is about.
//!
//! The four answers are deliberate. `absent` is not `red`: a commit nothing
//! ever verified is a different fact from a commit that failed, and conflating
//! them is how a project convinces itself that untested code is merely
//! unlucky. `unreadable` is not `green`: when the record cannot be read, the
//! answer is that it cannot be read. Only `green` is a pass.

use std::path::Path;
use std::process::Command as ProcessCommand;

use clap::Subcommand;

use familiar_ai_core::AppPaths;
use familiar_ai_storage::{
    Database, GateOverrideRepository, GateVerdictRecord, GateVerdictRepository,
};

use super::shared::effective_repository_config;

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

/// Read a recorded verdict. Anything stored that is not one of the two
/// outcomes the gate can produce is `unreadable` rather than assumed — a
/// verdict that cannot be understood is never a pass.
pub fn verdict_of(record: Option<&GateVerdictRecord>) -> GateVerdict {
    match record {
        None => GateVerdict::Absent,
        Some(record) => match record.verdict.as_str() {
            "green" => GateVerdict::Green,
            "red" => GateVerdict::Red,
            _ => GateVerdict::Unreadable,
        },
    }
}

/// Whether a merge may proceed, given the verdict and whatever override is on
/// record. Pure, so the refusal is pinned by a regression rather than by
/// reading the command and hoping.
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

#[derive(Debug, Subcommand)]
pub enum GateCommand {
    /// Run the single definition and record its verdict against HEAD.
    Run {
        /// Invoked from the pre-push hook. Honours `[gate] pre_push_hook`,
        /// so the trigger can be turned off without the definition changing
        /// and without uninstalling the hook. A plain `gate run` always runs.
        #[arg(long)]
        hook: bool,
    },
    /// Answer green, red, absent or unreadable for a commit. Exits non-zero
    /// for anything but green.
    Status {
        /// Defaults to the current HEAD.
        #[arg(long)]
        commit: Option<String>,
    },
    /// Refuse unless the commit is green or carries a recorded override.
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

/// Whether tracked files differ from HEAD. Untracked files are ignored: they
/// are not part of the commit and cannot change what the gate compiled.
fn working_tree_is_dirty() -> Result<bool, String> {
    let output = ProcessCommand::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err("could not read the working tree state".into());
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn open_db() -> Result<Database, String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    Ok(db)
}

pub fn gate(command: GateCommand) -> Result<(), String> {
    match command {
        GateCommand::Run { hook } => {
            if hook {
                let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
                let current = std::env::current_dir().map_err(|e| e.to_string())?;
                let config = effective_repository_config(&paths, &current)?;
                if !config.gate.pre_push_hook {
                    // A trigger is off, not a step. Nothing about what the gate
                    // runs has changed, and the commit simply carries no
                    // verdict — `gate status` answers `absent`, which is the
                    // honest answer and is never a pass.
                    println!(
                        "gate: pre-push hook disabled by config ([gate] pre_push_hook = false); \
                         this commit will read `absent` until something verifies it"
                    );
                    return Ok(());
                }
            }
            let commit = resolve_commit(None)?;
            let repo = std::env::current_dir().map_err(|e| e.to_string())?;
            let script = Path::new("scripts/gate.sh");
            if !repo.join(script).exists() {
                return Err("scripts/gate.sh not found — run from the repository root".into());
            }
            // The gate inherits this process's stdout and stderr rather than
            // having its output captured and relayed. A full suite run is
            // megabytes, and a git hook's stdout can be non-blocking, so
            // buffering it here and writing it back in one go fails with
            // EAGAIN and takes the push down with it. Letting bash write
            // straight to the terminal streams at whatever pace it can take.
            //
            // The outcome lines come back through a summary file instead.
            let summary = std::env::temp_dir().join(format!("familiar-ai-gate-{commit}.summary"));
            let _ = std::fs::remove_file(&summary);
            let status = ProcessCommand::new("bash")
                .arg(script)
                .current_dir(&repo)
                .env("GATE_SUMMARY", &summary)
                .status()
                .map_err(|error| format!("could not run the gate: {error}"))?;

            // An interrupted run has no exit code at all; `success()` is false
            // for it, which is the "incomplete is failed" rule applied at the
            // point the verdict is written.
            let passed = status.success();
            let verdict = if passed { "green" } else { "red" };
            let detail = std::fs::read_to_string(&summary)
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            let _ = std::fs::remove_file(&summary);
            let detail = if detail.is_empty() {
                format!("gate exited with {status}")
            } else {
                detail
            };
            // The gate verified the working tree, not necessarily the commit.
            // Recording this verdict against HEAD while tracked files differ
            // from it would attach a pass to a tree nobody tested — the exact
            // false green this PRD exists to prevent. Say so instead.
            if working_tree_is_dirty()? {
                println!(
                    "not recording: the working tree differs from {commit}, so this verdict \
                     does not describe that commit. `gate status` will read `absent` for it."
                );
                return if passed {
                    Ok(())
                } else {
                    Err(format!("gate is red for the working tree at {commit}"))
                };
            }

            let db = open_db()?;
            let record = GateVerdictRepository::new(&db)
                .record(&commit, verdict, &detail)
                .map_err(|e| e.to_string())?;
            println!(
                "recorded commit={} gate={} at {}",
                record.commit_sha, record.verdict, record.recorded_at
            );
            if passed {
                Ok(())
            } else {
                Err(format!("gate is red for {commit}"))
            }
        }
        GateCommand::Status { commit } => {
            let commit = resolve_commit(commit)?;
            let db = open_db()?;
            let record = GateVerdictRepository::new(&db)
                .for_commit(&commit)
                .map_err(|e| e.to_string())?;
            let verdict = verdict_of(record.as_ref());
            match &record {
                Some(record) => println!(
                    "commit={commit} gate={} recorded {} ({})",
                    verdict.as_str(),
                    record.recorded_at,
                    record.detail
                ),
                None => println!("commit={commit} gate=absent — nothing has verified this commit"),
            }
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
            let db = open_db()?;
            let verdict = verdict_of(
                GateVerdictRepository::new(&db)
                    .for_commit(&commit)
                    .map_err(|e| e.to_string())?
                    .as_ref(),
            );
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
            let db = open_db()?;
            let verdict = verdict_of(
                GateVerdictRepository::new(&db)
                    .for_commit(&commit)
                    .map_err(|e| e.to_string())?
                    .as_ref(),
            );
            if verdict.is_pass() {
                return Err(format!(
                    "refusing to record an override for {commit}: the gate is green, so there is nothing to override"
                ));
            }
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
