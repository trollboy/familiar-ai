//! The bug log is countable, or it is not a log.
//!
//! On 2026-09-21 the question "how many bugs are open" had no answer a
//! command could give. Six of sixty-two ids were recorded as bullets inside
//! dated narrative rather than as entries, so a count following the file's own
//! convention simply could not see them — and four of those six had been
//! written that day, by me. One entry's status read "Retracted", which is a
//! resolution no classifier recognises without knowing the history.
//!
//! These assertions make the convention a contract: every id has an entry,
//! every entry declares a state a reader can classify, and both are true of
//! the next bug as well as this one.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn bug_log() -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs/running_bugs.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must exist: {e}", path.display()))
}

/// `### FAM-BUG-NNN` headings, in file order.
fn entries(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("### "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|token| token.starts_with("FAM-BUG-"))
        .map(|token| token.trim_end_matches(&[',', '.'][..]).to_string())
        .collect()
}

/// Every id mentioned anywhere in the file.
fn mentioned(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (index, _) in body.match_indices("FAM-BUG-") {
        let digits: String = body[index + "FAM-BUG-".len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            out.insert(format!("FAM-BUG-{digits}"));
        }
    }
    out
}

#[test]
fn every_bug_mentioned_has_an_entry_of_its_own() {
    let body = bug_log();
    let headed: BTreeSet<String> = entries(&body).into_iter().collect();
    let missing: Vec<_> = mentioned(&body).difference(&headed).cloned().collect();
    assert!(
        missing.is_empty(),
        "these ids appear only in prose, so no count can find them: {missing:?}\n\
         Give each a `### FAM-BUG-NNN — title` entry with a `- **Status:**` line."
    );
}

#[test]
fn every_entry_declares_a_state_a_reader_can_classify() {
    let body = bug_log();
    // `In progress` is two words and had to be matched as a phrase: taking
    // only the first word rejected it as "In". It is a real state the rest of
    // the system already names (`backlog_prds.status = 'in_progress'`) and is
    // distinct from `Open` — someone is on it right now.
    const KNOWN: [&str; 5] = ["Open", "Fixed", "Closed", "Reopened", "In progress"];

    let mut offenders = Vec::new();
    let mut current: Option<String> = None;
    let mut saw_status = true;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("### ") {
            if let Some(id) = rest.split_whitespace().next() {
                if id.starts_with("FAM-BUG-") {
                    if !saw_status {
                        offenders.push(format!("{} has no Status line", current.clone().unwrap()));
                    }
                    current = Some(id.to_string());
                    saw_status = false;
                    continue;
                }
            }
        }
        let Some(id) = current.clone() else { continue };
        if saw_status {
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("- **Status:**") {
            saw_status = true;
            let head = rest.trim_start();
            let matched = KNOWN.iter().any(|known| {
                head.get(..known.len())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(known))
            });
            if !matched {
                let shown: String = head.chars().take(24).collect();
                offenders.push(format!(
                    "{id} status starts with {shown:?}; use one of {KNOWN:?}"
                ));
            }
        }
    }
    if !saw_status {
        if let Some(id) = current {
            offenders.push(format!("{id} has no Status line"));
        }
    }

    assert!(
        offenders.is_empty(),
        "the open count is only as good as the status lines:\n{}",
        offenders.join("\n")
    );
}

/// The count itself, so a regression in the log shows up as a number moving
/// rather than as a reader's impression.
#[test]
fn the_open_count_is_answerable() {
    let body = bug_log();
    let mut open = Vec::new();
    let mut current: Option<String> = None;
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("### ") {
            if let Some(id) = rest.split_whitespace().next() {
                if id.starts_with("FAM-BUG-") {
                    current = Some(id.to_string());
                }
            }
            continue;
        }
        if let (Some(id), Some(rest)) = (
            current.clone(),
            line.trim_start().strip_prefix("- **Status:**"),
        ) {
            let first = rest.split_whitespace().next().unwrap_or("");
            if first.eq_ignore_ascii_case("Open") || first.eq_ignore_ascii_case("Reopened") {
                open.push(id);
            }
            current = None;
        }
    }
    // Not pinned to a number: bugs are found and fixed, and a test that fails
    // whenever that happens would be deleted within a week. What is pinned is
    // that the answer exists and is derived, not remembered.
    assert!(
        !open.is_empty(),
        "no open bugs at all is more likely a parse failure than a clean tree"
    );
    println!("open bugs: {} — {open:?}", open.len());
}
