use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

use crate::*;

#[derive(Debug, Clone)]
pub struct CapturedDiff {
    pub base_revision: String,
    pub resulting_tree: String,
    pub changed_files: Vec<ChangedFile>,
    pub diff: EvidenceRef,
    pub bytes: Vec<u8>,
}

pub trait EvidenceCollector {
    fn capture(
        &self,
        repository: &Path,
        base_revision: &str,
    ) -> Result<CapturedDiff, EvidenceError>;
}

#[derive(Debug, Clone)]
pub struct GitEvidenceCollector {
    artifact_directory: PathBuf,
    max_diff_bytes: u64,
}
impl GitEvidenceCollector {
    pub fn new(artifact_directory: PathBuf, max_diff_bytes: u64) -> Self {
        Self {
            artifact_directory,
            max_diff_bytes,
        }
    }
}

impl EvidenceCollector for GitEvidenceCollector {
    fn capture(
        &self,
        repository: &Path,
        base_revision: &str,
    ) -> Result<CapturedDiff, EvidenceError> {
        fs::create_dir_all(&self.artifact_directory)?;
        let resulting_tree = snapshot_tree(repository, &self.artifact_directory, base_revision)?;
        let diff = git(
            repository,
            &[
                "diff",
                "--binary",
                "--no-ext-diff",
                "--find-renames",
                base_revision,
                &resulting_tree,
                "--",
            ],
        )?;
        let names = git(
            repository,
            &[
                "diff",
                "--name-status",
                "-z",
                "--find-renames",
                base_revision,
                &resulting_tree,
                "--",
            ],
        )?;
        let mut changed_files = parse_name_status(&names)?;
        for file in &mut changed_files {
            file.line_summary =
                changed_ranges(repository, base_revision, &resulting_tree, &file.path)?;
        }
        let hash = content_hash(&diff);
        let path = self.artifact_directory.join(&hash);
        if contains_secret(&diff) {
            return Err(EvidenceError::SecretDetected);
        }
        fs::write(&path, &diff)?;
        let size = u64::try_from(diff.len()).map_err(|_| EvidenceError::Overflow)?;
        if size > self.max_diff_bytes {
            return Err(EvidenceError::DiffTooLarge {
                size,
                maximum: self.max_diff_bytes,
            });
        }
        Ok(CapturedDiff {
            base_revision: base_revision.into(),
            resulting_tree,
            changed_files,
            diff: EvidenceRef {
                content_hash: hash.clone(),
                media_type: "text/x-diff".into(),
                byte_size: size,
                repository: repository.to_string_lossy().replace('\\', "/"),
                revision: format!("baseline:{base_revision};diff:{hash}"),
                storage_ref: path.to_string_lossy().into(),
                truncated: false,
                omitted_bytes: 0,
            },
            bytes: diff,
        })
    }
}

static SNAPSHOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn snapshot_tree(
    repository: &Path,
    artifact_directory: &Path,
    base_revision: &str,
) -> Result<String, EvidenceError> {
    let index = artifact_directory.join(format!(
        ".review-index-{}-{}",
        std::process::id(),
        SNAPSHOT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let run = |args: &[&str]| -> Result<Vec<u8>, EvidenceError> {
        let output = Command::new("git")
            .args(args)
            .current_dir(repository)
            .env("GIT_INDEX_FILE", &index)
            .output()?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(EvidenceError::Git(
                String::from_utf8_lossy(&output.stderr).trim().into(),
            ))
        }
    };
    let result = (|| {
        // Seed from the baseline rather than emptying the index. `git add -A`
        // applies ignore rules only to files the index does not already track,
        // so an empty index makes every tracked-but-ignored file (a lockfile
        // under a `*.lock` rule, say) invisible to the scan — it never enters
        // the tree and the base-to-tree diff reports it as Deleted. Seeding
        // from the baseline keeps those files tracked, while genuine deletions
        // are still staged by `-A` and untracked ignored files still stay out.
        run(&["read-tree", base_revision])?;
        run(&["add", "-A"])?;
        let value = run(&["write-tree"])?;
        String::from_utf8(value)
            .map(|value| value.trim().to_owned())
            .map_err(|_| EvidenceError::MalformedGit)
    })();
    match fs::remove_file(&index) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(EvidenceError::Io(error)),
    }
    result
}

fn changed_ranges(
    repository: &Path,
    base: &str,
    tree: &str,
    path: &str,
) -> Result<Vec<LineRange>, EvidenceError> {
    let bytes = git(
        repository,
        &["diff", "--unified=0", "--no-color", base, tree, "--", path],
    )?;
    let text = String::from_utf8(bytes).map_err(|_| EvidenceError::MalformedGit)?;
    let mut ranges = Vec::new();
    for line in text.lines().filter(|line| line.starts_with("@@ ")) {
        let mut fields = line.split_whitespace();
        fields.next();
        let old = fields.next().ok_or(EvidenceError::MalformedGit)?;
        let new = fields.next().ok_or(EvidenceError::MalformedGit)?;
        let (new_start, new_count) = parse_range(new, '+')?;
        let (start, count) = if new_count == 0 {
            parse_range(old, '-')?
        } else {
            (new_start, new_count)
        };
        if count > 0 {
            ranges.push(LineRange {
                start,
                end: start
                    .checked_add(count - 1)
                    .ok_or(EvidenceError::Overflow)?,
            });
        }
    }
    Ok(ranges)
}

fn parse_range(value: &str, prefix: char) -> Result<(u64, u64), EvidenceError> {
    let value = value
        .strip_prefix(prefix)
        .ok_or(EvidenceError::MalformedGit)?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Ok((
        start.parse().map_err(|_| EvidenceError::MalformedGit)?,
        count.parse().map_err(|_| EvidenceError::MalformedGit)?,
    ))
}

fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, EvidenceError> {
    let out = Command::new("git").args(args).current_dir(repo).output()?;
    if !out.status.success() {
        return Err(EvidenceError::Git(
            String::from_utf8_lossy(&out.stderr).trim().into(),
        ));
    }
    Ok(out.stdout)
}
/// Identity of a scope finding's decision substance: the finding with its
/// policy snapshot hash normalized out. A durable human approval keyed this
/// way survives unrelated landings that rotate the compiled policy hash,
/// while any change to the path, rule, class, or decision still invalidates
/// it (PRD-080).
pub fn scope_finding_substance_hash(finding: &crate::ScopeFinding) -> String {
    let mut canonical = finding.clone();
    canonical.policy_snapshot_hash = String::new();
    content_hash(
        serde_json::to_string(&canonical)
            .unwrap_or_default()
            .as_bytes(),
    )
}

/// The waivable substance of a review finding: its category and the sorted
/// set of evidenced paths and verification checks. Reviewers re-issue the
/// same claim under fresh ids, re-worded prose, AND a varying subset of
/// citations (FAM-BUG-044), so this is stored as data rather than a hash:
/// a waiver covers a later finding of the same category whose citations are
/// a subset of the ones the human saw.
pub fn review_finding_substance(finding: &crate::ReviewFinding) -> (String, Vec<String>) {
    let category = serde_json::to_string(&finding.category)
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned();
    let mut cited: Vec<String> = finding
        .evidence
        .iter()
        .filter_map(|evidence| match evidence {
            crate::FindingEvidence::FileRange { path, .. }
            | crate::FindingEvidence::DiffHunk { path, .. } => Some(format!("path:{path}")),
            crate::FindingEvidence::Verification { check_id, .. } => {
                Some(format!("check:{check_id}"))
            }
            crate::FindingEvidence::Invariant {
                source, section, ..
            } => Some(format!("invariant:{source}#{section}")),
            crate::FindingEvidence::Contract { .. } | crate::FindingEvidence::Artifact { .. } => {
                None
            }
        })
        .collect();
    cited.sort();
    cited.dedup();
    (category, cited)
}

/// Serialized substance for durable storage.
pub fn review_finding_substance_json(finding: &crate::ReviewFinding) -> String {
    let (category, cited) = review_finding_substance(finding);
    serde_json::json!({"category": category, "cited": cited}).to_string()
}

/// Whether a durable waiver recorded against `waived` covers `finding`.
/// Same category, and everything the new finding cites was already visible
/// in what the human waived.
pub fn waiver_covers(waived_substance_json: &str, finding: &crate::ReviewFinding) -> bool {
    let Ok(waived) = serde_json::from_str::<serde_json::Value>(waived_substance_json) else {
        return false;
    };
    let (category, cited) = review_finding_substance(finding);
    if waived.get("category").and_then(|v| v.as_str()) != Some(category.as_str()) {
        return false;
    }
    let Some(waived_cited) = waived.get("cited").and_then(|v| v.as_array()) else {
        return false;
    };
    let waived_set: std::collections::BTreeSet<&str> =
        waived_cited.iter().filter_map(|v| v.as_str()).collect();
    !cited.is_empty() && cited.iter().all(|item| waived_set.contains(item.as_str()))
}

pub fn content_hash(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let hex = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}
pub fn contains_secret(bytes: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    // Value-shaped detection: a marker alone is legitimately quotable (a
    // redaction pattern list quotes every one of these); a marker adjoined
    // to a token-like value is a credential. For PEM headers the "value" is
    // the base64 body that follows a real key's header line.
    [
        "-----begin private key-----",
        "-----begin rsa private key-----",
        "aws_secret_access_key",
        "authorization: bearer ",
        "github_pat_",
        "sk-proj-",
    ]
    .iter()
    .any(|marker| marker_has_value(&lower, marker))
}

fn marker_has_value(haystack: &str, marker: &str) -> bool {
    let mut from = 0;
    while let Some(found) = haystack[from..].find(marker) {
        let after = &haystack[from + found + marker.len()..];
        let value = after.trim_start_matches(|c: char| {
            c.is_ascii_whitespace() || matches!(c, '"' | '\'' | '=' | ':')
        });
        let token_len = value
            .bytes()
            .take_while(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b'/')
            })
            .count();
        if token_len >= 16 {
            return true;
        }
        from += found + marker.len();
    }
    false
}
fn parse_name_status(bytes: &[u8]) -> Result<Vec<ChangedFile>, EvidenceError> {
    let fields: Vec<&[u8]> = bytes.split(|b| *b == 0).filter(|f| !f.is_empty()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let status = std::str::from_utf8(fields[i]).map_err(|_| EvidenceError::MalformedGit)?;
        i += 1;
        let code = status.chars().next().ok_or(EvidenceError::MalformedGit)?;
        let (kind, old_path) = match code {
            'A' => (GitChangeKind::Added, None),
            'M' => (GitChangeKind::Modified, None),
            'D' => (GitChangeKind::Deleted, None),
            'T' => (GitChangeKind::TypeChanged, None),
            'U' => (GitChangeKind::Unmerged, None),
            'R' | 'C' => {
                let old = path(fields.get(i).ok_or(EvidenceError::MalformedGit)?)?;
                i += 1;
                (
                    if code == 'R' {
                        GitChangeKind::Renamed
                    } else {
                        GitChangeKind::Copied
                    },
                    Some(old),
                )
            }
            _ => return Err(EvidenceError::MalformedGit),
        };
        let current = path(fields.get(i).ok_or(EvidenceError::MalformedGit)?)?;
        i += 1;
        out.push(ChangedFile {
            path: current,
            kind,
            old_path,
            line_summary: Vec::new(),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}
fn path(value: &[u8]) -> Result<String, EvidenceError> {
    let p = std::str::from_utf8(value).map_err(|_| EvidenceError::MalformedGit)?;
    if p.is_empty()
        || p.starts_with('/')
        || p.contains('\\')
        || p.split('/').any(|x| x == "." || x == "..")
    {
        return Err(EvidenceError::MalformedGit);
    }
    Ok(p.into())
}

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error("git evidence capture failed: {0}")]
    Git(String),
    #[error("malformed git path output")]
    MalformedGit,
    #[error("evidence size overflow")]
    Overflow,
    #[error("diff contains {size} bytes, exceeding evidence limit {maximum}")]
    DiffTooLarge { size: u64, maximum: u64 },
    #[error("evidence contains a deterministic secret marker and was not persisted")]
    SecretDetected,
    #[error("evidence I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Collect deterministic filesystem metadata required for scope evaluation.
/// External crate names a candidate ADDS to `Cargo.lock`, read from the
/// unified diff. Cargo.lock lists every resolved package, so a name that
/// appears only on the added side is third-party code that was not in the
/// build before. A name on both sides is a version bump or a re-ordering,
/// not new code (FAM-BUG-014).
fn lockfile_additions(diff: &[u8]) -> std::collections::BTreeSet<String> {
    let text = String::from_utf8_lossy(diff);
    let mut in_lockfile = false;
    let mut added = std::collections::BTreeSet::new();
    let mut removed = std::collections::BTreeSet::new();
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            in_lockfile = line.ends_with("Cargo.lock") || line.contains("/Cargo.lock ");
            continue;
        }
        if !in_lockfile {
            continue;
        }
        let (sink, rest) = match line.as_bytes().first() {
            Some(b'+') if !line.starts_with("+++") => (&mut added, &line[1..]),
            Some(b'-') if !line.starts_with("---") => (&mut removed, &line[1..]),
            _ => continue,
        };
        if let Some(value) = rest.trim().strip_prefix("name = ") {
            sink.insert(value.trim().trim_matches('"').to_owned());
        }
    }
    added.difference(&removed).cloned().collect()
}

pub fn collect_scope_evidence(
    repository: &Path,
    files: &[ChangedFile],
    diff: &[u8],
) -> Result<ScopeEvidence, EvidenceError> {
    let mut evidence = ScopeEvidence {
        new_external_crates: lockfile_additions(diff),
        ..ScopeEvidence::default()
    };
    for file in files {
        if file.kind == GitChangeKind::Deleted {
            continue;
        }
        match fs::symlink_metadata(repository.join(&file.path)) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                evidence.symlink_paths.insert(file.path.clone());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(EvidenceError::Io(error)),
        }
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lockfile_additions_name_only_new_external_crates() {
        // FAM-BUG-014: Cargo.lock is the resolved truth. A name present only
        // on the added side is third-party code entering the build; a name on
        // both sides is a version bump, not new code.
        let diff = b"diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n-name = \"serde\"\n+name = \"serde\"\n+name = \"brand-new-crate\"\n" as &[u8];
        let added = super::lockfile_additions(diff);
        assert_eq!(
            added.into_iter().collect::<Vec<_>>(),
            vec!["brand-new-crate".to_string()]
        );
        // Manifest-only churn outside Cargo.lock never counts.
        let manifest_only =
            b"diff --git a/Cargo.toml b/Cargo.toml\n+name = \"not-a-lock-entry\"\n" as &[u8];
        assert!(super::lockfile_additions(manifest_only).is_empty());
    }

    #[test]
    fn a_waiver_covers_the_same_claim_under_a_new_id_and_narrower_citations() {
        // FAM-BUG-044: reviewers re-issue one claim with fresh ids, re-worded
        // prose, and a varying subset of citations. A human waived the claim
        // they saw; a later, narrower restatement of it is covered.
        let make = |id: &str, paths: &[&str]| crate::ReviewFinding {
            finding_id: id.into(),
            category: crate::FindingCategory::ScopeViolation,
            severity: crate::FindingSeverity::Medium,
            blocking: true,
            title: format!("title for {id}"),
            claim: "outside allowed paths".into(),
            evidence: paths
                .iter()
                .map(|path| crate::FindingEvidence::FileRange {
                    path: (*path).into(),
                    range: crate::LineRange { start: 1, end: 2 },
                })
                .collect(),
            remediation: "relocate".into(),
            status: crate::FindingStatus::Open,
            supersedes: None,
            acceptance_criterion_id: None,
        };
        let waived = super::review_finding_substance_json(&make(
            "F9-changes-outside-allowed-paths",
            &["Cargo.toml", "Cargo.lock", "config/default.toml"],
        ));
        // Same claim, new id, subset of citations: covered.
        assert!(super::waiver_covers(
            &waived,
            &make("F5-changes-outside-allowed-paths", &["Cargo.toml"])
        ));
        // Identical set: covered.
        assert!(super::waiver_covers(
            &waived,
            &make("F1", &["Cargo.lock", "Cargo.toml", "config/default.toml"])
        ));
        // Cites something the human never saw: NOT covered.
        assert!(!super::waiver_covers(
            &waived,
            &make("F2", &["Cargo.toml", "crates/secret/src/lib.rs"])
        ));
        // Different category with the same paths: NOT covered.
        let mut other = make("F3", &["Cargo.toml"]);
        other.category = crate::FindingCategory::SecurityIssue;
        assert!(!super::waiver_covers(&waived, &other));
    }

    use super::*;
    use tempfile::tempdir;

    fn command(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn captures_modified_deleted_renamed_and_untracked_files_with_ranges() {
        let repository = tempdir().unwrap();
        let artifacts = tempdir().unwrap();
        command(repository.path(), &["init", "-q"]);
        fs::write(repository.path().join("modified.txt"), "one\ntwo\n").unwrap();
        fs::write(repository.path().join("deleted.txt"), "delete\n").unwrap();
        fs::write(repository.path().join("old.txt"), "rename unchanged\n").unwrap();
        command(repository.path(), &["add", "-A"]);
        command(repository.path(), &["commit", "-qm", "base"]);
        let baseline = snapshot_tree(repository.path(), artifacts.path(), "HEAD").unwrap();
        fs::write(repository.path().join("modified.txt"), "one\nchanged\n").unwrap();
        fs::remove_file(repository.path().join("deleted.txt")).unwrap();
        fs::rename(
            repository.path().join("old.txt"),
            repository.path().join("new.txt"),
        )
        .unwrap();
        fs::write(repository.path().join("untracked.txt"), "new\n").unwrap();
        let captured = GitEvidenceCollector::new(artifacts.path().into(), 1_000_000)
            .capture(repository.path(), &baseline)
            .unwrap();
        assert_eq!(captured.base_revision, baseline);
        assert_ne!(captured.resulting_tree, captured.base_revision);
        assert_eq!(captured.diff.content_hash, content_hash(&captured.bytes));
        let by_path: std::collections::BTreeMap<_, _> = captured
            .changed_files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        assert_eq!(by_path["modified.txt"].kind, GitChangeKind::Modified);
        assert_eq!(by_path["deleted.txt"].kind, GitChangeKind::Deleted);
        assert_eq!(by_path["new.txt"].kind, GitChangeKind::Renamed);
        assert_eq!(by_path["new.txt"].old_path.as_deref(), Some("old.txt"));
        assert_eq!(by_path["untracked.txt"].kind, GitChangeKind::Added);
        assert!(by_path.values().all(|file| !file.line_summary.is_empty()));
    }

    #[test]
    fn tracked_but_ignored_files_are_not_reported_as_deleted() {
        // A file can be both tracked and matched by an ignore rule: the rule
        // was added after the file, or it is broad (`*.lock`) and the file was
        // force-added. Git keeps tracking it, so an untouched one is not a
        // change and must not surface as a deletion — a lockfile deletion is
        // classified as a human-review stop and would halt an unattended run.
        let repository = tempdir().unwrap();
        let artifacts = tempdir().unwrap();
        command(repository.path(), &["init", "-q"]);
        fs::write(repository.path().join(".gitignore"), "*.lock\nbuild/\n").unwrap();
        fs::write(repository.path().join("Cargo.lock"), "locked\n").unwrap();
        fs::write(repository.path().join("kept.lock"), "kept\n").unwrap();
        fs::write(repository.path().join("src.rs"), "code\n").unwrap();
        command(repository.path(), &["add", "-A", "-f"]);
        command(repository.path(), &["commit", "-qm", "base"]);
        // The baseline is a real commit, exactly as a run supplies it. Deriving
        // it from `snapshot_tree` instead would hide the defect: both trees
        // would omit the ignored files symmetrically and the diff would be
        // empty rather than wrong.
        let baseline = String::from_utf8(git(repository.path(), &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();

        // Touch nothing ignored except one genuine deletion, and drop an
        // untracked ignored artifact in the way of the scan.
        fs::write(repository.path().join("src.rs"), "changed\n").unwrap();
        fs::remove_file(repository.path().join("kept.lock")).unwrap();
        fs::create_dir(repository.path().join("build")).unwrap();
        fs::write(repository.path().join("build/out.o"), "binary\n").unwrap();

        let captured = GitEvidenceCollector::new(artifacts.path().into(), 1_000_000)
            .capture(repository.path(), &baseline)
            .unwrap();
        let by_path: std::collections::BTreeMap<_, _> = captured
            .changed_files
            .iter()
            .map(|file| (file.path.as_str(), file.kind))
            .collect();
        // The untouched tracked-but-ignored lockfile is absent from the change
        // set entirely; it is not a change of any kind.
        assert!(!by_path.contains_key("Cargo.lock"));
        // A real deletion of a tracked-but-ignored file is still reported.
        assert_eq!(by_path["kept.lock"], GitChangeKind::Deleted);
        assert_eq!(by_path["src.rs"], GitChangeKind::Modified);
        // Untracked and ignored stays out of the snapshot.
        assert!(!by_path.contains_key("build/out.o"));
    }

    #[cfg(unix)]
    #[test]
    fn scope_evidence_flags_symlinks_and_skips_deleted_and_missing_paths() {
        use std::os::unix::fs::symlink;
        let repository = tempdir().unwrap();
        symlink("target", repository.path().join("link")).unwrap();
        fs::write(repository.path().join("plain.rs"), "code\n").unwrap();
        let file = |path: &str, kind| ChangedFile {
            path: path.into(),
            kind,
            old_path: None,
            line_summary: vec![],
        };
        let evidence = collect_scope_evidence(
            repository.path(),
            &[
                file("link", GitChangeKind::Added),
                file("plain.rs", GitChangeKind::Modified),
                file("gone.rs", GitChangeKind::Deleted),
                file("never-existed.rs", GitChangeKind::Modified),
            ],
            b"",
        )
        .unwrap();
        assert_eq!(
            evidence.symlink_paths.into_iter().collect::<Vec<_>>(),
            vec!["link".to_owned()]
        );
    }

    #[test]
    fn quoted_marker_list_is_not_a_secret() {
        // A redaction implementation legitimately quotes every marker; the
        // scanner must not deadlock the PRD that implements redaction.
        let diff = br#"+    "aws_secret_access_key",
+    "authorization: bearer ",
+    "github_pat_",
+    "sk-proj-","#;
        assert!(!contains_secret(diff));
    }

    #[test]
    fn marker_with_token_like_value_is_a_secret() {
        for leak in [
            "aws_secret_access_key = \"wJalrXUtnFEMIK7MDENGbPxRfiCY\"",
            "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            "github_pat_11ABCDEFG0123456789abcdef",
            "sk-proj-abcdefghijklmnop123456",
        ] {
            assert!(contains_secret(leak.as_bytes()), "{leak}");
        }
    }

    #[test]
    fn private_key_header_with_body_is_a_secret_and_quoted_header_is_not() {
        assert!(contains_secret(
            b"-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEA\n"
        ));
        assert!(!contains_secret(
            b"+    \"-----begin private key-----\",\n+    \"-----begin rsa private key-----\","
        ));
    }
}
