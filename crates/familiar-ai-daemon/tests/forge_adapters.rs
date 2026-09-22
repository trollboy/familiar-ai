//! PRD-097: delivery speaks a forge grammar chosen by declared identity, not
//! spelled inline. `crate::forge` in `familiar-ai-daemon/src/forge.rs`
//! carries per-verb unit coverage for exact argv; this file exercises the
//! adapters end to end through `deliver_with`, the same seam
//! `delivery_identity.rs` and the delivery unit tests already use.
//!
//! Every test here runs through `CommandRunner`. No test performs, or is
//! able to perform, a live forge call.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Mutex;

use familiar_ai_core::config::{DeliveryIdentityConfig, ReviewGateConfig};
use familiar_ai_core::{DeliveryConfig, DeliveryMode, Forge};
use familiar_ai_daemon::delivery::{deliver_with, CommandRunner, DeliveryJournal};
use familiar_ai_daemon::forge::ChangeRequestId;
use familiar_ai_daemon::worktree::WorktreeOwnership;

struct RecordingRunner {
    calls: Mutex<Vec<Vec<String>>>,
    /// Stdout returned for the adapter's `locate` invocation.
    located: &'static str,
    /// The account `account_probe_argv` reports.
    resolves_to: &'static str,
}

impl RecordingRunner {
    fn new(located: &'static str, resolves_to: &'static str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            located,
            resolves_to,
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(
        &self,
        _directory: &Path,
        argv: &[String],
        _env: &[(String, String)],
    ) -> Result<Output, String> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let is_probe = argv.iter().any(|value| value == "whoami");
        // github's `pr view`, gitlab's `mr view`, gitea's `pulls ls` — any of
        // this fixture's three "find the change request" spellings.
        let is_locate = argv.iter().any(|value| value == "view")
            || (argv.iter().any(|value| value == "pulls")
                && argv.iter().any(|value| value == "ls"));
        let is_staged = argv.iter().any(|value| value == "diff");
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: if is_probe {
                format!("{}\n", self.resolves_to).into_bytes()
            } else if is_locate {
                format!("{}\n", self.located).into_bytes()
            } else if is_staged {
                b"src/lib.rs\n".to_vec()
            } else {
                Vec::new()
            },
            stderr: Vec::new(),
        })
    }
}

fn identity(account: &str) -> DeliveryIdentityConfig {
    DeliveryIdentityConfig {
        author_name: account.into(),
        author_email: format!("{account}@example.invalid"),
        forge_account: account.into(),
        provider_env: BTreeMap::new(),
        account_probe_argv: vec!["whoami".into()],
    }
}

fn fixture(
    mode: DeliveryMode,
    forge: Forge,
    provider_argv: Vec<String>,
) -> (tempfile::TempDir, PathBuf, DeliveryConfig) {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    let ownership_path = temp.path().join("attempt.ownership.json");
    fs::write(
        &ownership_path,
        serde_json::to_vec(&WorktreeOwnership {
            session_id: "session".into(),
            prd_id: "PRD-97".into(),
            worktree,
            created_at: "now".into(),
            heartbeat_at: "now".into(),
            state: "ready_for_delivery".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let policy = DeliveryConfig {
        mode,
        forge,
        enabled: true,
        max_deliveries_per_session: 1,
        command_timeout_ms: 1_000,
        remote: "origin".into(),
        base: "main".into(),
        provider_argv,
        staging_environment: "staging".into(),
        deploy_argv: vec!["deploy".into()],
        smoke_argv: vec!["smoke".into()],
        rollback_argv: vec!["rollback".into()],
        identity: Some(identity("steward-bot")),
        review_gate: Some(ReviewGateConfig {
            implementer: "impl".into(),
            reviewer: "review".into(),
            approver: "approve".into(),
        }),
        ..DeliveryConfig::default()
    };
    (temp, ownership_path, policy)
}

/// `forge = "none"` is a first-class adapter: delivery pushes the branch and
/// stops at a terminal phase naming the branch and base, and never reaches
/// for a change-request identifier that was never going to exist.
#[test]
fn none_forge_pushes_the_branch_and_stops_without_ever_calling_a_forge() {
    let (_temp, ownership, policy) =
        fixture(DeliveryMode::ReviewGatedAutomatic, Forge::None, vec![]);
    let runner = RecordingRunner::new("", "steward-bot");

    let result = deliver_with(&ownership, &policy, "repo", &runner).unwrap();

    assert_eq!(result.phase, "awaiting_manual_publication");
    assert!(result.change_request.is_none());
    let detail = result.detail.unwrap();
    assert!(detail.contains(&result.branch), "{detail}");
    assert!(detail.contains("main"), "{detail}");

    let calls = runner.calls();
    assert!(
        calls
            .iter()
            .any(|call| call.first().is_some_and(|v| v == "git")
                && call.iter().any(|value| value == "push")),
        "the branch must still be pushed: {calls:?}"
    );
    for call in &calls {
        assert!(
            !call
                .iter()
                .any(|value| ["pr", "mr", "pulls"].contains(&value.as_str())),
            "forge = none must never issue a forge command: {call:?}"
        );
    }
}

/// `github` must reproduce today's `gh` grammar exactly, byte for byte, so
/// the pre-PRD-097 delivery tests keep pinning behaviour unmodified.
#[test]
fn github_adapter_reproduces_exact_gh_argv_through_full_delivery() {
    let (_temp, ownership, policy) = fixture(
        DeliveryMode::ReviewedPrManual,
        Forge::Github,
        vec!["gh".into()],
    );
    let runner = RecordingRunner::new("42", "steward-bot");

    let result = deliver_with(&ownership, &policy, "repo", &runner).unwrap();

    assert_eq!(result.phase, "awaiting_merge_authority");
    assert_eq!(
        result.change_request,
        Some(ChangeRequestId {
            id: "42".into(),
            display: None,
            url: None
        })
    );
    let calls = runner.calls();
    assert!(calls.contains(&vec![
        "gh".into(),
        "pr".into(),
        "create".into(),
        "--fill".into(),
        "--base".into(),
        "main".into(),
        "--head".into(),
        result.branch.clone(),
    ]));
    assert!(calls.contains(&vec![
        "gh".into(),
        "pr".into(),
        "view".into(),
        result.branch.clone(),
        "--json".into(),
        "number".into(),
        "--jq".into(),
        ".number".into(),
    ]));
}

/// A `gh pr view` invocation that exits zero but writes something other than
/// a bare integer — a warning line, a `--jq` result of `null` — must not be
/// accepted as a change request id. Delivery has to fail closed with the
/// same diagnostic the pre-PRD-097 GitHub path gave, and it must never reach
/// `gh pr merge <text>` with that stray text as the identifier.
#[test]
fn github_adapter_rejects_non_numeric_locate_output_and_fails_delivery() {
    let (_temp, ownership, policy) = fixture(
        DeliveryMode::ReviewGatedAutomatic,
        Forge::Github,
        vec!["gh".into()],
    );
    let runner = RecordingRunner::new("null", "steward-bot");

    let error = deliver_with(&ownership, &policy, "repo", &runner).unwrap_err();

    assert!(
        error.contains("did not return a change request identifier"),
        "{error}"
    );
    let calls = runner.calls();
    assert!(
        !calls
            .iter()
            .any(|call| call.iter().any(|value| value == "merge")),
        "a rejected locate result must never reach merge: {calls:?}"
    );
}

/// `gitlab` mirrors `glab`'s `mr` grammar — a genuinely different noun and
/// flag spelling from `gh` — and reports a bare id with a `!123`-style
/// display spelling alongside it.
#[test]
fn gitlab_adapter_speaks_mr_grammar_and_returns_a_bang_display() {
    let (_temp, ownership, policy) = fixture(
        DeliveryMode::ReviewedPrManual,
        Forge::Gitlab,
        vec!["glab".into()],
    );
    let runner = RecordingRunner::new("123", "steward-bot");

    let result = deliver_with(&ownership, &policy, "repo", &runner).unwrap();

    assert_eq!(result.phase, "awaiting_merge_authority");
    assert_eq!(
        result.change_request,
        Some(ChangeRequestId {
            id: "123".into(),
            display: Some("!123".into()),
            url: None
        })
    );
    let calls = runner.calls();
    assert!(calls.contains(&vec![
        "glab".into(),
        "mr".into(),
        "create".into(),
        "--fill".into(),
        "--target-branch".into(),
        "main".into(),
        "--source-branch".into(),
        result.branch.clone(),
    ]));
    assert!(
        !calls
            .iter()
            .any(|call| call.iter().any(|value| value == "pr")),
        "gitlab must never issue gh's 'pr' noun: {calls:?}"
    );
}

/// The parsed GitLab identity must be fed back into `wait_checks`, `merge`
/// and `comment` in the bare spelling `glab` accepts as an argument — not
/// the `!123` a human reads on the merge request page. This runs the
/// `ReviewGatedAutomatic` mode past `awaiting_merge_authority` so it
/// actually reaches those verbs, unlike the `ReviewedPrManual` fixture
/// above which stops before any of them.
#[test]
fn gitlab_consuming_verbs_use_the_bare_parsed_id_not_the_bang_display() {
    let (_temp, ownership, policy) = fixture(
        DeliveryMode::ReviewGatedAutomatic,
        Forge::Gitlab,
        vec!["glab".into()],
    );
    let runner = RecordingRunner::new("123", "steward-bot");

    let result = deliver_with(&ownership, &policy, "repo", &runner).unwrap();

    assert_eq!(result.phase, "staging_verified");
    assert_eq!(
        result.change_request,
        Some(ChangeRequestId {
            id: "123".into(),
            display: Some("!123".into()),
            url: None
        })
    );
    let calls = runner.calls();
    assert!(
        calls.contains(&vec![
            "glab".into(),
            "mr".into(),
            "checks".into(),
            "123".into()
        ]),
        "wait_checks must use the bare id: {calls:?}"
    );
    assert!(
        calls.contains(&vec![
            "glab".into(),
            "mr".into(),
            "merge".into(),
            "123".into(),
            "--remove-source-branch".into(),
        ]),
        "merge must use the bare id: {calls:?}"
    );
    for call in &calls {
        assert!(
            !call.iter().any(|value| value.contains('!')),
            "no argv should carry the `!` display spelling: {call:?}"
        );
    }
}

/// `gitea`'s `tea` CLI has no CI-awareness, so it declines `wait_checks`.
/// That decline is a typed, journaled outcome — delivery stops at the same
/// manual-authority phase a `reviewed_pr_manual` policy would, not an error,
/// and never silently proceeds to merge unchecked code.
#[test]
fn gitea_adapter_declines_checks_and_stops_short_of_merge_without_erroring() {
    let (_temp, ownership, policy) = fixture(
        DeliveryMode::ReviewGatedAutomatic,
        Forge::Gitea,
        vec!["tea".into()],
    );
    let runner = RecordingRunner::new("7", "steward-bot");

    let result = deliver_with(&ownership, &policy, "repo", &runner).unwrap();

    assert_eq!(result.phase, "awaiting_merge_authority");
    let detail = result.detail.unwrap();
    assert!(detail.contains("gitea"), "{detail}");
    assert!(detail.contains("watch checks"), "{detail}");
    assert_eq!(
        result.change_request,
        Some(ChangeRequestId {
            id: "7".into(),
            display: None,
            url: None
        })
    );
    let calls = runner.calls();
    assert!(
        !calls
            .iter()
            .any(|call| call.iter().any(|value| value == "merge")),
        "a declined checks verb must not be silently skipped past into a merge: {calls:?}"
    );
}

/// The opaque change-request identity is never `Option<u64>`: a Gerrit-style
/// change id and a GitLab-style `!123` both round-trip through the delivery
/// journal's own JSON encoding without lossy parsing.
#[test]
fn a_gerrit_style_and_a_gitlab_style_change_id_round_trip_through_the_delivery_journal() {
    for change_request in [
        ChangeRequestId {
            id: "I8f7d45ab19204dc9c6e6b4b1e7c2a1a4b8d9f012".into(),
            display: None,
            url: Some("https://gerrit.example/c/repo/+/4200".into()),
        },
        ChangeRequestId {
            id: "123".into(),
            display: Some("!123".into()),
            url: None,
        },
    ] {
        let journal = DeliveryJournal {
            session_id: "session".into(),
            prd_id: "PRD-97".into(),
            worktree: PathBuf::from("/tmp/worktree"),
            branch: "familiar/session/PRD-97".into(),
            change_request: Some(change_request.clone()),
            phase: "published".into(),
            detail: None,
            updated_at: "2026-09-22T00:00:00Z".into(),
        };
        let round_tripped: DeliveryJournal =
            serde_json::from_str(&serde_json::to_string(&journal).unwrap()).unwrap();
        assert_eq!(round_tripped.change_request, Some(change_request));
    }
}
