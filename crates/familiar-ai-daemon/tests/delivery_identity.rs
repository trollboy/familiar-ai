//! PRD-095: the identity Familiar authors and publishes under is declared per
//! repository, selected per invocation, and verified before publication.
//!
//! Before this, `DeliveryConfig::provider_argv` embedded no account (its own
//! doc comment says so) and `ProcessRunner` inherited the daemon's whole
//! environment while setting no `GIT_AUTHOR_*`, so both the commit's author
//! and the publishing account were whatever ambient state supplied. On a host
//! with one account that is accidentally right; on a host with several it is
//! silently wrong until a write fails with a message naming neither identity
//! nor permission.
//!
//! Everything here runs through the `CommandRunner` seam. No test performs, or
//! is able to perform, a live forge call.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Mutex;

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;

use familiar_ai_core::config::DeliveryIdentityConfig;
use familiar_ai_core::{DeliveryConfig, DeliveryMode};
use familiar_ai_daemon::delivery::{deliver_with, CommandRunner};
use familiar_ai_daemon::worktree::WorktreeOwnership;

/// One recorded invocation: its argv, and the environment applied to it
/// alone.
type Invocation = (Vec<String>, Vec<(String, String)>);

/// Records argv *and* the per-invocation environment, because "identity was
/// passed to this call and nothing else changed" is exactly the property
/// under test.
struct RecordingRunner {
    calls: Mutex<Vec<Invocation>>,
    /// What the account probe reports — the adapter's actually-resolved
    /// account, which need not be the declared one.
    resolves_to: String,
}

impl RecordingRunner {
    fn resolving_to(account: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            resolves_to: account.to_owned(),
        }
    }

    fn argv(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(argv, _)| argv.clone())
            .collect()
    }

    fn calls(&self) -> Vec<Invocation> {
        self.calls.lock().unwrap().clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(
        &self,
        _directory: &Path,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<Output, String> {
        self.calls
            .lock()
            .unwrap()
            .push((argv.to_vec(), env.to_vec()));
        let is_probe = argv.get(1).is_some_and(|value| value == "api");
        let is_view = argv.get(2).is_some_and(|value| value == "view");
        let is_staged = argv.get(1).is_some_and(|value| value == "diff");
        Ok(Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: if is_probe {
                format!("{}\n", self.resolves_to).into_bytes()
            } else if is_view {
                b"7\n".to_vec()
            } else if is_staged {
                b"src/lib.rs\n".to_vec()
            } else {
                Vec::new()
            },
            stderr: Vec::new(),
        })
    }
}

fn identity(account: &str, email: &str) -> DeliveryIdentityConfig {
    let mut provider_env = BTreeMap::new();
    // A descriptor-only selection: which config the adapter reads, not a
    // credential. Per invocation, so no account state outlives the call.
    provider_env.insert("GH_CONFIG_DIR".to_owned(), format!("/cfg/{account}"));
    DeliveryIdentityConfig {
        author_name: account.to_owned(),
        author_email: email.to_owned(),
        forge_account: account.to_owned(),
        provider_env,
        account_probe_argv: vec![
            "gh".into(),
            "api".into(),
            "user".into(),
            "--jq".into(),
            ".login".into(),
        ],
    }
}

fn fixture(
    identity: Option<DeliveryIdentityConfig>,
) -> (tempfile::TempDir, PathBuf, DeliveryConfig) {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().join("worktree");
    std::fs::create_dir(&worktree).unwrap();
    let ownership_path = temp.path().join("attempt.ownership.json");
    std::fs::write(
        &ownership_path,
        serde_json::to_vec(&WorktreeOwnership {
            session_id: "session".into(),
            prd_id: "PRD-1".into(),
            worktree,
            created_at: "now".into(),
            heartbeat_at: "now".into(),
            state: "ready_for_delivery".into(),
        })
        .unwrap(),
    )
    .unwrap();
    let policy = DeliveryConfig {
        mode: DeliveryMode::ReviewedPrManual,
        enabled: true,
        max_deliveries_per_session: 1,
        command_timeout_ms: 1_000,
        remote: "origin".into(),
        base: "main".into(),
        provider_argv: vec!["gh".into()],
        identity,
        ..DeliveryConfig::default()
    };
    (temp, ownership_path, policy)
}

/// The multi-account case the whole PRD exists for: one session, two
/// repositories, two declared identities, and no leakage between them.
#[test]
fn two_repositories_in_one_session_author_and_publish_under_their_own_identities() {
    let (_a_temp, a_ownership, a_policy) =
        fixture(Some(identity("alpha", "alpha@example.invalid")));
    let (_b_temp, b_ownership, b_policy) = fixture(Some(identity("beta", "beta@example.invalid")));

    let alpha = RecordingRunner::resolving_to("alpha");
    let beta = RecordingRunner::resolving_to("beta");
    deliver_with(&a_ownership, &a_policy, "repo/alpha", &alpha).unwrap();
    deliver_with(&b_ownership, &b_policy, "repo/beta", &beta).unwrap();

    for (runner, account, email, other) in [
        (&alpha, "alpha", "alpha@example.invalid", "beta"),
        (&beta, "beta", "beta@example.invalid", "alpha"),
    ] {
        let commit = runner
            .argv()
            .into_iter()
            .find(|argv| argv.contains(&"commit".to_owned()))
            .unwrap_or_else(|| panic!("{account} delivery never committed"));
        assert!(
            commit.contains(&format!("user.name={account}")),
            "commit was not authored as {account}: {commit:?}"
        );
        assert!(
            commit.contains(&format!("user.email={email}")),
            "commit carried the wrong author email: {commit:?}"
        );

        // The publishing calls carry this repository's selection and only it.
        let published: Vec<_> = runner
            .calls()
            .into_iter()
            .filter(|(argv, _)| argv.contains(&"pr".to_owned()))
            .collect();
        assert!(!published.is_empty(), "{account} never published");
        for (argv, env) in published {
            assert!(
                env.iter()
                    .any(|(name, value)| name == "GH_CONFIG_DIR"
                        && value == &format!("/cfg/{account}")),
                "publishing call {argv:?} did not select {account}: {env:?}"
            );
            assert!(
                !env.iter().any(|(_, value)| value.contains(other)),
                "publishing call {argv:?} leaked the other repository's identity: {env:?}"
            );
        }
    }
}

/// Fail-closed: no declared identity means no authoring and no publishing,
/// with a diagnostic that names the repository rather than a generic refusal.
#[test]
fn a_repository_without_a_declared_identity_refuses_to_author_or_publish() {
    let (_temp, ownership, policy) = fixture(None);
    let runner = RecordingRunner::resolving_to("whoever-was-active");

    let error = deliver_with(&ownership, &policy, "repo/undeclared", &runner).unwrap_err();

    assert!(
        error.contains("repo/undeclared"),
        "the refusal must name the repository: {error}"
    );
    assert!(
        error.contains("identity"),
        "the refusal must say what is missing: {error}"
    );
    assert!(
        runner.argv().is_empty(),
        "nothing may run before an identity is resolved, but these did: {:?}",
        runner.argv()
    );
}

/// The motivating failure, made legible. An adapter resolving to an account
/// other than the declared one must stop publication with a diagnostic naming
/// both — not pass the call through to return an opaque provider error.
#[test]
fn an_identity_mismatch_is_refused_with_both_identities_named() {
    let (_temp, ownership, policy) = fixture(Some(identity("alpha", "alpha@example.invalid")));
    // The adapter is logged in as somebody else entirely.
    let runner = RecordingRunner::resolving_to("someone-else");

    let error = deliver_with(&ownership, &policy, "repo/alpha", &runner).unwrap_err();

    assert!(
        error.contains("alpha"),
        "the diagnostic must name the declared identity: {error}"
    );
    assert!(
        error.contains("someone-else"),
        "the diagnostic must name the observed identity: {error}"
    );
    assert!(
        error.contains("repo/alpha"),
        "the diagnostic must name the repository: {error}"
    );
    assert!(
        !runner
            .argv()
            .iter()
            .any(|argv| argv.contains(&"create".to_owned())),
        "publication must not be attempted after a mismatch: {:?}",
        runner.argv()
    );

    // The journal records the identity failure as the reason, so a resumed
    // delivery does not rediscover it as a provider outage.
    let journal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ownership.with_extension("delivery.json")).unwrap())
            .unwrap();
    assert_eq!(journal["phase"], "identity_mismatch");
    assert!(
        journal["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("someone-else")),
        "journal detail must carry the mismatch: {journal}"
    );
}

/// Identity is selected per invocation and never by toggling machine-global
/// state. A global toggle is a race under concurrent workers: two overlapping
/// deliveries would each publish under the other's account depending on
/// scheduling.
#[test]
fn delivery_never_mutates_machine_global_identity() {
    let (_temp, ownership, policy) = fixture(Some(identity("alpha", "alpha@example.invalid")));
    let runner = RecordingRunner::resolving_to("alpha");
    deliver_with(&ownership, &policy, "repo/alpha", &runner).unwrap();

    let calls = runner.argv();
    assert!(!calls.is_empty(), "the delivery ran no commands");
    for argv in &calls {
        let lowered: Vec<String> = argv.iter().map(|a| a.to_ascii_lowercase()).collect();
        let has = |needle: &str| lowered.iter().any(|a| a == needle);
        assert!(
            !(has("auth") && (has("switch") || has("login") || has("logout"))),
            "delivery switched the adapter's active account: {argv:?}"
        );
        assert!(
            !(has("config") && (has("--global") || has("--system"))),
            "delivery wrote global or system git configuration: {argv:?}"
        );
    }
}

/// Configuration is the other place the global toggle could sneak in, so the
/// policy refuses to validate an adapter argv that switches accounts.
#[test]
fn a_provider_argv_that_switches_accounts_is_refused_by_validation() {
    let (_temp, _ownership, mut policy) = fixture(Some(identity("alpha", "alpha@example.invalid")));
    policy.provider_argv = vec!["gh".into(), "auth".into(), "switch".into()];

    let error = policy.validate().unwrap_err();
    assert!(
        error.contains("machine-global identity"),
        "validation must explain what it refused: {error}"
    );
}

/// Identity is a descriptor. A bearer secret placed in `provider_env` would
/// reach process arguments and diagnostics, which the credential contract
/// forbids, so it is refused in configuration rather than redacted later.
#[test]
fn a_secret_shaped_provider_env_name_is_refused() {
    let mut declared = identity("alpha", "alpha@example.invalid");
    declared
        .provider_env
        .insert("GH_TOKEN".to_owned(), "ghp_not_a_real_token".to_owned());
    let (_temp, _ownership, policy) = fixture(Some(declared));

    let error = policy.validate().unwrap_err();
    assert!(
        error.contains("GH_TOKEN"),
        "validation must name the offending variable: {error}"
    );
    assert!(
        !error.contains("ghp_not_a_real_token"),
        "the diagnostic must not echo the value it refused: {error}"
    );
}
