use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Migrate legacy configuration sections to supported replacements.
    Migrate {
        #[command(subcommand)]
        command: ConfigMigrateCommand,
    },
    /// Show durable configuration mutation decisions.
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Approve or revoke the exact current familiar.toml snapshot.
    Project {
        #[command(subcommand)]
        command: ProjectConfigCommand,
    },
    /// Show the approval-aware three-layer configuration with provenance.
    Show {
        #[arg(long)]
        effective: bool,
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// Probe and register externally prepared identity-bearing files.
    Register {
        alias: String,
        root: PathBuf,
        #[arg(long = "file", required = true)]
        files: Vec<PathBuf>,
        /// JSON object of identity-bearing configuration.
        #[arg(long, default_value = "{}")]
        identity: String,
        /// JSON provenance record; omitted fields remain explicitly unknown.
        #[arg(long, default_value = "{}")]
        provenance: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long = "adapter")]
        adapters: Vec<String>,
        #[arg(long)]
        merged: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Record a legacy/runtime-only alias as degraded and unverified.
    RegisterAlias {
        alias: String,
        runtime_alias: String,
        #[arg(long)]
        actor: Option<String>,
    },
    List,
    Show {
        alias: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigMigrateCommand {
    /// Losslessly migrate [agents] to the worker registry.
    Agents {
        #[arg(long)]
        actor: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectConfigCommand {
    Approve {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        actor: String,
    },
    Revoke {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        actor: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    Add {
        name: String,
        #[arg(long, default_value = "inference")]
        kind: String,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        auth: Option<String>,
        #[arg(long)]
        via: Option<String>,
        #[arg(long)]
        recipe: Option<String>,
        #[arg(long)]
        actor: Option<String>,
    },
    Remove {
        name: String,
        #[arg(long)]
        actor: Option<String>,
    },
    Verify {
        name: String,
        #[arg(long)]
        actor: Option<String>,
    },
    List {
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Bind a declared environment name to a machine-local provider.
    Bind {
        role: String,
        provider: String,
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        actor: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ModelCommand {
    Enable {
        model: String,
        #[arg(long, value_delimiter = ',', num_args = 1..)]
        capabilities: Vec<String>,
        #[arg(long)]
        actor: Option<String>,
    },
    Disable {
        model: String,
        #[arg(long)]
        actor: Option<String>,
    },
    List,
}

pub fn config_command(command: ConfigCommand) -> Result<(), String> {
    use crate::config_cli::{execute, ConfigAction};
    let action = match command {
        ConfigCommand::Provider { command } => match command {
            ProviderCommand::Add {
                name,
                kind,
                mode,
                host,
                auth,
                via,
                recipe,
                actor,
            } => ConfigAction::ProviderAdd {
                name,
                kind,
                mode,
                host,
                auth,
                via,
                recipe,
                actor,
            },
            ProviderCommand::Remove { name, actor } => ConfigAction::ProviderRemove { name, actor },
            ProviderCommand::Verify { name, actor } => ConfigAction::ProviderVerify { name, actor },
            ProviderCommand::List { refresh, actor } => {
                ConfigAction::ProviderList { refresh, actor }
            }
            ProviderCommand::Bind {
                role,
                provider,
                repository,
                actor,
            } => ConfigAction::ProviderBind {
                repository,
                role,
                provider,
                actor,
            },
        },
        ConfigCommand::Model { command } => match command {
            ModelCommand::Enable {
                model,
                capabilities,
                actor,
            } => ConfigAction::ModelEnable {
                model,
                capabilities,
                actor,
            },
            ModelCommand::Disable { model, actor } => ConfigAction::ModelDisable { model, actor },
            ModelCommand::List => ConfigAction::ModelList,
        },
        ConfigCommand::Artifact { command } => match command {
            ArtifactCommand::Register {
                alias,
                root,
                files,
                identity,
                provenance,
                base,
                adapters,
                merged,
                actor,
            } => ConfigAction::ArtifactRegister {
                alias,
                root,
                files,
                identity,
                provenance,
                base,
                adapters,
                merged,
                actor,
            },
            ArtifactCommand::RegisterAlias {
                alias,
                runtime_alias,
                actor,
            } => ConfigAction::ArtifactRegisterAlias {
                alias,
                runtime_alias,
                actor,
            },
            ArtifactCommand::List => ConfigAction::ArtifactList,
            ArtifactCommand::Show { alias } => ConfigAction::ArtifactShow { alias },
        },
        ConfigCommand::Migrate { command } => match command {
            ConfigMigrateCommand::Agents { actor } => ConfigAction::MigrateAgents { actor },
        },
        ConfigCommand::History { limit } => ConfigAction::History { limit },
        ConfigCommand::Project { command } => match command {
            ProjectConfigCommand::Approve { repository, actor } => {
                ConfigAction::ProjectApprove { repository, actor }
            }
            ProjectConfigCommand::Revoke { repository, actor } => {
                ConfigAction::ProjectRevoke { repository, actor }
            }
        },
        ConfigCommand::Show {
            effective,
            repository,
        } => {
            if !effective {
                return Err("config show currently requires --effective".into());
            }
            ConfigAction::ShowEffective { repository }
        }
    };
    execute(action)
}

/// The top-level `familiar-ai status` subcommand delegates entirely to the
/// same `config_cli` action used by `config show --effective`'s sibling
/// status projection.
pub fn status_command(repository: PathBuf) -> Result<(), String> {
    crate::config_cli::execute(crate::config_cli::ConfigAction::Status { repository })
}
