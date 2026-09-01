//! `familiar-ai onboard` — discover and approve repository-owned policy
//! without claiming work.

use std::path::PathBuf;

use clap::Subcommand;
use familiar_ai_core::onboarding;
use familiar_ai_core::{AppPaths, Config};

#[derive(Debug, Subcommand)]
pub enum OnboardCommand {
    /// Write untrusted discovery proposals. Grants no authority.
    Propose {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long, default_value = "onboarding-proposal.toml")]
        output: PathBuf,
    },
    /// Convert an explicit deterministic answers file into attributed policy.
    Approve {
        proposal: PathBuf,
        #[arg(long)]
        answers: PathBuf,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        repositories_dir: Option<PathBuf>,
    },
    /// Validate one policy snapshot without storage, a PRD claim, or a model.
    Validate { policy: PathBuf },
    /// Run the harmless onboarding boundary fixture.
    Fixture { policy: PathBuf },
}

pub fn onboard(command: OnboardCommand) -> Result<(), String> {
    match command {
        OnboardCommand::Propose { repository, output } => {
            let proposal = onboarding::propose(&repository)?;
            let encoded = onboarding::encode_proposal(&proposal)?;
            std::fs::write(&output, encoded)
                .map_err(|e| format!("cannot write proposal {}: {e}", output.display()))?;
            println!("proposal={} authority_granted=false", output.display());
        }
        OnboardCommand::Approve {
            proposal,
            answers,
            actor,
            repositories_dir,
        } => {
            let paths = AppPaths::resolve().map_err(|e| e.to_string())?;
            let main = paths.config_dir.join("config.toml");
            let config = Config::load(Some(&main)).map_err(|e| e.to_string())?;
            let directory = repositories_dir.unwrap_or_else(|| {
                let configured = config
                    .repositories_dir
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("repositories"));
                if configured.is_absolute() {
                    configured
                } else {
                    paths.config_dir.join(configured)
                }
            });
            let (hash, encoded) = onboarding::approve(&proposal, &answers, &actor)?;
            std::fs::create_dir_all(&directory)
                .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;
            let attribution = onboarding::encoded_policy_attribution(&encoded)?;
            let repository = &attribution.repository;
            let name = format!("{}.toml", onboarding::sha256(repository.as_bytes()));
            let target = directory.join(name);
            let diff = match std::fs::read_to_string(&target) {
                Ok(current) if current == encoded => "unchanged",
                Ok(current) => {
                    let prior_hash = onboarding::encoded_policy_attribution(&current)
                        .ok()
                        .map(|value| value.content_sha256);
                    if prior_hash.as_deref() == Some(hash.as_str()) {
                        "attribution-changed"
                    } else {
                        "policy-content-changed"
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => "new",
                Err(error) => return Err(format!("cannot inspect {}: {error}", target.display())),
            };
            let pending = target.with_extension("toml.pending");
            std::fs::write(&pending, encoded)
                .map_err(|e| format!("cannot write {}: {e}", pending.display()))?;
            if let Err(error) = onboarding::validate_policy(&pending) {
                let _ = std::fs::remove_file(&pending);
                return Err(error);
            }
            std::fs::rename(&pending, &target)
                .map_err(|e| format!("cannot install {}: {e}", target.display()))?;
            println!(
                "policy={} diff={} actor={} sha256={}",
                target.display(),
                diff,
                actor,
                hash
            );
        }
        OnboardCommand::Validate { policy } => {
            let attribution = onboarding::validate_policy(&policy)?;
            println!(
                "valid actor={} sha256={}",
                attribution.actor, attribution.content_sha256
            );
        }
        OnboardCommand::Fixture { policy } => println!("{}", onboarding::safe_fixture(&policy)?),
    }
    Ok(())
}
