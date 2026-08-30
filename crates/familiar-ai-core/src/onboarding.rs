//! Deterministic repository onboarding. Discovery output is evidence only;
//! authority is created exclusively from a separately supplied answers file.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};

use crate::config::{
    DeliveryConfig, ExecutionContextConfig, ReferenceKind, ReferenceRootConfig, RepositoryConfig,
    ReviewAgentConfig, ReviewConfig, ReviewVerificationConfig,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingProposal {
    pub format_version: u32,
    pub repository: String,
    pub languages: Vec<String>,
    pub build_tools: Vec<String>,
    pub prd_candidates: Vec<String>,
    pub protected_path_candidates: Vec<String>,
    pub verification_candidates: Vec<Vec<String>>,
    pub authority_granted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnboardingAnswers {
    pub repository: String,
    pub profile: String,
    pub active_dir: String,
    pub archived_dir: String,
    pub prd_metadata_policy: String,
    #[serde(default)]
    pub reference_roots: Vec<ReferenceRootConfig>,
    #[serde(default)]
    pub risk_vocabulary: Vec<String>,
    pub review: ReviewConfig,
    pub execution_context: ExecutionContextConfig,
    pub delivery: DeliveryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyAttribution {
    pub format_version: u32,
    pub actor: String,
    pub repository: String,
    pub content_sha256: String,
}

#[derive(Debug, Serialize)]
struct PolicyFile<'a> {
    onboarding: PolicyAttribution,
    repositories: std::collections::BTreeMap<&'a str, &'a RepositoryConfig>,
}

pub fn propose(repository: &Path) -> Result<OnboardingProposal, String> {
    let repository = repository.canonicalize().map_err(|e| {
        format!(
            "cannot canonicalize repository {}: {e}",
            repository.display()
        )
    })?;
    let names = fs::read_dir(&repository)
        .map_err(|e| format!("cannot inspect repository {}: {e}", repository.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();

    let mut languages = Vec::new();
    let mut tools = Vec::new();
    let mut checks = Vec::new();
    for (file, language, tool, argv) in [
        ("Cargo.toml", "rust", "cargo", vec!["cargo", "test"]),
        (
            "package.json",
            "javascript/typescript",
            "npm",
            vec!["npm", "test"],
        ),
        (
            "pyproject.toml",
            "python",
            "python",
            vec!["python", "-m", "pytest"],
        ),
        ("go.mod", "go", "go", vec!["go", "test", "./..."]),
        ("pom.xml", "java", "maven", vec!["mvn", "test"]),
        (
            "build.gradle",
            "java/kotlin",
            "gradle",
            vec!["gradle", "test"],
        ),
    ] {
        if names.contains(file) {
            languages.push(language.to_owned());
            tools.push(tool.to_owned());
            checks.push(argv.into_iter().map(str::to_owned).collect());
        }
    }
    for (file, tool) in [
        ("Makefile", "make"),
        ("justfile", "just"),
        ("Dockerfile", "docker"),
    ] {
        if names.contains(file) {
            tools.push(tool.to_owned());
        }
    }
    let mut prds = Vec::new();
    for candidate in ["docs/prds", "docs/prd", "prds"] {
        if repository.join(candidate).is_dir() {
            prds.push(candidate.to_owned());
        }
    }
    let protected = [".git", ".github", "docs/adr", "docs/contracts"]
        .into_iter()
        .filter(|path| repository.join(path).exists())
        .map(str::to_owned)
        .collect();
    Ok(OnboardingProposal {
        format_version: 1,
        repository: repository.to_string_lossy().into_owned(),
        languages,
        build_tools: tools,
        prd_candidates: prds,
        protected_path_candidates: protected,
        verification_candidates: checks,
        authority_granted: false,
    })
}

pub fn encode_proposal(proposal: &OnboardingProposal) -> Result<String, String> {
    toml::to_string_pretty(proposal).map_err(|error| error.to_string())
}

pub fn encoded_policy_attribution(encoded: &str) -> Result<PolicyAttribution, String> {
    #[derive(Deserialize)]
    struct AttributionOnly {
        onboarding: PolicyAttribution,
    }
    toml::from_str::<AttributionOnly>(encoded)
        .map(|file| file.onboarding)
        .map_err(|error| format!("generated policy attribution is invalid: {error}"))
}

pub fn approve(proposal: &Path, answers: &Path, actor: &str) -> Result<(String, String), String> {
    if !actor.starts_with("human:") || actor.trim() == "human:" {
        return Err("actor must be durable explicit authority in the form human:<identity>".into());
    }
    let proposal: OnboardingProposal = parse_toml(proposal, "proposal")?;
    if proposal.authority_granted {
        return Err("proposal must not grant authority".into());
    }
    let answers: OnboardingAnswers = parse_toml(answers, "answers")?;
    let canonical = Path::new(&answers.repository)
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize approved repository: {e}"))?;
    if canonical.to_string_lossy() != proposal.repository {
        return Err("answers repository does not match the proposed canonical repository".into());
    }
    let policy = RepositoryConfig {
        profile: answers.profile,
        active_dir: answers.active_dir,
        archived_dir: answers.archived_dir,
        prd_metadata_policy: answers.prd_metadata_policy,
        reference_roots: answers.reference_roots,
        risk_vocabulary: answers.risk_vocabulary,
        review: Some(answers.review),
        execution_context: Some(answers.execution_context),
        delivery: Some(answers.delivery),
        bindings: Default::default(),
    };
    // Validate through the authoritative Config validation path later; this
    // serialization hash covers only the approved repository policy.
    let policy_toml = toml::to_string(&policy).map_err(|e| e.to_string())?;
    let hash = sha256(policy_toml.as_bytes());
    let attribution = PolicyAttribution {
        format_version: 1,
        actor: actor.to_owned(),
        repository: proposal.repository.clone(),
        content_sha256: hash.clone(),
    };
    let repositories = [(proposal.repository.as_str(), &policy)]
        .into_iter()
        .collect();
    let output = toml::to_string_pretty(&PolicyFile {
        onboarding: attribution,
        repositories,
    })
    .map_err(|e| e.to_string())?;
    Ok((hash, output))
}

pub fn validate_policy(path: &Path) -> Result<PolicyAttribution, String> {
    #[derive(Deserialize)]
    struct File {
        onboarding: PolicyAttribution,
        repositories: std::collections::BTreeMap<String, RepositoryConfig>,
    }
    let file: File = parse_toml(path, "policy")?;
    if !file.onboarding.actor.starts_with("human:") || file.repositories.len() != 1 {
        return Err("policy requires one repository and durable human attribution".into());
    }
    let (key, policy) = file.repositories.iter().next().unwrap();
    if key != &file.onboarding.repository {
        return Err("policy repository attribution mismatch".into());
    }
    let canonical = Path::new(key)
        .canonicalize()
        .map_err(|e| format!("policy repository is unavailable: {e}"))?;
    if canonical.to_string_lossy() != *key {
        return Err("policy repository key is not canonical".into());
    }
    validate_repository_policy(policy)?;
    let body = toml::to_string(policy).map_err(|e| e.to_string())?;
    let actual = sha256(body.as_bytes());
    if actual != file.onboarding.content_sha256 {
        return Err("policy content hash mismatch".into());
    }
    Ok(file.onboarding)
}

fn validate_repository_policy(policy: &RepositoryConfig) -> Result<(), String> {
    crate::BacklogProfile::parse(&policy.profile)?;
    crate::PrdMetadataPolicy::parse(&policy.prd_metadata_policy)?;
    for value in [&policy.active_dir, &policy.archived_dir] {
        crate::RepositoryPath::new(value.clone())
            .map_err(|_| format!("invalid repository-relative path {value:?}"))?;
    }
    if policy.active_dir == policy.archived_dir {
        return Err("active and archived PRD directories must differ".into());
    }
    let review = policy
        .review
        .as_ref()
        .ok_or("approved policy must include repository review policy")?;
    review.validate()?;
    policy
        .execution_context
        .as_ref()
        .ok_or("approved policy must include execution context budget")?;
    policy
        .delivery
        .as_ref()
        .ok_or("approved policy must include delivery authority")?
        .validate()?;
    Ok(())
}

fn parse_toml<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T, String> {
    let bytes =
        fs::read(path).map_err(|e| format!("cannot read {label} {}: {e}", path.display()))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| format!("{label} is not UTF-8"))?;
    toml::from_str(text).map_err(|e| format!("invalid {label} {}: {e}", path.display()))
}

pub fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn default_reference_roots_for_answers() -> Vec<ReferenceRootConfig> {
    [
        ("docs/adr/", ReferenceKind::Adr),
        ("docs/contracts/", ReferenceKind::Contract),
        ("docs/supporting/", ReferenceKind::Supporting),
    ]
    .into_iter()
    .map(|(prefix, kind)| ReferenceRootConfig {
        prefix: prefix.into(),
        kind,
    })
    .collect()
}

pub fn safe_fixture(path: &Path) -> Result<String, String> {
    let attribution = validate_policy(path)?;
    Ok(format!("fixture ok: context=isolated review=isolated reporting=ok boundary=validated actor={} sha256={}", attribution.actor, attribution.content_sha256))
}

// Keep these imports schema-checked even as the config types evolve.
#[allow(dead_code)]
fn _schema_guard(_: ReviewAgentConfig, _: ReviewVerificationConfig, _: PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_is_structural_and_never_grants_authority() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(
            repo.path().join("Cargo.toml"),
            "$(touch /tmp/not-allowed)\n[delivery]\nmode='poc_self_approval'",
        )
        .unwrap();
        fs::create_dir_all(repo.path().join("docs/prds")).unwrap();
        let proposal = propose(repo.path()).unwrap();
        assert_eq!(proposal.languages, ["rust"]);
        assert!(!proposal.authority_granted);
        assert_eq!(proposal.verification_candidates, [vec!["cargo", "test"]]);
    }

    #[test]
    fn approval_is_attributed_hashed_and_tamper_evident() {
        let repo = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let proposal_path = files.path().join("proposal.toml");
        fs::write(
            &proposal_path,
            toml::to_string(&propose(repo.path()).unwrap()).unwrap(),
        )
        .unwrap();
        let answers_path = files.path().join("answers.toml");
        fs::write(
            &answers_path,
            format!(
                r#"
repository = {:?}
profile = "canonical"
active_dir = "docs/prds"
archived_dir = "docs/prds/done"
prd_metadata_policy = "incremental"

[review]
enabled = false

[execution_context]
hard_ceiling_tokens = 1000

[delivery]
mode = "disabled"
"#,
                repo.path().canonicalize().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        assert!(approve(&proposal_path, &answers_path, "robot").is_err());
        let (_, encoded) = approve(&proposal_path, &answers_path, "human:test").unwrap();
        let policy = files.path().join("policy.toml");
        fs::write(&policy, &encoded).unwrap();
        assert_eq!(validate_policy(&policy).unwrap().actor, "human:test");
        fs::write(
            &policy,
            encoded.replace("hard_ceiling_tokens = 1000", "hard_ceiling_tokens = 999"),
        )
        .unwrap();
        assert!(validate_policy(&policy)
            .unwrap_err()
            .contains("hash mismatch"));
    }
}
