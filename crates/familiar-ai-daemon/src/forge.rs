//! PRD-097: the forge verb grammar delivery speaks, kept out of
//! `delivery.rs` on purpose. Every verb Familiar issues to a forge — publish,
//! locate, wait on checks, query one named check, merge, comment — resolves
//! through an adapter here rather than being spelled inline at the call
//! site. See `docs/contracts/forge-adapters.md` for the closed verb
//! vocabulary this module implements.
//!
//! `github` reproduces today's `gh` grammar exactly, byte for byte, so the
//! existing delivery tests keep passing unmodified. `gitlab` mirrors `glab`,
//! which itself mirrors `gh` — the cheap proof that the abstraction covers a
//! near-clone. `gitea` (`tea`) has a genuinely different grammar and
//! declines the two checks-related verbs it has no CLI surface for, which is
//! what exercises the decline path rather than a silent skip.

use serde::{Deserialize, Serialize};

use familiar_ai_core::Forge;

/// A change request's opaque, adapter-supplied identity, plus the URL where
/// the adapter reports one. Nothing in delivery parses or does arithmetic on
/// `id` — it is only ever passed back to the same adapter's own verbs, so it
/// must stay in the spelling those verbs accept as an argument (GitLab's
/// bare `123`, not the `!123` a human reads on the merge request page).
/// `display`, when an adapter's argument spelling and human spelling
/// diverge, carries the human one; callers that show a change request to a
/// person use `display.unwrap_or(&id)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeRequestId {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// One forge verb's result: either the argv delivery should run, or an
/// explicit, typed refusal. A decline is a legitimate outcome delivery must
/// handle, never a silent skip that lets a mode claim an authority it never
/// exercised, and never an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeCall {
    Run(Vec<String>),
    Declined,
}

fn run(values: &[&str]) -> ForgeCall {
    ForgeCall::Run(values.iter().map(|value| (*value).to_owned()).collect())
}

/// Publish a change request from `branch` against `base`.
pub fn publish(forge: Forge, base: &str, branch: &str) -> ForgeCall {
    match forge {
        Forge::Github => run(&["pr", "create", "--fill", "--base", base, "--head", branch]),
        Forge::Gitlab => run(&[
            "mr",
            "create",
            "--fill",
            "--target-branch",
            base,
            "--source-branch",
            branch,
        ]),
        Forge::Gitea => ForgeCall::Run(
            [
                "pulls", "create", "--base", base, "--head", branch, "--title", branch,
            ]
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        ),
        Forge::None => ForgeCall::Declined,
    }
}

/// Locate the change request already open for `branch`, if any. Its stdout
/// is handed to [`parse_change_request`].
pub fn locate(forge: Forge, branch: &str) -> ForgeCall {
    match forge {
        Forge::Github => run(&["pr", "view", branch, "--json", "number", "--jq", ".number"]),
        Forge::Gitlab => run(&["mr", "view", branch, "--output", "json", "--jq", ".iid"]),
        Forge::Gitea => ForgeCall::Run(
            [
                "pulls", "ls", "--head", branch, "--fields", "index", "--output", "simple",
            ]
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        ),
        Forge::None => ForgeCall::Declined,
    }
}

/// Parse a `locate` invocation's stdout into the opaque identity the adapter
/// reports. `None` means the adapter found nothing — not a failure by
/// itself; the caller decides whether that is a problem.
pub fn parse_change_request(forge: Forge, stdout: &str) -> Option<ChangeRequestId> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    match forge {
        // `gh pr view --json number --jq .number` prints a bare integer on
        // success. Anything else on stdout — a warning line, an upgrade
        // notice, `--jq` resolving to `null` — is not a change request
        // identifier; rejecting it here keeps the pre-PRD-097 fail-closed
        // diagnostic (`did not return a change request identifier`) intact
        // instead of feeding stray text into `gh pr checks`/`gh pr merge`.
        Forge::Github => trimmed.parse::<u64>().ok().map(|_| ChangeRequestId {
            id: trimmed.to_owned(),
            display: None,
            url: None,
        }),
        // `glab` identifies a merge request by its bare IID; `!123` is only
        // how GitLab displays that IID to a human, so it goes in `display`
        // rather than the id every verb below feeds back into argv.
        Forge::Gitlab => Some(ChangeRequestId {
            id: trimmed.to_owned(),
            display: Some(format!("!{trimmed}")),
            url: None,
        }),
        Forge::Gitea => Some(ChangeRequestId {
            id: trimmed.to_owned(),
            display: None,
            url: None,
        }),
        Forge::None => None,
    }
}

/// Wait on `change`'s checks to complete. `tea` has no CI-awareness in its
/// grammar, so `gitea` declines this rather than pretending to watch.
pub fn wait_checks(forge: Forge, change: &str) -> ForgeCall {
    match forge {
        Forge::Github => run(&["pr", "checks", change, "--watch", "--fail-fast"]),
        Forge::Gitlab => run(&["mr", "checks", change]),
        Forge::Gitea => ForgeCall::Declined,
        Forge::None => ForgeCall::Declined,
    }
}

/// Query one named check on `change`.
pub fn check_named(forge: Forge, change: &str, check: &str) -> ForgeCall {
    match forge {
        Forge::Github => run(&["pr", "check", change, check]),
        Forge::Gitlab => ForgeCall::Run(vec![
            "mr".into(),
            "checks".into(),
            change.into(),
            check.into(),
        ]),
        Forge::Gitea => ForgeCall::Declined,
        Forge::None => ForgeCall::Declined,
    }
}

/// Merge `change`, deleting the source branch.
pub fn merge(forge: Forge, change: &str) -> ForgeCall {
    match forge {
        Forge::Github => run(&["pr", "merge", change, "--merge", "--delete-branch"]),
        Forge::Gitlab => run(&["mr", "merge", change, "--remove-source-branch"]),
        Forge::Gitea => ForgeCall::Run(vec!["pulls".into(), "merge".into(), change.into()]),
        Forge::None => ForgeCall::Declined,
    }
}

/// Comment on `change` with `body`.
pub fn comment(forge: Forge, change: &str, body: &str) -> ForgeCall {
    match forge {
        Forge::Github => ForgeCall::Run(vec![
            "pr".into(),
            "comment".into(),
            change.into(),
            "--body".into(),
            body.into(),
        ]),
        Forge::Gitlab => ForgeCall::Run(vec![
            "mr".into(),
            "note".into(),
            change.into(),
            "--message".into(),
            body.into(),
        ]),
        Forge::Gitea => ForgeCall::Run(vec![
            "comment".into(),
            "create".into(),
            "--issue".into(),
            change.into(),
            "--body".into(),
            body.into(),
        ]),
        Forge::None => ForgeCall::Declined,
    }
}

/// Diagnostic name for the declined-verb messages delivery journals.
pub fn name(forge: Forge) -> &'static str {
    match forge {
        Forge::Github => "github",
        Forge::Gitlab => "gitlab",
        Forge::Gitea => "gitea",
        Forge::None => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_reproduces_todays_gh_grammar_exactly() {
        assert_eq!(
            publish(Forge::Github, "main", "familiar/session/PRD-1"),
            run(&[
                "pr",
                "create",
                "--fill",
                "--base",
                "main",
                "--head",
                "familiar/session/PRD-1"
            ])
        );
        assert_eq!(
            locate(Forge::Github, "familiar/session/PRD-1"),
            run(&[
                "pr",
                "view",
                "familiar/session/PRD-1",
                "--json",
                "number",
                "--jq",
                ".number"
            ])
        );
        assert_eq!(
            wait_checks(Forge::Github, "42"),
            run(&["pr", "checks", "42", "--watch", "--fail-fast"])
        );
        assert_eq!(
            check_named(Forge::Github, "42", "ci/build"),
            run(&["pr", "check", "42", "ci/build"])
        );
        assert_eq!(
            merge(Forge::Github, "42"),
            run(&["pr", "merge", "42", "--merge", "--delete-branch"])
        );
        assert_eq!(
            comment(Forge::Github, "42", "blocked"),
            run(&["pr", "comment", "42", "--body", "blocked"])
        );
        assert_eq!(
            parse_change_request(Forge::Github, "42\n"),
            Some(ChangeRequestId {
                id: "42".into(),
                display: None,
                url: None
            })
        );
    }

    #[test]
    fn github_rejects_non_numeric_locate_output_as_no_change_request() {
        for stdout in ["null", "warning: gh upgrade available\n", "", "  \n"] {
            assert_eq!(
                parse_change_request(Forge::Github, stdout),
                None,
                "stdout {stdout:?} must not be accepted as a change request id"
            );
        }
    }

    #[test]
    fn gitlab_grammar_differs_from_github_and_keeps_a_bare_id_with_a_bang_display() {
        assert_eq!(
            publish(Forge::Gitlab, "main", "familiar/session/PRD-1"),
            run(&[
                "mr",
                "create",
                "--fill",
                "--target-branch",
                "main",
                "--source-branch",
                "familiar/session/PRD-1"
            ])
        );
        assert_ne!(
            publish(Forge::Gitlab, "main", "b"),
            publish(Forge::Github, "main", "b")
        );
        let parsed = parse_change_request(Forge::Gitlab, "123\n").unwrap();
        assert_eq!(
            parsed,
            ChangeRequestId {
                id: "123".into(),
                display: Some("!123".into()),
                url: None
            }
        );
        // The id is what every GitLab verb below consumes as an argument —
        // it must stay bare, never the `!123` a human reads.
        assert_eq!(
            merge(Forge::Gitlab, &parsed.id),
            run(&["mr", "merge", "123", "--remove-source-branch"])
        );
        assert_eq!(
            wait_checks(Forge::Gitlab, &parsed.id),
            run(&["mr", "checks", "123"])
        );
        assert_eq!(
            comment(Forge::Gitlab, &parsed.id, "blocked"),
            run(&["mr", "note", "123", "--message", "blocked"])
        );
    }

    #[test]
    fn gitea_grammar_genuinely_differs_and_declines_check_verbs() {
        assert_eq!(
            publish(Forge::Gitea, "main", "familiar/session/PRD-1"),
            ForgeCall::Run(vec![
                "pulls".into(),
                "create".into(),
                "--base".into(),
                "main".into(),
                "--head".into(),
                "familiar/session/PRD-1".into(),
                "--title".into(),
                "familiar/session/PRD-1".into(),
            ])
        );
        assert_eq!(wait_checks(Forge::Gitea, "7"), ForgeCall::Declined);
        assert_eq!(
            check_named(Forge::Gitea, "7", "ci/build"),
            ForgeCall::Declined
        );
        assert_eq!(
            merge(Forge::Gitea, "7"),
            ForgeCall::Run(vec!["pulls".into(), "merge".into(), "7".into()])
        );
    }

    #[test]
    fn none_declines_every_verb() {
        for call in [
            publish(Forge::None, "main", "branch"),
            locate(Forge::None, "branch"),
            wait_checks(Forge::None, "1"),
            check_named(Forge::None, "1", "check"),
            merge(Forge::None, "1"),
            comment(Forge::None, "1", "body"),
        ] {
            assert_eq!(call, ForgeCall::Declined);
        }
        assert_eq!(parse_change_request(Forge::None, "anything"), None);
    }

    #[test]
    fn a_gerrit_style_change_id_round_trips_through_json_without_lossy_parsing() {
        let change = ChangeRequestId {
            id: "Iabc123def4560789abcdef0123456789abcdef0".into(),
            display: None,
            url: Some("https://gerrit.example/c/repo/+/4200".into()),
        };
        let round_tripped: ChangeRequestId =
            serde_json::from_str(&serde_json::to_string(&change).unwrap()).unwrap();
        assert_eq!(round_tripped, change);

        let gitlab_style = ChangeRequestId {
            id: "123".into(),
            display: Some("!123".into()),
            url: None,
        };
        let round_tripped: ChangeRequestId =
            serde_json::from_str(&serde_json::to_string(&gitlab_style).unwrap()).unwrap();
        assert_eq!(round_tripped, gitlab_style);
    }
}
