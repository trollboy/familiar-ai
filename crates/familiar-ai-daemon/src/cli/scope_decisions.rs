//! `familiar-ai scope-decisions` -- list or decide one hash-bound pending
//! scope finding.
//!
//! PRD-083: the bare, no-argument invocation is the discoverable half of the
//! contract. It never asks an operator to transcribe a sha256 -- decisions
//! are made by ordinal or PRD identifier, over a numbered listing that names
//! the PRD, the change at issue, and the finding that stopped it. The
//! decision itself stays hash-bound in storage; only the human interface
//! drops the hash. [`list_or_decide_interactively`] is the shared surface:
//! this command's no-argument form and the drive's own pause (`drive.rs`)
//! both call it, so an interactive decision and a flag-form decision take
//! identical durable rows through the identical continuation path.

use std::io::{self, BufRead, IsTerminal, Write};

use familiar_ai_core::{AppPaths, BacklogDiscovery, Config, FilesystemBacklogDiscovery};
use familiar_ai_storage::{Database, OrchestrationRepository, ScopeDecision};

use super::shared::effective_repository_config;

pub fn scope_decisions(
    finding_hash: Option<String>,
    candidate_hash: Option<String>,
    approve: bool,
    reject: bool,
    actor: Option<String>,
    reason: Option<String>,
) -> Result<(), String> {
    let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
    let current = std::env::current_dir().map_err(|e| e.to_string())?;
    let config = effective_repository_config(&paths, &current)?;
    let repository = FilesystemBacklogDiscovery
        .resolve(&current)
        .map_err(|e| e.to_string())?;
    let mut db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|e| e.to_string())?;
    db.run_migrations().map_err(|e| e.to_string())?;
    if finding_hash.is_none() {
        let attended = io::stdin().is_terminal() && io::stderr().is_terminal();
        let mut stdin = io::stdin().lock();
        return list_or_decide_interactively(&mut db, &repository, &config, attended, &mut stdin);
    }
    let repo = OrchestrationRepository::new(db.conn());
    if approve == reject {
        return Err("supply exactly one of --approve or --reject".into());
    }
    let actor = actor.ok_or_else(|| "--actor is required for a decision".to_string())?;
    if !actor.starts_with("human:") {
        return Err("--actor must be human:<identity>".into());
    }
    let checkpoint = repo
        .decide_scope(
            &repository.key,
            &finding_hash.unwrap(),
            &candidate_hash.ok_or_else(|| "--candidate-hash is required".to_string())?,
            approve,
            &actor,
            &reason
                .filter(|r| !r.trim().is_empty())
                .ok_or_else(|| "--reason is required".to_string())?,
        )
        .map_err(|e| e.to_string())?;
    continue_scope_decision(&mut db, &repository, &config, &checkpoint)?;
    Ok(())
}

/// A human-readable line naming the PRD, the change at issue, and the
/// finding that stopped it -- never the hash. Used by the numbered listing
/// so an operator can decide without reading source or transcribing a
/// sha256 (PRD-083).
fn describe_pending_decision(index: usize, item: &ScopeDecision) -> String {
    match serde_json::from_str::<familiar_ai_review::ScopeFinding>(&item.finding_json) {
        Ok(finding) => format!(
            "{}. {}  change: {:?} {}  finding: {:?} rule={} ({})",
            index + 1,
            item.prd_id,
            finding.change_kind,
            finding.path,
            finding.decision,
            finding.rule_id,
            finding.rule_detail
        ),
        Err(_) => format!(
            "{}. {}  (finding detail unavailable for checkpoint {})",
            index + 1,
            item.prd_id,
            item.checkpoint_id
        ),
    }
}

/// The shared interactive/listing surface (PRD-083). Always lists every
/// pending decision numbered, naming its PRD, its finding summary, and the
/// change at issue. When `attended`, also takes one decision by ordinal or
/// PRD identifier -- never a hash -- and durably records it through the
/// same [`decide_scope`](OrchestrationRepository::decide_scope) /
/// continuation path the flag form uses. `input` supplies the operator's
/// answers, so this is directly testable with canned input instead of a
/// real terminal; only `attended` (computed once, from the same terminal
/// check the CLI entrypoint and the drive's pause both use) decides whether
/// the prompts run at all.
pub fn list_or_decide_interactively(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    config: &Config,
    attended: bool,
    input: &mut impl BufRead,
) -> Result<(), String> {
    let repo = OrchestrationRepository::new(db.conn());
    let pending = repo
        .pending_scope_decisions(&repository.key)
        .map_err(|e| e.to_string())?;
    if pending.is_empty() {
        return Ok(());
    }
    for (index, item) in pending.iter().enumerate() {
        println!("{}", describe_pending_decision(index, item));
    }
    if !attended {
        return Ok(());
    }
    eprint!("Decide # or PRD id (blank to preserve): ");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let mut selection = String::new();
    input.read_line(&mut selection).map_err(|e| e.to_string())?;
    let selection = selection.trim();
    if selection.is_empty() {
        return Ok(());
    }
    let chosen = selection
        .parse::<usize>()
        .ok()
        .filter(|ordinal| *ordinal >= 1 && *ordinal <= pending.len())
        .map(|ordinal| &pending[ordinal - 1])
        .or_else(|| {
            pending
                .iter()
                .find(|item| item.prd_id.eq_ignore_ascii_case(selection))
        })
        .ok_or_else(|| format!("no pending decision matches '{selection}'"))?;
    eprint!("Approve or reject [a/r]: ");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let mut choice = String::new();
    input.read_line(&mut choice).map_err(|e| e.to_string())?;
    eprint!("Actor (human:<identity>): ");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let mut who = String::new();
    input.read_line(&mut who).map_err(|e| e.to_string())?;
    let actor = who.trim();
    if !actor.starts_with("human:") {
        return Err("actor must be human:<identity>".into());
    }
    eprint!("Reason: ");
    io::stderr().flush().map_err(|e| e.to_string())?;
    let mut reason = String::new();
    input.read_line(&mut reason).map_err(|e| e.to_string())?;
    let reason = reason.trim();
    if reason.is_empty() {
        return Err("a reason is required".into());
    }
    let checkpoint = repo
        .decide_scope(
            &repository.key,
            &chosen.finding_hash,
            &chosen.candidate_hash,
            choice.trim().eq_ignore_ascii_case("a"),
            actor,
            reason,
        )
        .map_err(|e| e.to_string())?;
    continue_scope_decision(db, repository, config, &checkpoint)
}

fn continue_scope_decision(
    db: &mut Database,
    repository: &familiar_ai_core::RepositoryIdentity,
    config: &Config,
    checkpoint_id: &str,
) -> Result<(), String> {
    let checkpoint = familiar_ai_storage::CheckpointRepository::new(db.conn())
        .all(&repository.key)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
        .ok_or_else(|| format!("checkpoint {checkpoint_id} disappeared"))?;
    if checkpoint.phase != "reviewed" {
        return Ok(());
    }
    let repository_config = config
        .repository(&repository.worktree)
        .map_err(|error| error.to_string())?;
    let target = FilesystemBacklogDiscovery
        .discover_with_layout(repository, &repository_config.layout())
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|target| target.id.to_string() == checkpoint.prd_id)
        .ok_or_else(|| format!("{} is no longer discoverable", checkpoint.prd_id))?;
    crate::drive::continue_scope_approved_candidate(db, repository, &target, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_storage::{CheckpointRepository, ExecutionCheckpoint};
    use std::io::Cursor;

    fn database() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        db
    }

    fn identity() -> familiar_ai_core::RepositoryIdentity {
        familiar_ai_core::RepositoryIdentity {
            worktree: "/tmp/work".into(),
            key: "repo".into(),
        }
    }

    fn seed_checkpoint(db: &Database, checkpoint_id: &str, prd_id: &str) {
        CheckpointRepository::new(db.conn())
            .put(&ExecutionCheckpoint {
                checkpoint_id: checkpoint_id.into(),
                repository_key: "repo".into(),
                prd_id: prd_id.into(),
                prd_path: format!("docs/prds/{prd_id}.md"),
                execution_id: Some("exec-1".into()),
                phase: "implemented_pending_review".into(),
                base_revision: "deadbeef".into(),
                worktree_path: "/tmp/does-not-matter".into(),
                branch_name: None,
                diff_hash: "sha256:candidate".into(),
                changed_files_json: "[]".into(),
                agent_identity: "claude-code".into(),
                usage_json: "{}".into(),
                test_evidence_json: "{}".into(),
                invalid_reason: None,
            })
            .unwrap();
    }

    /// PRD-083 acceptance criterion 2: the bare listing never shows a hash,
    /// and names the PRD and the change at issue.
    #[test]
    fn listing_names_prd_and_change_never_a_hash() {
        let finding = familiar_ai_review::ScopeFinding {
            finding_id: "f1".into(),
            change_id: "c1".into(),
            path: "src/unexpected.rs".into(),
            old_path: None,
            change_kind: familiar_ai_review::GitChangeKind::Added,
            file_class: familiar_ai_review::ScopeFileClass::OrdinarySource,
            decision: familiar_ai_review::ScopeDecision::UndeclaredScopeExpansion,
            rule_id: "no-expected-file-match".into(),
            rule_source: familiar_ai_review::ScopeRuleSource::BuiltIn,
            rule_detail: "src/unexpected.rs is not in the PRD's expected_files".into(),
            expected_file_match: None,
            allowed_path_match: None,
            prohibited_rule_match: None,
            policy_snapshot_hash: "policy-1".into(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let description = describe_pending_decision(
            0,
            &ScopeDecision {
                finding_hash: "abc123hash".into(),
                checkpoint_id: "cp-1".into(),
                prd_id: "PRD-83".into(),
                candidate_hash: "cand456hash".into(),
                finding_json: json,
            },
        );
        assert!(description.starts_with("1. PRD-83"));
        assert!(description.contains("src/unexpected.rs"));
        assert!(description.contains("UndeclaredScopeExpansion"));
        assert!(!description.contains("abc123hash"));
        assert!(!description.contains("cand456hash"));
    }

    /// PRD-083 acceptance criterion 3/4: an attended decision picks by
    /// ordinal, never transcribes a hash, and durably records the same
    /// hash-bound row the flag form would.
    #[test]
    fn attended_interactive_decision_resolves_by_ordinal() {
        let db_owned = database();
        let identity = identity();
        seed_checkpoint(&db_owned, "cp-1", "PRD-83");
        OrchestrationRepository::new(db_owned.conn())
            .record_scope_finding(
                &identity.key,
                "cp-1",
                "PRD-83",
                "sha256:candidate",
                "finding-hash-1",
                r#"{"finding_id":"f","change_id":"c","path":"p","old_path":null,"change_kind":"added","file_class":"ordinary_source","decision":"undeclared_scope_expansion","rule_id":"r","rule_source":"built_in","rule_detail":"d","expected_file_match":null,"allowed_path_match":null,"prohibited_rule_match":null,"policy_snapshot_hash":"h"}"#,
            )
            .unwrap();
        let mut db = db_owned;
        let config = Config::default();
        let mut input = Cursor::new(b"1\na\nhuman:tester\nlooks fine\n".to_vec());
        list_or_decide_interactively(&mut db, &identity, &config, true, &mut input).unwrap();

        let (decision, actor, reason, candidate_hash): (
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = db
            .conn()
            .query_row(
                "SELECT decision,actor,reason,candidate_hash FROM scope_decisions WHERE finding_hash='finding-hash-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(decision.as_deref(), Some("approved"));
        assert_eq!(actor.as_deref(), Some("human:tester"));
        assert_eq!(reason.as_deref(), Some("looks fine"));
        assert_eq!(candidate_hash, "sha256:candidate");

        let phase: String = db
            .conn()
            .query_row(
                "SELECT phase FROM execution_checkpoints WHERE checkpoint_id='cp-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, "implemented");
    }

    /// An unattended invocation lists but never prompts or decides -- the
    /// same pending row survives untouched.
    #[test]
    fn unattended_listing_never_decides() {
        let db_owned = database();
        let identity = identity();
        seed_checkpoint(&db_owned, "cp-1", "PRD-83");
        OrchestrationRepository::new(db_owned.conn())
            .record_scope_finding(
                &identity.key,
                "cp-1",
                "PRD-83",
                "sha256:candidate",
                "finding-hash-1",
                r#"{"finding_id":"f","change_id":"c","path":"p","old_path":null,"change_kind":"added","file_class":"ordinary_source","decision":"undeclared_scope_expansion","rule_id":"r","rule_source":"built_in","rule_detail":"d","expected_file_match":null,"allowed_path_match":null,"prohibited_rule_match":null,"policy_snapshot_hash":"h"}"#,
            )
            .unwrap();
        let mut db = db_owned;
        let config = Config::default();
        let mut input = Cursor::new(Vec::new());
        list_or_decide_interactively(&mut db, &identity, &config, false, &mut input).unwrap();

        let decision: Option<String> = db
            .conn()
            .query_row(
                "SELECT decision FROM scope_decisions WHERE finding_hash='finding-hash-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(decision, None);
    }
}
