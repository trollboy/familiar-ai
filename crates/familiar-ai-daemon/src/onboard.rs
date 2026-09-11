//! Repository onboarding: turn an unmanaged repository into an
//! operator-approved, reviewable policy without changing Familiar code.
//!
//! `propose` reads repository content as untrusted, heuristic evidence and
//! never executes or interpolates anything it finds. `approve` consumes a
//! deterministic, human-authored answers file through the exact same
//! `RepositoryConfig` validation `Config::load` enforces, and only then
//! writes a generated policy file. `validate` proves the resulting
//! installation-wide configuration still loads. `fixture` proves the
//! resulting policy actually wires into context compilation, isolated
//! review, reporting, and the delivery boundary, without claiming a PRD or
//! invoking a model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use familiar_ai_context::{
    ContextBudget, ContextBudgeter, ContextDocument, DocumentKind, ExecutionContext,
    InclusionReason, RepositoryContext,
};
use familiar_ai_core::{
    AppPaths, BacklogDiscovery, Config, DeliveryMode, FilesystemBacklogDiscovery,
};
use familiar_ai_review::{CommandVerificationRunner, VerificationCheck, VerificationRunner};

use crate::worktree::WorktreeLease;

// ---------------------------------------------------------------------
// propose: untrusted, heuristic, read-only repository discovery
// ---------------------------------------------------------------------

const MAX_WALK_DEPTH: usize = 6;
const MAX_WALK_ENTRIES: usize = 20_000;

const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "vendor",
    ".venv",
    "venv",
    "dist",
    "build",
    ".idea",
    ".vscode",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
];

const LANGUAGE_EXTENSIONS: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("go", "Go"),
    ("py", "Python"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript"),
    ("java", "Java"),
    ("rb", "Ruby"),
    ("c", "C"),
    ("cpp", "C++"),
    ("cc", "C++"),
    ("cs", "C#"),
    ("php", "PHP"),
    ("swift", "Swift"),
    ("kt", "Kotlin"),
    ("scala", "Scala"),
];

struct BuildToolMarker {
    filename: &'static str,
    tool: &'static str,
    verification: &'static [&'static [&'static str]],
}

const BUILD_TOOL_MARKERS: &[BuildToolMarker] = &[
    BuildToolMarker {
        filename: "Cargo.toml",
        tool: "cargo",
        verification: &[
            &["cargo", "fmt", "--check"],
            &["cargo", "clippy", "--", "-D", "warnings"],
            &["cargo", "test"],
        ],
    },
    BuildToolMarker {
        filename: "package.json",
        tool: "npm",
        verification: &[&["npm", "test"]],
    },
    BuildToolMarker {
        filename: "go.mod",
        tool: "go",
        verification: &[&["go", "vet", "./..."], &["go", "test", "./..."]],
    },
    BuildToolMarker {
        filename: "pyproject.toml",
        tool: "pip",
        verification: &[&["pytest"]],
    },
    BuildToolMarker {
        filename: "requirements.txt",
        tool: "pip",
        verification: &[&["pytest"]],
    },
    BuildToolMarker {
        filename: "pom.xml",
        tool: "maven",
        verification: &[&["mvn", "test"]],
    },
    BuildToolMarker {
        filename: "build.gradle",
        tool: "gradle",
        verification: &[&["./gradlew", "test"]],
    },
    BuildToolMarker {
        filename: "Makefile",
        tool: "make",
        verification: &[&["make", "test"]],
    },
];

const PROTECTED_PATH_MARKERS: &[&str] = &[
    ".git",
    "migrations",
    "db/migrate",
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "go.sum",
    ".github",
    ".env",
    "secrets",
];

/// Untrusted evidence about a repository, derived only from file *names* and
/// extensions -- file content is never read, so nothing a hostile repository
/// contains can inject a command or configuration value here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryProposal {
    pub languages: Vec<String>,
    pub build_tools: Vec<String>,
    pub verification_commands: Vec<Vec<String>>,
    pub prd_profile: Option<String>,
    pub active_dir: Option<String>,
    pub archived_dir: Option<String>,
    pub protected_paths: Vec<String>,
}

pub fn discover(root: &Path) -> Result<RepositoryProposal, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    let mut build_tools = Vec::new();
    let mut verification_commands = Vec::new();
    for marker in BUILD_TOOL_MARKERS {
        if root.join(marker.filename).is_file() {
            build_tools.push(marker.tool.to_string());
            for argv in marker.verification {
                verification_commands.push(argv.iter().map(|s| s.to_string()).collect());
            }
        }
    }

    let mut protected_paths = BTreeSet::new();
    for candidate in PROTECTED_PATH_MARKERS {
        if root.join(candidate).exists() {
            protected_paths.insert((*candidate).to_string());
        }
    }

    let (prd_profile, active_dir, archived_dir) = if root.join("docs/prds").is_dir() {
        (
            Some("canonical".to_string()),
            Some("docs/prds".to_string()),
            Some("docs/prds/done".to_string()),
        )
    } else if root.join("docs/prd/todo").is_dir() && root.join("docs/prd/done").is_dir() {
        (
            Some("numbered-slug".to_string()),
            Some("docs/prd/todo".to_string()),
            Some("docs/prd/done".to_string()),
        )
    } else {
        (None, None, None)
    };

    let mut languages = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH || visited > MAX_WALK_ENTRIES {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_WALK_ENTRIES {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if SKIP_DIR_NAMES.contains(&name.as_ref()) {
                    continue;
                }
                stack.push((path, depth + 1));
                continue;
            }
            if let Some((_, language)) =
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .and_then(|ext| {
                        LANGUAGE_EXTENSIONS
                            .iter()
                            .find(|(candidate, _)| *candidate == ext)
                    })
            {
                languages.insert((*language).to_string());
            }
        }
    }

    Ok(RepositoryProposal {
        languages: languages.into_iter().collect(),
        build_tools,
        verification_commands,
        prd_profile,
        active_dir,
        archived_dir,
        protected_paths: protected_paths.into_iter().collect(),
    })
}

pub fn render_proposal(worktree: &Path, proposal: &RepositoryProposal) -> String {
    let mut out = String::new();
    out.push_str("# UNTRUSTED PROPOSAL\n");
    out.push_str("# Discovered from repository file names only; no file content was read and\n");
    out.push_str("# nothing below is active configuration. Review it, then author an answers\n");
    out.push_str(
        "# file and run `familiar-ai onboard approve --answers <file> --actor human:<you>`.\n\n",
    );
    out.push_str(&format!(
        "repository = {:?}\n",
        worktree.display().to_string()
    ));
    out.push_str(&format!("languages = {:?}\n", proposal.languages));
    out.push_str(&format!("build_tools = {:?}\n", proposal.build_tools));
    out.push_str(&format!("prd_profile = {:?}\n", proposal.prd_profile));
    out.push_str(&format!("active_dir = {:?}\n", proposal.active_dir));
    out.push_str(&format!("archived_dir = {:?}\n", proposal.archived_dir));
    out.push_str(&format!(
        "protected_paths = {:?}\n",
        proposal.protected_paths
    ));
    out.push_str("\n# Proposed (untrusted) verification commands. Copy any you accept into an\n");
    out.push_str("# answers file's [repositories.\"<worktree>\".review.verification] entries.\n");
    for argv in &proposal.verification_commands {
        out.push_str(&format!("# - {argv:?}\n"));
    }
    out
}

/// Writes the proposal to durable, human-facing state -- never to
/// `repositories_dir`, so a proposal grants no authority by construction.
pub fn write_proposal(
    state_dir: &Path,
    worktree: &Path,
    proposal: &RepositoryProposal,
) -> Result<PathBuf, String> {
    let dir = state_dir.join("onboarding-proposals");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.txt", slug(&worktree.display().to_string())));
    fs::write(&path, render_proposal(worktree, proposal)).map_err(|e| e.to_string())?;
    Ok(path)
}

fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_was_sep = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "repository".to_string()
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------
// approve: deterministic answers file -> validated, generated policy
// ---------------------------------------------------------------------

const POLICY_SENTINEL: &str = "# --- policy ---\n";

fn human(actor: &str) -> Result<(), String> {
    if matches!(actor.strip_prefix("human:"), Some(v) if !v.trim().is_empty() && !v.chars().any(char::is_control))
    {
        Ok(())
    } else {
        Err("human authority required: actor must be human:<identity>".into())
    }
}

fn data_section(full: &str) -> &str {
    match full.find(POLICY_SENTINEL) {
        Some(index) => full[index + POLICY_SENTINEL.len()..].trim_start_matches('\n'),
        None => full,
    }
}

fn explain_diff(old: &str, new: &str) -> Vec<String> {
    if old == new {
        return vec!["(no change)".to_string()];
    }
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut out = Vec::new();
    for line in &old_lines {
        if !new_lines.contains(line) {
            out.push(format!("- {line}"));
        }
    }
    for line in &new_lines {
        if !old_lines.contains(line) {
            out.push(format!("+ {line}"));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedSnapshot {
    pub worktree: String,
    pub generated_path: PathBuf,
    pub content_hash: String,
    pub actor: String,
    pub generated_at: String,
    pub diff: Vec<String>,
}

/// Validates a candidate repository policy under exactly the rules
/// `Config::load` enforces, writes it to `repositories_dir`, and proves the
/// resulting installation-wide configuration still loads before returning.
/// Every failure path leaves the installation exactly as it was found.
pub fn approve(
    config_path: &Path,
    answers_path: &Path,
    actor: &str,
) -> Result<ApprovedSnapshot, String> {
    human(actor)?;

    let answers_text =
        fs::read_to_string(answers_path).map_err(|e| format!("{}: {e}", answers_path.display()))?;
    let (worktree_key, repository_config) =
        familiar_ai_core::parse_repository_answers(&answers_text)
            .map_err(|e| format!("{}: {e}", answers_path.display()))?;

    let config_dir = config_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", config_path.display()))?;
    let base_config = Config::load(Some(config_path)).map_err(|e| e.to_string())?;

    let repositories_dir = base_config.resolve_repositories_dir(config_dir);
    let generated_path = repositories_dir.join(format!("{}.toml", slug(&worktree_key)));

    // A repository is re-onboardable only through the exact generated file
    // its own worktree key already maps to. A key declared anywhere else
    // (the main file, or a generated file this run did not create) is a
    // conflicting duplicate, refused before anything is written.
    let previous_contents = if generated_path.exists() {
        let existing_full = fs::read_to_string(&generated_path).map_err(|e| e.to_string())?;
        let existing_data = data_section(&existing_full).to_string();
        match familiar_ai_core::declared_repository_key(&existing_data) {
            Some(existing_key) if existing_key == worktree_key => {}
            Some(existing_key) => {
                return Err(format!(
                    "{} already holds a policy for {existing_key:?}, not {worktree_key:?}; refusing a filename collision",
                    generated_path.display()
                ));
            }
            None => {
                return Err(format!(
                    "{} is not a well-formed generated policy file",
                    generated_path.display()
                ));
            }
        }
        Some(existing_full)
    } else {
        None
    };
    if previous_contents.is_none() && base_config.repositories.contains_key(&worktree_key) {
        return Err(format!(
            "repository {worktree_key:?} is already declared by a configuration source other than {}; refusing to create a conflicting duplicate",
            generated_path.display()
        ));
    }

    let mut trial = base_config.clone();
    trial
        .repositories
        .insert(worktree_key.clone(), repository_config.clone());
    trial.validate().map_err(|e| e.to_string())?;

    fs::create_dir_all(&repositories_dir).map_err(|e| e.to_string())?;
    let data = familiar_ai_core::serialize_repository_policy(&worktree_key, &repository_config)?;

    let diff = match &previous_contents {
        Some(existing_full) => explain_diff(data_section(existing_full), &data),
        None => vec![format!("+ new repository policy for {worktree_key:?}")],
    };

    let content_hash = familiar_ai_review::content_hash(data.as_bytes());
    let generated_at = Utc::now().to_rfc3339();
    let header = format!(
        "# familiar-ai onboarding policy snapshot\n# repository = {worktree_key:?}\n# actor = {actor:?}\n# generated_at = {generated_at:?}\n# content_hash = {content_hash:?}\n{POLICY_SENTINEL}\n"
    );
    let full_contents = format!("{header}{data}");

    let tmp_path = generated_path.with_extension("toml.tmp");
    fs::write(&tmp_path, &full_contents).map_err(|e| e.to_string())?;
    fs::rename(&tmp_path, &generated_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        e.to_string()
    })?;

    if let Err(error) = Config::load(Some(config_path)) {
        match &previous_contents {
            Some(bytes) => {
                let _ = fs::write(&generated_path, bytes);
            }
            None => {
                let _ = fs::remove_file(&generated_path);
            }
        }
        return Err(format!(
            "generated policy failed final validation and was rolled back: {error}"
        ));
    }

    Ok(ApprovedSnapshot {
        worktree: worktree_key,
        generated_path,
        content_hash,
        actor: actor.to_string(),
        generated_at,
        diff,
    })
}

// ---------------------------------------------------------------------
// validate: prove the final merged policy loads, without claiming a PRD
// or invoking a model
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryPolicySummary {
    pub worktree: String,
    pub review_source: &'static str,
    pub execution_context_source: &'static str,
    pub delivery_mode: String,
}

fn delivery_mode_str(mode: DeliveryMode) -> &'static str {
    match mode {
        DeliveryMode::Disabled => "disabled",
        DeliveryMode::ReviewedPrManual => "reviewed_pr_manual",
        DeliveryMode::PocSelfApproval => "poc_self_approval",
        DeliveryMode::ReviewGatedAutomatic => "review_gated_automatic",
    }
}

pub fn validate(config_path: &Path) -> Result<Vec<RepositoryPolicySummary>, String> {
    let config = Config::load(Some(config_path)).map_err(|e| e.to_string())?;
    let mut summaries = Vec::new();
    for (worktree, entry) in &config.repositories {
        let canonical = Path::new(worktree)
            .canonicalize()
            .map_err(|e| format!("{worktree}: {e}"))?;
        let effective = config.effective_execution(&canonical);
        let delivery_mode = entry
            .delivery
            .as_ref()
            .map(|delivery| delivery_mode_str(delivery.mode).to_string())
            .unwrap_or_else(|| "unauthorized (no policy)".to_string());
        summaries.push(RepositoryPolicySummary {
            worktree: worktree.clone(),
            review_source: effective.review_source.as_str(),
            execution_context_source: effective.execution_context_source.as_str(),
            delivery_mode,
        });
    }
    Ok(summaries)
}

// ---------------------------------------------------------------------
// fixture: harmless proof of context, review isolation, reporting, and the
// delivery boundary for one already-approved repository
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureReport {
    pub repository_key: String,
    pub worktree: PathBuf,
    pub review_source: &'static str,
    pub execution_context_source: &'static str,
    pub context_proof: String,
    pub isolation_worktree: PathBuf,
    pub verification_status: String,
    pub delivery_summary: String,
    pub report_path: PathBuf,
}

pub fn fixture(paths: &AppPaths, worktree_root: &Path) -> Result<FixtureReport, String> {
    let identity = FilesystemBacklogDiscovery
        .resolve(worktree_root)
        .map_err(|e| e.to_string())?;
    let config =
        Config::load(Some(&paths.config_dir.join("config.toml"))).map_err(|e| e.to_string())?;
    let worktree_key = config
        .repository_key(&identity.worktree)
        .ok_or_else(|| {
            format!(
                "{} is not onboarded; run `familiar-ai onboard approve` first",
                identity.worktree.display()
            )
        })?
        .to_string();
    let repository_config = config.repository(&identity.worktree);
    let effective = config.effective_execution(&identity.worktree);

    // --- context: prove the repository's execution-context ceiling (or its
    // deliberate absence) is wired in, using a synthetic canary document so
    // nothing real is read or spent. ---
    let canary_content = "harmless onboarding fixture canary";
    let canary_tokens = familiar_ai_tokens::estimate_tokens(canary_content) as u64;
    let canary = ExecutionContext {
        repository: RepositoryContext {
            repository: identity.worktree.clone(),
            worktree: identity.worktree.clone(),
            git_commit: None,
        },
        prd: ContextDocument {
            path: "onboarding-fixture/canary.md".into(),
            kind: DocumentKind::Prd,
            content: canary_content.into(),
            inclusion: InclusionReason::RequestedPrd,
            estimated_tokens: canary_tokens,
        },
        documents: Vec::new(),
        estimated_tokens: canary_tokens,
    };
    let context_proof = match effective.execution_context.hard_ceiling_tokens {
        Some(ceiling) => {
            let budgeted = ContextBudgeter::new()
                .budget(
                    canary,
                    ContextBudget {
                        hard_ceiling_tokens: ceiling,
                    },
                )
                .map_err(|e| format!("context budget fixture failed: {e}"))?;
            format!(
                "hard_ceiling_tokens={ceiling} included_tokens={}",
                budgeted.report.included_estimated_tokens
            )
        }
        None => "no hard_ceiling_tokens configured (unbounded)".to_string(),
    };

    // --- review isolation: lease a real isolated worktree, exactly the
    // primitive production review uses, and run one harmless check inside
    // it. The fixture never writes into the repository itself. ---
    let component_id = slug(&worktree_key);
    let mut lease = WorktreeLease::create_component(
        &identity.worktree,
        &paths.state_dir,
        "onboarding-fixture",
        &component_id,
        None,
    )
    .map_err(|e| format!("cannot create isolated fixture worktree: {e}"))?;
    let isolation_worktree = lease.path().to_path_buf();
    let check = VerificationCheck {
        check_id: "onboarding-fixture".into(),
        argv: vec!["true".into()],
        working_directory: ".".into(),
        environment: BTreeMap::new(),
        timeout_ms: 5_000,
        required: true,
        path_prefixes: Vec::new(),
    };
    let artifact_dir = paths
        .state_dir
        .join("onboarding-fixtures")
        .join(&component_id);
    fs::create_dir_all(&artifact_dir).map_err(|e| e.to_string())?;
    let evidence = CommandVerificationRunner::new(artifact_dir.clone(), 4096)
        .run(&isolation_worktree, &check, "onboarding-fixture")
        .map_err(|e| format!("isolation fixture check failed: {e}"))?;
    lease.mark_retained().map_err(|e| e.to_string())?;

    // --- delivery boundary: report which mode governs this repository
    // without merging, deploying, or authorizing anything. ---
    let delivery_summary = match &repository_config.delivery {
        Some(delivery) => format!(
            "{} (automatically_authorized={})",
            delivery_mode_str(delivery.mode),
            delivery.automatically_authorized()
        ),
        None => "unauthorized (no delivery policy configured)".to_string(),
    };

    let report = FixtureReport {
        repository_key: worktree_key,
        worktree: identity.worktree.clone(),
        review_source: effective.review_source.as_str(),
        execution_context_source: effective.execution_context_source.as_str(),
        context_proof,
        isolation_worktree,
        verification_status: format!("{:?}", evidence.status),
        delivery_summary,
        report_path: artifact_dir.join("report.txt"),
    };

    // --- reporting: durable, file-based (no storage crate dependency). ---
    write_fixture_report(&report)?;

    Ok(report)
}

fn write_fixture_report(report: &FixtureReport) -> Result<(), String> {
    let text = format!(
        "repository_key={}\nworktree={}\nreview_source={}\nexecution_context_source={}\ncontext_proof={}\nisolation_worktree={}\nverification_status={}\ndelivery={}\n",
        report.repository_key,
        report.worktree.display(),
        report.review_source,
        report.execution_context_source,
        report.context_proof,
        report.isolation_worktree.display(),
        report.verification_status,
        report.delivery_summary,
    );
    fs::write(&report.report_path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
        temp
    }

    fn commit(repo: &Path) {
        fs::write(repo.join("README.md"), "hello\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "init"])
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
    }

    fn app_paths(root: &Path) -> AppPaths {
        AppPaths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            state_dir: root.join("state"),
            runtime_dir: root.join("runtime"),
            log_dir: root.join("logs"),
            socket_path: root.join("runtime/familiar-ai.sock"),
            pid_path: root.join("runtime/familiar-ai.pid"),
        }
    }

    #[test]
    fn discover_reads_only_names_never_content() {
        let repo = git_repo();
        fs::write(
            repo.path().join("Cargo.toml"),
            "malicious = \"$(rm -rf /)\"",
        )
        .unwrap();
        fs::create_dir_all(repo.path().join("src")).unwrap();
        fs::write(repo.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(repo.path().join("docs/prds")).unwrap();

        let proposal = discover(repo.path()).unwrap();
        assert_eq!(proposal.build_tools, vec!["cargo".to_string()]);
        assert_eq!(proposal.languages, vec!["Rust".to_string()]);
        assert_eq!(proposal.prd_profile.as_deref(), Some("canonical"));
        assert!(proposal
            .verification_commands
            .contains(&vec!["cargo".to_string(), "test".to_string()]));
        // The malicious Cargo.toml content never influences the proposal:
        // every proposed command comes from Familiar's fixed dictionary.
        for argv in &proposal.verification_commands {
            assert!(!argv.iter().any(|arg| arg.contains("rm -rf")));
        }
    }

    #[test]
    fn discover_recognizes_numbered_slug_layout() {
        let repo = git_repo();
        fs::create_dir_all(repo.path().join("docs/prd/todo")).unwrap();
        fs::create_dir_all(repo.path().join("docs/prd/done")).unwrap();
        fs::write(repo.path().join("go.mod"), "module example\n").unwrap();

        let proposal = discover(repo.path()).unwrap();
        assert_eq!(proposal.prd_profile.as_deref(), Some("numbered-slug"));
        assert_eq!(proposal.build_tools, vec!["go".to_string()]);
    }

    #[test]
    fn approve_refuses_without_human_actor() {
        let error = approve(
            Path::new("/nonexistent"),
            Path::new("/nonexistent"),
            "agent:x",
        )
        .unwrap_err();
        assert!(error.contains("human authority required"));
    }

    #[test]
    fn approve_writes_generated_policy_and_re_approval_shows_explainable_diff() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, "").unwrap();
        let repo = git_repo();

        let answers_path = home.path().join("answers.toml");
        fs::write(
            &answers_path,
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n",
                repo.path().display().to_string()
            ),
        )
        .unwrap();

        let snapshot = approve(&config_path, &answers_path, "human:trollboy").unwrap();
        assert!(snapshot.generated_path.is_file());
        assert_eq!(
            snapshot.diff,
            vec![format!(
                "+ new repository policy for {:?}",
                repo.path().display().to_string()
            )]
        );
        let written = fs::read_to_string(&snapshot.generated_path).unwrap();
        assert!(written.contains("content_hash"));
        assert!(written.contains(&snapshot.content_hash));

        // Re-running onboarding for the same repository with a changed
        // answer overwrites the same generated file and reports an
        // explainable diff of exactly what changed.
        fs::write(
            &answers_path,
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\nrisk_vocabulary = [\"low\"]\n",
                repo.path().display().to_string()
            ),
        )
        .unwrap();
        let second = approve(&config_path, &answers_path, "human:trollboy").unwrap();
        assert_eq!(second.generated_path, snapshot.generated_path);
        assert!(
            second
                .diff
                .iter()
                .any(|line| line.contains("risk_vocabulary")),
            "{:?}",
            second.diff
        );
        assert_ne!(second.content_hash, snapshot.content_hash);

        let config = Config::load(Some(&config_path)).unwrap();
        assert_eq!(config.repositories.len(), 1);
        let entry = config
            .repositories
            .get(&repo.path().display().to_string())
            .unwrap();
        assert_eq!(entry.risk_vocabulary, vec!["low".to_string()]);
    }

    #[test]
    fn approve_refuses_a_key_declared_outside_onboarding() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        let repo = git_repo();
        fs::write(
            &config_path,
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n",
                repo.path().display().to_string()
            ),
        )
        .unwrap();

        let answers_path = home.path().join("answers.toml");
        fs::write(
            &answers_path,
            format!(
                "[repositories.{:?}]\nprofile = \"numbered-slug\"\n",
                repo.path().display().to_string()
            ),
        )
        .unwrap();

        let error = approve(&config_path, &answers_path, "human:trollboy").unwrap_err();
        assert!(
            error.contains("already declared by a configuration source other than"),
            "{error}"
        );
    }

    #[test]
    fn approve_validates_against_configured_worker_registry_before_writing() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, "").unwrap();
        let repo = git_repo();

        let answers_path = home.path().join("answers.toml");
        fs::write(
            &answers_path,
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n[repositories.{:?}.review]\nenabled = true\nallowed_paths = [\"src/\"]\n[repositories.{:?}.review.implementation_agent]\nadapter_id = \"codex\"\nagent_id = \"missing\"\n",
                repo.path().display().to_string(),
                repo.path().display().to_string(),
                repo.path().display().to_string(),
            ),
        )
        .unwrap();

        let error = approve(&config_path, &answers_path, "human:trollboy").unwrap_err();
        assert!(!error.is_empty());
        assert!(
            !config_dir.join("repositories").exists() || {
                fs::read_dir(config_dir.join("repositories"))
                    .unwrap()
                    .next()
                    .is_none()
            }
        );
    }

    #[test]
    fn validate_reports_configuration_source_per_repository() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, "").unwrap();
        let repo = git_repo();

        let answers_path = home.path().join("answers.toml");
        fs::write(
            &answers_path,
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n",
                repo.path().display().to_string()
            ),
        )
        .unwrap();
        approve(&config_path, &answers_path, "human:trollboy").unwrap();

        let summaries = validate(&config_path).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].review_source, "global");
        assert_eq!(summaries[0].delivery_mode, "unauthorized (no policy)");
    }

    #[test]
    fn fixture_requires_prior_onboarding() {
        let home = tempfile::tempdir().unwrap();
        let paths = app_paths(home.path());
        paths.ensure_dirs().unwrap();
        fs::write(paths.config_dir.join("config.toml"), "").unwrap();
        let repo = git_repo();
        commit(repo.path());

        let error = fixture(&paths, repo.path()).unwrap_err();
        assert!(error.contains("is not onboarded"), "{error}");
    }

    #[test]
    fn fixture_proves_context_isolation_and_delivery_boundary() {
        let home = tempfile::tempdir().unwrap();
        let paths = app_paths(home.path());
        paths.ensure_dirs().unwrap();
        let config_path = paths.config_dir.join("config.toml");
        fs::write(&config_path, "").unwrap();
        let repo = git_repo();
        commit(repo.path());

        let answers_path = home.path().join("answers.toml");
        fs::write(
            &answers_path,
            format!(
                "[repositories.{:?}]\nprofile = \"canonical\"\n[repositories.{:?}.execution_context]\nhard_ceiling_tokens = 5000\n",
                repo.path().display().to_string(),
                repo.path().display().to_string(),
            ),
        )
        .unwrap();
        approve(&config_path, &answers_path, "human:trollboy").unwrap();

        let report = fixture(&paths, repo.path()).unwrap();
        assert_eq!(report.execution_context_source, "repository");
        assert!(report.context_proof.contains("hard_ceiling_tokens=5000"));
        assert!(report.isolation_worktree.is_dir());
        assert_ne!(
            report.isolation_worktree,
            repo.path().canonicalize().unwrap()
        );
        assert_eq!(report.verification_status, "Passed");
        assert_eq!(
            report.delivery_summary,
            "unauthorized (no delivery policy configured)"
        );
        assert!(report.report_path.is_file());
    }
}
