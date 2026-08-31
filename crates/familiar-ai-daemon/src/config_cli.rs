use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use familiar_ai_core::config::{
    validate_cloud_cli, validate_host, validate_ssh_host, AgentAdapterKind, AuthDescriptor,
    BillingMode, EndpointProviderKind, InferenceRuntimeKind, ProviderConfig, RegistryWorkerConfig,
    WorkerCapabilityConfig,
};
use familiar_ai_core::{
    AppPaths, BacklogDiscovery, Config, FamiliarToml, FilesystemBacklogDiscovery,
};
use familiar_ai_storage::{ConfigDecisionRepository, Database, FamiliarTomlRepository};
use ring::digest::{digest, SHA256};
use toml_edit::{value, Array, Document, Item, Table};

const BYO_AUTH_REMEDY: &str =
    "configure BYO-auth with a descriptor (`cli-login: NAME`, `env: NAME`, `credential-store: STORE/SERVICE/ACCOUNT`, `ssh-agent`, or `none`); never pass a credential";

/// Read-only boundary for platform credential stores. Implementations must
/// return credential bytes in memory and must never export them to an
/// environment variable or durable record.
pub trait CredentialStore {
    fn resolve(
        &self,
        descriptor: &familiar_ai_core::config::CredentialStoreDescriptor,
    ) -> Result<String, CredentialStoreError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStoreError {
    UnsupportedPlatform,
    Missing,
    AccessDenied,
    Unavailable,
}

pub struct SystemCredentialStore;

impl CredentialStore for SystemCredentialStore {
    fn resolve(
        &self,
        descriptor: &familiar_ai_core::config::CredentialStoreDescriptor,
    ) -> Result<String, CredentialStoreError> {
        if descriptor.store != "macos-keychain" || !cfg!(target_os = "macos") {
            return Err(CredentialStoreError::UnsupportedPlatform);
        }
        let output = Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                &descriptor.service,
                "-a",
                &descriptor.account,
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| CredentialStoreError::Unavailable)?;
        if !output.status.success() {
            return Err(match output.status.code() {
                Some(44) => CredentialStoreError::Missing,
                Some(36) | Some(51) | Some(128) => CredentialStoreError::AccessDenied,
                _ => CredentialStoreError::Unavailable,
            });
        }
        let credential =
            String::from_utf8(output.stdout).map_err(|_| CredentialStoreError::Unavailable)?;
        let credential = credential.trim_end_matches(['\r', '\n']).to_owned();
        if credential.is_empty() {
            return Err(CredentialStoreError::Missing);
        }
        Ok(credential)
    }
}

/// An intentionally non-printable in-memory credential. Debug output is
/// always redacted; callers can only borrow it while constructing a request.
pub struct ResolvedCredential(String);

impl std::fmt::Debug for ResolvedCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResolvedCredential([REDACTED])")
    }
}

impl ResolvedCredential {
    pub fn expose_for_request(&self) -> &str {
        &self.0
    }
}

pub fn resolve_auth_with_store(
    auth: &AuthDescriptor,
    store: &dyn CredentialStore,
) -> Result<Option<ResolvedCredential>, String> {
    match auth {
        AuthDescriptor::Env(name) => std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .map(ResolvedCredential)
            .map(Some)
            .ok_or_else(|| {
                format!("required environment variable {name} is missing — export `{name}`.")
            }),
        AuthDescriptor::CredentialStore(descriptor) => store
            .resolve(descriptor)
            .and_then(|value| {
                if value.is_empty() {
                    Err(CredentialStoreError::Missing)
                } else {
                    Ok(value)
                }
            })
            .map(|value| Some(ResolvedCredential(value)))
            .map_err(|condition| {
                let condition = match condition {
                    CredentialStoreError::UnsupportedPlatform => {
                        "credential store is unsupported on this platform"
                    }
                    CredentialStoreError::Missing => "entry is missing or empty",
                    CredentialStoreError::AccessDenied => "access was denied",
                    CredentialStoreError::Unavailable => "credential store is unavailable",
                };
                format!("{descriptor}: {condition}")
            }),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone)]
pub struct ConfigContext {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
}

impl ConfigContext {
    pub fn resolve() -> Result<Self, String> {
        let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
        Ok(Self {
            config_path: paths.config_dir.join("config.toml"),
            data_dir: paths.data_dir,
        })
    }
}

#[derive(Debug, Clone)]
pub enum ConfigAction {
    ProviderAdd {
        name: String,
        kind: String,
        mode: Option<String>,
        host: Option<String>,
        auth: Option<String>,
        via: Option<String>,
        recipe: Option<String>,
        actor: Option<String>,
    },
    ProviderRemove {
        name: String,
        actor: Option<String>,
    },
    ProviderVerify {
        name: String,
        actor: Option<String>,
    },
    ProviderList {
        refresh: bool,
        actor: Option<String>,
    },
    ProviderBind {
        repository: PathBuf,
        role: String,
        provider: String,
        actor: Option<String>,
    },
    ProjectApprove {
        repository: PathBuf,
        actor: String,
    },
    ProjectRevoke {
        repository: PathBuf,
        actor: String,
    },
    ShowEffective {
        repository: PathBuf,
    },
    Status {
        repository: PathBuf,
    },
    ModelEnable {
        model: String,
        capabilities: Vec<String>,
        actor: Option<String>,
    },
    ModelDisable {
        model: String,
        actor: Option<String>,
    },
    ModelList,
    MigrateAgents {
        actor: Option<String>,
    },
    History {
        limit: usize,
    },
}

pub fn execute(action: ConfigAction) -> Result<(), String> {
    execute_with_context(action, &ConfigContext::resolve()?)
}

pub fn execute_with_context(action: ConfigAction, context: &ConfigContext) -> Result<(), String> {
    match action {
        ConfigAction::ProviderAdd {
            name,
            kind,
            mode,
            host,
            auth,
            via,
            recipe,
            actor,
        } => provider_add(
            context,
            &name,
            &kind,
            mode.as_deref(),
            host.as_deref(),
            auth.as_deref(),
            via.as_deref(),
            recipe.as_deref(),
            actor.as_deref(),
        ),
        ConfigAction::ProviderRemove { name, actor } => {
            provider_remove(context, &name, actor.as_deref())
        }
        ConfigAction::ProviderVerify { name, actor } => {
            provider_verify(context, &name, actor.as_deref())
        }
        ConfigAction::ProviderList { refresh, actor } => {
            provider_list(context, refresh, actor.as_deref())
        }
        ConfigAction::ProviderBind {
            repository,
            role,
            provider,
            actor,
        } => provider_bind(context, &repository, &role, &provider, actor.as_deref()),
        ConfigAction::ProjectApprove { repository, actor } => {
            project_approve(context, &repository, &actor)
        }
        ConfigAction::ProjectRevoke { repository, actor } => {
            project_revoke(context, &repository, &actor)
        }
        ConfigAction::ShowEffective { repository } => show_effective(context, &repository),
        ConfigAction::Status { repository } => project_status(context, &repository),
        ConfigAction::ModelEnable {
            model,
            capabilities,
            actor,
        } => model_enable(context, &model, &capabilities, actor.as_deref()),
        ConfigAction::ModelDisable { model, actor } => {
            model_disable(context, &model, actor.as_deref())
        }
        ConfigAction::ModelList => model_list(context),
        ConfigAction::MigrateAgents { actor } => migrate_agents(context, actor.as_deref()),
        ConfigAction::History { limit } => history(context, limit),
    }
}

fn repository_identity(repository: &std::path::Path) -> Result<(PathBuf, String), String> {
    let canonical = repository
        .canonicalize()
        .map_err(|e| format!("cannot resolve repository {}: {e}", repository.display()))?;
    let canonical = FilesystemBacklogDiscovery
        .resolve(&canonical)
        .map(|identity| identity.worktree)
        .unwrap_or(canonical);
    Ok((
        canonical.clone(),
        canonical.to_string_lossy().replace('\\', "/"),
    ))
}

fn project_snapshot(
    repository: &std::path::Path,
) -> Result<Option<(String, FamiliarToml)>, String> {
    let path = repository.join("familiar.toml");
    match fs::read_to_string(&path) {
        Ok(content) => Ok(Some((
            content.clone(),
            FamiliarToml::parse(&content).map_err(|e| e.to_string())?,
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn project_approve(
    context: &ConfigContext,
    repository: &std::path::Path,
    supplied_actor: &str,
) -> Result<(), String> {
    let actor = actor(Some(supplied_actor))?;
    let (repository, key) = repository_identity(repository)?;
    let (content, _) = project_snapshot(&repository)?.ok_or("familiar.toml not found")?;
    let hash = sha256(content.as_bytes());
    let config = load_config(context)?;
    let db = open_database(context, &config)?;
    FamiliarTomlRepository::new(&db)
        .record(&key, "approve", &actor, &hash, &content)
        .map_err(|e| e.to_string())?;
    println!("familiar.toml: approved (snapshot {hash}, {actor})");
    Ok(())
}

fn project_revoke(
    context: &ConfigContext,
    repository: &std::path::Path,
    supplied_actor: &str,
) -> Result<(), String> {
    let actor = actor(Some(supplied_actor))?;
    let (repository, key) = repository_identity(repository)?;
    let content = project_snapshot(&repository)?
        .map(|v| v.0)
        .unwrap_or_default();
    let hash = sha256(content.as_bytes());
    let config = load_config(context)?;
    let db = open_database(context, &config)?;
    FamiliarTomlRepository::new(&db)
        .record(&key, "revoke", &actor, &hash, &content)
        .map_err(|e| e.to_string())?;
    println!("familiar.toml: revoked ({actor})");
    Ok(())
}

fn approval(
    context: &ConfigContext,
    key: &str,
) -> Result<Option<familiar_ai_storage::FamiliarTomlDecision>, String> {
    let config = load_config(context)?;
    let db = open_database(context, &config)?;
    FamiliarTomlRepository::new(&db)
        .latest(key)
        .map_err(|e| e.to_string())
}

fn diff(old: &str, new: &str) -> String {
    let old_lines = old.lines().collect::<Vec<_>>();
    let new_lines = new.lines().collect::<Vec<_>>();
    let mut out = String::from("--- approved/familiar.toml\n+++ current/familiar.toml\n");
    let length = old_lines.len().max(new_lines.len());
    for index in 0..length {
        match (old_lines.get(index), new_lines.get(index)) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => out.push_str(&format!("-{a}\n+{b}\n")),
            (Some(a), None) => out.push_str(&format!("-{a}\n")),
            (None, Some(b)) => out.push_str(&format!("+{b}\n")),
            _ => {}
        }
    }
    out
}

fn resolved_project(
    context: &ConfigContext,
    repository: &std::path::Path,
) -> Result<(Config, Option<FamiliarToml>, bool), String> {
    let (repository, key) = repository_identity(repository)?;
    let mut config = load_config(context)?;
    let Some((content, project)) = project_snapshot(&repository)? else {
        return Ok((config, None, false));
    };
    let approved = approval(context, &key)?.is_some_and(|row| {
        row.decision == "approve" && row.content_hash == sha256(content.as_bytes())
    });
    if approved {
        let machine = config.repository(&repository).map_err(|e| e.to_string())?;
        let mut effective = project.repository_config(&machine);
        if let Some(delivery) = effective.delivery.as_mut() {
            for (role, declaration) in &project.environments {
                if let Some(provider) = machine.bindings.get(&declaration.name) {
                    delivery.targets.insert(role.clone(), provider.clone());
                }
            }
        }
        config
            .preflight
            .commands
            .extend(project.verification.iter().map(|check| {
                familiar_ai_core::PreflightCommandConfig {
                    check_id: check.check_id.clone(),
                    argv: check.argv.clone(),
                    working_directory: check.working_directory.clone(),
                }
            }));
        config.repositories.insert(key, effective);
    }
    Ok((config, Some(project), approved))
}

/// Load machine configuration and apply repository declarations only when the
/// current bytes match the latest durable approval decision.
pub fn effective_config_for_repository(
    context: &ConfigContext,
    repository: &std::path::Path,
) -> Result<Config, String> {
    Ok(resolved_project(context, repository)?.0)
}

fn provider_bind(
    context: &ConfigContext,
    repository: &std::path::Path,
    role: &str,
    provider: &str,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    validate_name(role, "environment role")?;
    validate_name(provider, "provider")?;
    let actor = actor(supplied_actor)?;
    let (repository, key) = repository_identity(repository)?;
    let (_, project, approved) = resolved_project(context, &repository)?;
    let project = project.ok_or("familiar.toml not found")?;
    if !approved {
        return Err(
            "familiar.toml is not approved at its current snapshot; approve it first".into(),
        );
    }
    if !project
        .environments
        .values()
        .any(|declaration| declaration.name == role)
    {
        return Err(format!(
            "familiar.toml declares no environment named '{role}'"
        ));
    }
    let config = load_config(context)?;
    let target = config
        .providers
        .get(provider)
        .ok_or_else(|| format!("unknown provider '{provider}'"))?;
    let required = project
        .environments
        .values()
        .find(|d| d.name == role)
        .unwrap();
    let kind = match target.kind {
        EndpointProviderKind::Inference => "inference",
        EndpointProviderKind::DeployTarget => "deploy-target",
        EndpointProviderKind::Billing => "billing",
    };
    if kind != required.requires {
        return Err(format!(
            "provider '{provider}' does not satisfy requirement '{}'",
            required.requires
        ));
    }
    mutate(
        context,
        "familiar-ai config provider bind",
        &actor,
        |document| {
            let repositories = root_table(document, "repositories")?;
            if !repositories.contains_key(&key) {
                repositories.insert(&key, Item::Table(Table::new()));
            }
            let repository = repositories
                .get_mut(&key)
                .and_then(Item::as_table_mut)
                .ok_or("repository config is not a table")?;
            if !repository.contains_key("bindings") {
                repository.insert("bindings", Item::Table(Table::new()));
            }
            repository
                .get_mut("bindings")
                .and_then(Item::as_table_mut)
                .ok_or("bindings is not a table")?
                .insert(role, value(provider));
            Ok(())
        },
    )?;
    println!("bound {role} -> {provider}");
    Ok(())
}

fn project_status(context: &ConfigContext, repository: &std::path::Path) -> Result<(), String> {
    let (repository, key) = repository_identity(repository)?;
    let latest = approval(context, &key)?;
    let Some((content, project)) = project_snapshot(&repository)? else {
        match latest {
            Some(row) if row.decision == "approve" => {
                let current_hash = sha256(b"");
                println!(
                    "familiar.toml: DRIFTED — authority suspended (current {current_hash}, approved {})",
                    row.content_hash
                );
                print!("{}", diff(&row.content, ""));
            }
            Some(row) => println!(
                "familiar.toml: absent; approval revoked ({} {})",
                row.actor, row.created_at
            ),
            None => println!("familiar.toml: absent"),
        }
        return Ok(());
    };
    let current_hash = sha256(content.as_bytes());
    match &latest {
        Some(row) if row.decision == "approve" && row.content_hash == current_hash => println!(
            "familiar.toml: approved (snapshot {}, {} {})",
            row.content_hash, row.actor, row.created_at
        ),
        Some(row) if row.decision == "approve" => {
            println!("familiar.toml: DRIFTED — authority suspended (current {current_hash}, approved {})", row.content_hash);
            print!("{}", diff(&row.content, &content));
        }
        Some(row) => println!("familiar.toml: revoked ({} {})", row.actor, row.created_at),
        None => println!("familiar.toml: unapproved (snapshot {current_hash}) — zero authority"),
    }
    let config = load_config(context)?;
    let machine = config.repository(&repository).map_err(|e| e.to_string())?;
    for declaration in project.environments.values() {
        match machine.bindings.get(&declaration.name) {
            Some(provider) if config.providers.contains_key(provider) => {
                println!("binding: {} -> {} (ok)", declaration.name, provider)
            }
            _ => {
                println!(
                    "binding: {} (requires {}) -> UNBOUND",
                    declaration.name, declaration.requires
                );
                println!(
                    "  run: familiar-ai config provider bind {} <provider>",
                    declaration.name
                );
            }
        }
    }
    Ok(())
}

fn show_effective(context: &ConfigContext, repository: &std::path::Path) -> Result<(), String> {
    let (repository, _) = repository_identity(repository)?;
    let machine_config = load_config(context)?;
    let machine_has_repository = machine_config
        .repositories
        .contains_key(&repository.to_string_lossy().replace('\\', "/"));
    let (config, project, approved) = resolved_project(context, &repository)?;
    let effective = config.repository(&repository).map_err(|e| e.to_string())?;
    let source = |project_has: bool| {
        if approved && project_has {
            "familiar.toml"
        } else if machine_has_repository {
            "user repository"
        } else {
            "global default"
        }
    };
    let p = project.as_ref();
    println!(
        "profile = {:?} # source: {}",
        effective.profile,
        source(p.and_then(|v| v.profile.as_ref()).is_some())
    );
    println!(
        "active_dir = {:?} # source: {}",
        effective.active_dir,
        source(p.and_then(|v| v.active_dir.as_ref()).is_some())
    );
    println!(
        "archived_dir = {:?} # source: {}",
        effective.archived_dir,
        source(p.and_then(|v| v.archived_dir.as_ref()).is_some())
    );
    println!(
        "prd_metadata_policy = {:?} # source: {}",
        effective.prd_metadata_policy,
        source(p.and_then(|v| v.prd_metadata_policy.as_ref()).is_some())
    );
    println!(
        "reference_roots = {:?} # source: {}",
        effective.reference_roots,
        source(p.is_some_and(|v| !v.reference_roots.is_empty()))
    );
    println!(
        "risk_vocabulary = {:?} # source: {}",
        effective.risk_vocabulary,
        source(p.is_some_and(|v| !v.risk_vocabulary.is_empty()))
    );
    println!(
        "review = {:?} # source: {}",
        effective.review,
        source(p.and_then(|v| v.review.as_ref()).is_some())
    );
    println!(
        "execution_context = {:?} # source: {}",
        effective.execution_context,
        source(p.and_then(|v| v.execution_context.as_ref()).is_some())
    );
    println!(
        "bindings = {:?} # source: user repository",
        effective.bindings
    );
    println!(
        "environments = {:?} # source: {}",
        project
            .as_ref()
            .map(|value| &value.environments)
            .cloned()
            .unwrap_or_default(),
        if approved {
            "familiar.toml"
        } else {
            "inactive familiar.toml"
        }
    );
    println!(
        "verification = {:?} # source: {}",
        project
            .as_ref()
            .map(|value| &value.verification)
            .cloned()
            .unwrap_or_default(),
        if approved {
            "familiar.toml"
        } else {
            "inactive familiar.toml"
        }
    );
    Ok(())
}

fn provider_add(
    context: &ConfigContext,
    name: &str,
    kind: &str,
    mode: Option<&str>,
    host: Option<&str>,
    auth: Option<&str>,
    via: Option<&str>,
    recipe: Option<&str>,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    validate_name(name, "provider")?;
    if kind != "inference" && kind != "unsloth" && kind != "deploy-target" && kind != "billing" {
        return Err(format!("unknown provider kind '{kind}'"));
    }
    if kind == "deploy-target" {
        return deploy_target_add(context, name, host, auth, via, recipe, supplied_actor);
    }
    if kind == "billing" {
        if matches!(mode, Some("openai-organization" | "openai")) {
            return billing_provider_add(context, name, host, auth, supplied_actor);
        }
        return billing_add(
            context,
            name,
            mode.unwrap_or("anthropic-organization"),
            host,
            auth,
            supplied_actor,
        );
    }
    let host = host.unwrap_or_else(|| {
        if name == "ollama" {
            "localhost:11434"
        } else {
            "localhost:443"
        }
    });
    validate_host(host)?;
    let descriptor = parse_auth(auth.unwrap_or_else(|| default_auth(name)))?;
    let config = load_config(context)?;
    if config.providers.contains_key(name) {
        return Err(format!("provider '{name}' already exists"));
    }
    let runtime = (kind == "unsloth").then_some(InferenceRuntimeKind::Unsloth);
    let models = probe(runtime, name, host, &descriptor)
        .map_err(|error| format!("{name}: {error} — nothing added."))?;
    let verified_at = Utc::now().to_rfc3339();
    let actor = actor(supplied_actor)?;
    let audit_command = format!(
        "familiar-ai config provider add --auth {}",
        String::from(descriptor.clone())
    );
    mutate(context, &audit_command, &actor, |document| {
        let table = provider_table(document, name);
        table.decor_mut().set_prefix(format!(
            "# added by familiar-ai config provider add — {actor} {verified_at}\n"
        ));
        table["kind"] = value("inference");
        if runtime == Some(InferenceRuntimeKind::Unsloth) {
            table["runtime"] = value("unsloth");
        }
        table["host"] = value(host);
        table["auth"] = value(String::from(descriptor.clone()));
        table["models"] = array_value(&models);
        table["verified_at"] = value(&verified_at);
        Ok(())
    })?;
    println!(
        "Added {name} ({kind}) at {host} — {} models discovered.",
        models.len()
    );
    Ok(())
}

fn billing_provider_add(
    context: &ConfigContext,
    name: &str,
    host: Option<&str>,
    auth: Option<&str>,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    let host = host.unwrap_or("api.openai.com:443");
    validate_host(host)?;
    let descriptor =
        parse_auth(auth.ok_or("billing provider requires --auth env:OPENAI_ADMIN_KEY")?)?;
    let AuthDescriptor::Env(env_name) = &descriptor else {
        return Err("billing provider auth must be an env: NAME Admin credential reference".into());
    };
    if load_config(context)?.providers.contains_key(name) {
        return Err(format!("provider '{name}' already exists"));
    }
    let secret = std::env::var(env_name).map_err(|_| format!("Admin credential unavailable — export `{env_name}` and retry; a project API key is not sufficient"))?;
    let now = Utc::now();
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "cannot initialize OpenAI billing probe")?
        .get(format!("https://{host}/v1/organization/costs"))
        .query(&[
            ("start_time", now.timestamp() - 86_400),
            ("end_time", now.timestamp()),
        ])
        .bearer_auth(secret)
        .send()
        .map_err(|_| "OpenAI billing probe failed — verify network access and OPENAI_ADMIN_KEY")?;
    if !response.status().is_success() {
        return Err("OpenAI Admin authority missing or expired — create/export an organization Admin key and retry; project API keys cannot collect organization costs".into());
    }
    let body = response
        .text()
        .map_err(|_| "OpenAI billing probe returned an unreadable response")?;
    let page = crate::openai_billing::parse_cost_page(&body, None)?;
    let organizations = page
        .items
        .iter()
        .map(|row| row.organization_id.as_str())
        .collect::<BTreeSet<_>>();
    if organizations.len() != 1 {
        return Err("OpenAI billing probe did not establish one unambiguous organization identity; retry a window containing costs or use an organization-scoped Admin key".into());
    }
    let organization_id = (*organizations.iter().next().unwrap()).to_owned();
    if load_config(context)?.providers.values().any(|p| {
        p.kind == EndpointProviderKind::Billing
            && p.organization_id.as_deref() == Some(&organization_id)
            && p.project_id.is_none()
    }) {
        return Err(format!("organization '{organization_id}' already has a billing collector; duplicate scope rejected"));
    }
    let verified_at = now.to_rfc3339();
    let actor = actor(supplied_actor)?;
    mutate(
        context,
        "familiar-ai config provider add",
        &actor,
        |document| {
            let table = provider_table(document, name);
            table["kind"] = value("billing");
            table["billing_mode"] = value("open-ai-organization");
            table["host"] = value(host);
            table["auth"] = value(String::from(descriptor.clone()));
            table["organization_id"] = value(&organization_id);
            table["verified_at"] = value(&verified_at);
            Ok(())
        },
    )?;
    println!("Added {name} (billing) organization={organization_id} authority=organization-costs verified={verified_at}.");
    Ok(())
}

fn deploy_target_add(
    context: &ConfigContext,
    name: &str,
    host: Option<&str>,
    auth: Option<&str>,
    via: Option<&str>,
    recipe: Option<&str>,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    if let Some(via) = via {
        if host.is_some() || auth.is_some() {
            return Err("CLI deploy-target uses --via and --recipe, not --host or --auth".into());
        }
        validate_cloud_cli(via)?;
        let recipe = recipe.ok_or("CLI deploy-target requires --recipe")?;
        let deploy_argv = shlex::split(recipe).ok_or("--recipe contains invalid shell quoting")?;
        if deploy_argv.first().map(String::as_str) != Some(via) {
            return Err(format!("--recipe must execute through '{via}'"));
        }
        if load_config(context)?.providers.contains_key(name) {
            return Err(format!("provider '{name}' already exists"));
        }
        let auth_probe = cloud_auth_probe(via);
        let identity_probe = cloud_identity_probe(via);
        let identity =
            run_cloud_probe(via).map_err(|error| format!("{name}: {error} — nothing added."))?;
        let verified_at = Utc::now().to_rfc3339();
        let actor = actor(supplied_actor)?;
        mutate(
            context,
            "familiar-ai config provider add",
            &actor,
            |document| {
                let table = provider_table(document, name);
                table.decor_mut().set_prefix(format!(
                    "# added by familiar-ai config provider add — {actor} {verified_at}\n"
                ));
                table["kind"] = value("deploy-target");
                table["host"] = value("");
                table["via"] = value(via);
                table["auth"] = value(format!("cli-login: {via}"));
                table["capabilities"] = array_value(&["authenticated".into()]);
                table["verified_at"] = value(&verified_at);
                let mut configured = Table::new();
                configured["sync_argv"] = array_value(&deploy_argv);
                configured["restart_argv"] = array_value(&auth_probe);
                configured["smoke_argv"] = array_value(&identity_probe);
                table.insert("recipe", Item::Table(configured));
                Ok(())
            },
        )?;
        println!("Added {name} (deploy-target via {via}) — authenticated as {identity}.");
        return Ok(());
    }
    if recipe.is_some() {
        return Err("--recipe requires --via".into());
    }
    let host = host.ok_or("deploy-target requires --host")?;
    validate_ssh_host(host)?;
    let descriptor = parse_auth(auth.unwrap_or("ssh-agent"))?;
    if descriptor != AuthDescriptor::SshAgent {
        return Err("deploy-target auth must be ssh-agent; identity stays in the operator agent and ~/.ssh/config".into());
    }
    if load_config(context)?.providers.contains_key(name) {
        return Err(format!("provider '{name}' already exists"));
    }
    let capabilities =
        probe_deploy_target(host).map_err(|error| format!("{name}: {error} — nothing added."))?;
    let verified_at = Utc::now().to_rfc3339();
    let actor = actor(supplied_actor)?;
    mutate(
        context,
        "familiar-ai config provider add",
        &actor,
        |document| {
            let table = provider_table(document, name);
            table.decor_mut().set_prefix(format!(
                "# added by familiar-ai config provider add — {actor} {verified_at}\n"
            ));
            table["kind"] = value("deploy-target");
            table["host"] = value(host);
            table["auth"] = value("ssh-agent");
            table["capabilities"] = array_value(&capabilities);
            table["verified_at"] = value(&verified_at);
            let mut recipe = Table::new();
            recipe["sync_argv"] = array_value(&["git".into(), "pull".into(), "--ff-only".into()]);
            recipe["restart_argv"] = array_value(&["true".into()]);
            recipe["smoke_argv"] = array_value(&["true".into()]);
            table.insert("recipe", Item::Table(recipe));
            Ok(())
        },
    )?;
    println!(
        "Added {name} (deploy-target) — ssh ok, {}.",
        capabilities.join(", ")
    );
    Ok(())
}

fn cloud_auth_probe(via: &str) -> Vec<String> {
    match via {
        "az" => vec!["az", "account", "show", "--query", "user.name", "-o", "tsv"],
        "aws" => vec![
            "aws",
            "sts",
            "get-caller-identity",
            "--query",
            "Arn",
            "--output",
            "text",
        ],
        "gcloud" => vec![
            "gcloud",
            "auth",
            "application-default",
            "print-access-token",
        ],
        "doctl" => vec![
            "doctl",
            "account",
            "get",
            "--format",
            "Email",
            "--no-header",
        ],
        _ => unreachable!("validated cloud CLI"),
    }
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn cloud_identity_probe(via: &str) -> Vec<String> {
    if via == "gcloud" {
        ["gcloud", "config", "get-value", "account"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        cloud_auth_probe(via)
    }
}

fn cloud_remedy(via: &str) -> &'static str {
    match via {
        "az" => "run `az login`",
        "aws" => "configure an AWS profile or run `aws sso login`",
        "gcloud" => "run `gcloud auth application-default login`",
        "doctl" => "run `doctl auth init`",
        _ => "authenticate the configured cloud CLI",
    }
}

fn run_cloud_probe(via: &str) -> Result<String, String> {
    let argv = cloud_auth_probe(via);
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            format!(
                "{via} CLI is not installed ({error}); install `{via}` and then {}",
                cloud_remedy(via)
            )
        })?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "{via} CLI not authenticated — {}",
            cloud_remedy(via)
        ));
    }
    if via == "gcloud" {
        let identity = cloud_identity_probe(via);
        let output = Command::new(&identity[0])
            .args(&identity[1..])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("cannot inspect gcloud account ({error})"))?;
        let account = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Ok(if account.is_empty() {
            "configured account".into()
        } else {
            account
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn billing_add(
    context: &ConfigContext,
    name: &str,
    mode: &str,
    host: Option<&str>,
    auth: Option<&str>,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    let mode = match mode {
        "anthropic-organization" | "anthropic" => BillingMode::AnthropicOrganization,
        "openai-organization" | "openai" => BillingMode::OpenAiOrganization,
        "bedrock" | "aws-bedrock" => BillingMode::Bedrock,
        "vertex" | "agent-platform" => BillingMode::Vertex,
        "foundry" | "microsoft-foundry" => BillingMode::Foundry,
        "external-billing" => BillingMode::ExternalBilling,
        other => return Err(format!("unknown billing mode '{other}'")),
    };
    if mode != BillingMode::AnthropicOrganization {
        return Err(format!("{mode:?}: {}", crate::billing::EXTERNAL_REMEDY));
    }
    let host = host.unwrap_or("api.anthropic.com:443");
    validate_host(host)?;
    let descriptor = parse_auth(auth.ok_or(
        "billing source requires --auth env:NAME — credential values are never accepted",
    )?)?;
    if !matches!(descriptor, AuthDescriptor::Env(_)) {
        return Err(
            "billing auth must use an `env: NAME` descriptor; credential values are never accepted"
                .into(),
        );
    }
    let config = load_config(context)?;
    if config.providers.contains_key(name) {
        return Err(format!("provider '{name}' already exists"));
    }
    let candidate = ProviderConfig {
        kind: EndpointProviderKind::Billing,
        billing_mode: Some(mode),
        organization_id: None,
        organization_name: None,
        project_id: None,
        runtime: None,
        host: host.into(),
        via: None,
        auth: descriptor.clone(),
        models: Vec::new(),
        verified_at: None,
        capabilities: Vec::new(),
        recipe: None,
    };
    let identity = crate::billing::probe_organization(&candidate)
        .map_err(|e| format!("{name}: {e}. Nothing added."))?;
    let db = open_database(context, &config)?;
    let repo = familiar_ai_storage::BillingRepository::new(db.conn());
    if let Some(bound) = repo
        .source_for_organization(&identity.id)
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "organization '{}' is already bound to billing source '{bound}'. Nothing added.",
            identity.id
        ));
    }
    let verified_at = Utc::now().to_rfc3339();
    let actor = actor(supplied_actor)?;
    mutate(
        context,
        "familiar-ai config provider add",
        &actor,
        |document| {
            let table = provider_table(document, name);
            table.decor_mut().set_prefix(format!(
                "# added by familiar-ai config provider add — {actor} {verified_at}\n"
            ));
            table["kind"] = value("billing");
            table["billing_mode"] = value("anthropic-organization");
            table["host"] = value(host);
            table["auth"] = value(String::from(descriptor.clone()));
            table["organization_id"] = value(&identity.id);
            table["organization_name"] = value(&identity.name);
            table["verified_at"] = value(&verified_at);
            Ok(())
        },
    )?;
    let credential_reference = String::from(descriptor);
    repo.bind_source(&familiar_ai_storage::BillingSource {
        name,
        mode: "anthropic-organization",
        organization_id: &identity.id,
        organization_name: &identity.name,
        credential_reference: &credential_reference,
    })
    .map_err(|e| e.to_string())?;
    println!(
        "Added {name} (billing) — organization \"{}\" ({}) via /v1/organizations/me.",
        identity.name, identity.id
    );
    Ok(())
}

fn probe_deploy_target(host: &str) -> Result<Vec<String>, String> {
    let output = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host,
            "uname -s -m; command -v docker >/dev/null && echo docker; command -v systemctl >/dev/null && echo systemd"])
        .stdin(Stdio::null()).output()
        .map_err(|error| format!("ssh unreachable ({error}); start ssh-agent, add the configured identity, and verify ~/.ssh/config"))?;
    if !output.status.success() {
        return Err("ssh unreachable; start ssh-agent, add the configured identity, and verify ~/.ssh/config".into());
    }
    let mut values = String::from_utf8_lossy(&output.stdout)
        .lines()
        .flat_map(|line| line.split_whitespace())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    Ok(values)
}

fn provider_remove(
    context: &ConfigContext,
    name: &str,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    validate_name(name, "provider")?;
    let config = load_config(context)?;
    if !config.providers.contains_key(name) {
        return Err(format!("unknown provider '{name}'"));
    }
    if config.worker_registry.as_ref().is_some_and(|registry| {
        registry
            .workers
            .values()
            .any(|worker| worker.provider == name)
    }) {
        return Err(format!(
            "provider '{name}' has enabled models; disable them first"
        ));
    }
    let actor = actor(supplied_actor)?;
    mutate(
        context,
        "familiar-ai config provider remove",
        &actor,
        |document| {
            document["providers"]
                .as_table_mut()
                .and_then(|table| table.remove(name));
            Ok(())
        },
    )?;
    println!("Removed {name}.");
    Ok(())
}

fn provider_verify(
    context: &ConfigContext,
    name: &str,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    let config = load_config(context)?;
    let provider = config
        .providers
        .get(name)
        .ok_or_else(|| format!("unknown provider '{name}'"))?;
    if provider.kind == EndpointProviderKind::Billing {
        let found = crate::billing::probe_organization(provider)
            .map_err(|error| format!("{name}: {error}"))?;
        if provider.organization_id.as_deref() != Some(found.id.as_str()) {
            return Err(format!(
                "{name}: credential resolved to a different organization"
            ));
        }
        println!(
            "{name}: verified — organization \"{}\" ({}).",
            found.name, found.id
        );
        return Ok(());
    }
    if provider.kind == EndpointProviderKind::DeployTarget {
        let capabilities = if let Some(via) = &provider.via {
            run_cloud_probe(via).map_err(|error| format!("{name}: {error}"))?;
            vec!["authenticated".into()]
        } else {
            probe_deploy_target(&provider.host).map_err(|error| format!("{name}: {error}"))?
        };
        let verified_at = Utc::now().to_rfc3339();
        let actor = actor(supplied_actor)?;
        return mutate(
            context,
            "familiar-ai config provider verify",
            &actor,
            |document| {
                let table = provider_table(document, name);
                table["capabilities"] = array_value(&capabilities);
                table["verified_at"] = value(&verified_at);
                Ok(())
            },
        );
    }
    let models = probe(provider.runtime, name, &provider.host, &provider.auth)
        .map_err(|error| format!("{name}: {error}"))?;
    let verified_at = Utc::now().to_rfc3339();
    let actor = actor(supplied_actor)?;
    mutate(
        context,
        "familiar-ai config provider verify",
        &actor,
        |document| {
            let table = provider_table(document, name);
            table["models"] = array_value(&models);
            table["verified_at"] = value(&verified_at);
            stamp_value(table, "models", "provider verify", &actor, &verified_at);
            Ok(())
        },
    )?;
    println!("{name}: verified — {} models discovered.", models.len());
    Ok(())
}

fn provider_list(
    context: &ConfigContext,
    refresh: bool,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    if refresh {
        let config = load_config(context)?;
        let mut discoveries = Vec::new();
        for (name, provider) in &config.providers {
            if provider.kind == EndpointProviderKind::Billing {
                if refresh {
                    return Err("billing sources are refreshed only by explicit `billing collect`; provider list is cached".into());
                }
                continue;
            }
            if provider.kind == EndpointProviderKind::DeployTarget {
                let capabilities = if let Some(via) = &provider.via {
                    run_cloud_probe(via).map_err(|error| format!("{name}: {error}"))?;
                    vec!["authenticated".into()]
                } else {
                    probe_deploy_target(&provider.host)
                        .map_err(|error| format!("{name}: {error}"))?
                };
                discoveries.push((name.clone(), capabilities, Utc::now().to_rfc3339()));
                continue;
            }
            let models = probe(provider.runtime, name, &provider.host, &provider.auth)
                .map_err(|error| format!("{name}: {error}"))?;
            discoveries.push((name.clone(), models, Utc::now().to_rfc3339()));
        }
        if !discoveries.is_empty() {
            let actor = actor(supplied_actor)?;
            mutate(
                context,
                "familiar-ai config provider list --refresh",
                &actor,
                |document| {
                    for (name, models, verified_at) in &discoveries {
                        let table = provider_table(document, name);
                        let field =
                            if table.get("kind").and_then(Item::as_str) == Some("deploy-target") {
                                "capabilities"
                            } else {
                                "models"
                            };
                        table[field] = array_value(models);
                        table["verified_at"] = value(verified_at);
                        stamp_value(
                            table,
                            "models",
                            "provider list --refresh",
                            &actor,
                            verified_at,
                        );
                    }
                    Ok(())
                },
            )?;
        }
    }
    let config = load_config(context)?;
    let config_for_list = config.clone();
    for (name, provider) in config.providers {
        if provider.kind == EndpointProviderKind::Billing {
            let db = open_database(context, &config_for_list)?;
            let status = familiar_ai_storage::BillingRepository::new(db.conn())
                .statuses()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|s| s.source_name == name);
            println!("{name} [ billing mode={:?} organization={} authority=verified last_success={} coverage={}..{} ]",provider.billing_mode,provider.organization_name.as_deref().unwrap_or("unknown"),status.as_ref().and_then(|s|s.last_success.as_deref()).unwrap_or("never"),status.as_ref().and_then(|s|s.window_start.as_deref()).unwrap_or("none"),status.as_ref().and_then(|s|s.window_end.as_deref()).unwrap_or("none"));
            continue;
        }
        let models = provider.models.join(", ");
        let age = provider
            .verified_at
            .as_deref()
            .map(verified_age)
            .unwrap_or_else(|| "never".into());
        let hint = if age.ends_with('d') {
            " — --refresh to re-probe"
        } else {
            ""
        };
        println!("{name} [ {models} ]  (verified {age}{hint})");
    }
    Ok(())
}

fn model_enable(
    context: &ConfigContext,
    address: &str,
    capabilities: &[String],
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    let (provider_name, model) = split_address(address)?;
    let config = load_config(context)?;
    if config.agents.is_some() {
        return Err(
            "cannot enable a registry model while legacy [agents] is configured; run `familiar-ai config migrate agents --actor ACTOR`, then retry"
                .into(),
        );
    }
    let provider = config
        .providers
        .get(provider_name)
        .ok_or_else(|| format!("unknown provider '{provider_name}'"))?;
    if provider.runtime == Some(InferenceRuntimeKind::Unsloth) {
        return Err(
            "Unsloth endpoint is registered, but its local execution runtime is not available yet"
                .into(),
        );
    }
    if !provider.models.iter().any(|candidate| candidate == model) {
        return Err(format!(
            "provider '{provider_name}' has no discovered model '{model}'"
        ));
    }
    if capabilities.is_empty() {
        return Err("--capabilities requires at least one capability".into());
    }
    let parsed = capabilities
        .iter()
        .map(|value| parse_capability(value))
        .collect::<Result<Vec<_>, _>>()?;
    let adapter = match provider_name {
        "ollama" => AgentAdapterKind::Ollama,
        "claude" => AgentAdapterKind::ClaudeCode,
        _ => AgentAdapterKind::Codex,
    };
    let worker = RegistryWorkerConfig {
        adapter: Some(adapter),
        provider: provider_name.into(),
        model: model.into(),
        runtime: Some(adapter.as_str().into()),
        model_artifact: None,
        auth_profile: None,
        capability_profile: None,
        runtime_config: None,
        executable: None,
        capabilities: parsed,
        fresh_process_isolation: true,
        context_tokens: 0,
        estimated_cost_microusd: 0,
        available: true,
        effort: None,
        permission_mode: None,
        extra_args: Vec::new(),
    };
    let mut registry = config.worker_registry.clone().unwrap_or_default();
    if registry
        .workers
        .insert(address.into(), worker.clone())
        .is_some()
    {
        return Err(format!("model '{address}' is already enabled"));
    }
    // Exercise the same validation used at startup before touching the file.
    registry.validate(&Default::default())?;
    let actor = actor(supplied_actor)?;
    let now = Utc::now().to_rfc3339();
    mutate(
        context,
        "familiar-ai config model enable",
        &actor,
        |document| {
            let registry = root_table(document, "worker_registry")?;
            if !registry.contains_key("workers") {
                registry.insert("workers", Item::Table(Table::new()));
            }
            let workers = registry
                .get_mut("workers")
                .and_then(Item::as_table_mut)
                .ok_or("worker_registry.workers is not a table")?;
            let mut table = Table::new();
            table.decor_mut().set_prefix(format!(
                "# added by familiar-ai config model enable — {actor} {now}\n"
            ));
            table["adapter"] = value(match adapter {
                AgentAdapterKind::Ollama => "ollama",
                AgentAdapterKind::ClaudeCode => "claude-code",
                AgentAdapterKind::Codex => "codex",
            });
            table["provider"] = value(provider_name);
            table["model"] = value(model);
            table["capabilities"] = array_value(capabilities);
            table["fresh_process_isolation"] = value(true);
            workers.insert(address, Item::Table(table));
            Ok(())
        },
    )?;
    println!("Enabled {address}.");
    Ok(())
}

fn model_disable(
    context: &ConfigContext,
    address: &str,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    split_address(address)?;
    let config = load_config(context)?;
    if !config
        .worker_registry
        .as_ref()
        .is_some_and(|registry| registry.workers.contains_key(address))
    {
        return Err(format!("model '{address}' is not enabled"));
    }
    let actor = actor(supplied_actor)?;
    mutate(
        context,
        "familiar-ai config model disable",
        &actor,
        |document| {
            let registry = document
                .get_mut("worker_registry")
                .and_then(Item::as_table_mut)
                .ok_or("worker_registry is not a table")?;
            let workers = registry
                .get_mut("workers")
                .and_then(Item::as_table_mut)
                .ok_or("worker_registry.workers is not a table")?;
            workers.remove(address);
            if workers.is_empty() {
                document.remove("worker_registry");
            }
            Ok(())
        },
    )?;
    println!("Disabled {address}.");
    Ok(())
}

fn model_list(context: &ConfigContext) -> Result<(), String> {
    if let Some(registry) = load_config(context)?.worker_registry {
        for (address, worker) in registry.workers {
            let capabilities = worker
                .capabilities
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            println!("{address} [ {capabilities} ]");
        }
    }
    Ok(())
}

fn history(context: &ConfigContext, limit: usize) -> Result<(), String> {
    let config = load_config(context)?;
    let db = open_database(context, &config)?;
    for row in ConfigDecisionRepository::new(&db)
        .list(limit)
        .map_err(|error| error.to_string())?
    {
        println!(
            "{}  {}  {}  {} -> {}",
            row.created_at, row.actor, row.command, row.before_hash, row.after_hash
        );
    }
    Ok(())
}

fn migrate_agents(context: &ConfigContext, supplied_actor: Option<&str>) -> Result<(), String> {
    let config = load_config(context)?;
    if config.worker_registry.is_some() && config.agents.is_none() {
        println!("Legacy [agents] migration: no-op (configuration is already migrated).");
        return Ok(());
    }
    let Some(agents) = config.agents.as_ref() else {
        println!("Legacy [agents] migration: no-op (no legacy agents section is configured).");
        return Ok(());
    };
    let actor = actor(supplied_actor)?;
    let registry = familiar_ai_core::config::WorkerRegistryConfig::from_legacy_agents(agents);
    let before = fs::read_to_string(&context.config_path).map_err(|error| error.to_string())?;
    let backup = context.config_path.with_extension("toml.bak");
    let backup_temporary = context.config_path.with_extension("toml.bak.tmp");
    fs::write(&backup_temporary, before.as_bytes()).map_err(|error| error.to_string())?;
    fs::rename(&backup_temporary, &backup).map_err(|error| error.to_string())?;
    mutate(
        context,
        "familiar-ai config migrate agents",
        &actor,
        |document| {
            document.remove("agents");
            #[derive(serde::Serialize)]
            struct RegistryDocument<'a> {
                worker_registry: &'a familiar_ai_core::config::WorkerRegistryConfig,
            }
            let encoded = toml::to_string(&RegistryDocument {
                worker_registry: &registry,
            })
            .map_err(|error| format!("cannot encode worker registry: {error}"))?
            .parse::<Document>()
            .map_err(|error| format!("cannot edit encoded worker registry: {error}"))?;
            let item = encoded
                .get("worker_registry")
                .cloned()
                .ok_or("encoded worker registry is missing")?;
            document.insert("worker_registry", item);
            Ok(())
        },
    )?;
    println!(
        "Migrated legacy [agents] to [worker_registry]; backup: {}",
        backup.display()
    );
    Ok(())
}

fn mutate<F>(context: &ConfigContext, command: &str, actor: &str, edit: F) -> Result<(), String>
where
    F: FnOnce(&mut Document) -> Result<(), String>,
{
    let before = fs::read_to_string(&context.config_path).unwrap_or_default();
    let mut document = before
        .parse::<Document>()
        .map_err(|error| format!("invalid config TOML: {error}"))?;
    edit(&mut document)?;
    let after = document.to_string();
    // Fail closed on typed config validation before writing.
    let candidate: Config =
        toml::from_str(&after).map_err(|error| format!("invalid config after edit: {error}"))?;
    candidate
        .validate()
        .map_err(|error| format!("invalid config after edit: {error}"))?;
    // Establish the audit sink before committing the file mutation.
    let db = open_database(context, &candidate)?;
    if let Some(parent) = context.config_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = context.config_path.with_extension("toml.tmp");
    fs::write(&temporary, after.as_bytes()).map_err(|error| error.to_string())?;
    fs::rename(&temporary, &context.config_path).map_err(|error| error.to_string())?;
    if let Err(error) = ConfigDecisionRepository::new(&db).record(
        command,
        actor,
        &sha256(before.as_bytes()),
        &sha256(after.as_bytes()),
    ) {
        fs::write(&temporary, before.as_bytes()).map_err(|rollback_error| {
            format!(
                "configuration audit failed ({error}); rollback staging failed: {rollback_error}"
            )
        })?;
        fs::rename(&temporary, &context.config_path).map_err(|rollback_error| {
            format!("configuration audit failed ({error}); rollback failed: {rollback_error}")
        })?;
        return Err(format!(
            "configuration audit failed; mutation was rolled back: {error}"
        ));
    }
    Ok(())
}

fn load_config(context: &ConfigContext) -> Result<Config, String> {
    Config::load(Some(&context.config_path)).map_err(|error| error.to_string())
}

fn open_database(context: &ConfigContext, config: &Config) -> Result<Database, String> {
    let db = Database::open(&config.database.resolve_path(&context.data_dir))
        .map_err(|error| error.to_string())?;
    db.run_migrations().map_err(|error| error.to_string())?;
    Ok(db)
}

fn provider_table<'a>(document: &'a mut Document, name: &str) -> &'a mut Table {
    let providers = root_table(document, "providers").expect("providers table");
    if !providers.contains_key(name) {
        providers.insert(name, Item::Table(Table::new()));
    }
    providers
        .get_mut(name)
        .expect("inserted provider")
        .as_table_mut()
        .expect("inserted provider table")
}

fn root_table<'a>(document: &'a mut Document, key: &str) -> Result<&'a mut Table, String> {
    if !document.contains_key(key) {
        document.insert(key, Item::Table(Table::new()));
    }
    document
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| format!("{key} is not a table"))
}

fn array_value(values: &[String]) -> Item {
    let mut array = Array::new();
    for entry in values {
        array.push(entry.as_str());
    }
    value(array)
}

fn stamp_value(table: &mut Table, key: &str, command: &str, actor: &str, at: &str) {
    if let Some(value) = table.get_mut(key).and_then(Item::as_value_mut) {
        value.decor_mut().set_prefix(format!(
            "# added by familiar-ai config {command} — {actor} {at}\n"
        ));
    }
}

fn actor(supplied: Option<&str>) -> Result<String, String> {
    let value = supplied
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("USER")
                .ok()
                .map(|name| format!("human:{name}"))
        })
        .ok_or("cannot determine actor; pass --actor human:<identity>")?;
    let identity = value.strip_prefix("human:");
    if !matches!(identity, Some(identity) if !identity.is_empty()
        && identity.chars().all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '@'))
        && !looks_credential_like(identity))
    {
        Err("invalid actor; expected human:<identity> without credential material".into())
    } else {
        Ok(value)
    }
}

fn validate_name(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Err(format!("invalid {field} '{value}'"))
    } else {
        Ok(())
    }
}

fn parse_auth(value: &str) -> Result<AuthDescriptor, String> {
    if looks_credential_like(value) {
        return Err(format!("credential-like input rejected; {BYO_AUTH_REMEDY}"));
    }
    AuthDescriptor::try_from(value.to_owned())
        .map_err(|_| format!("invalid auth descriptor; {BYO_AUTH_REMEDY}"))
}

fn looks_credential_like(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("sk-")
        || lower.contains("token=")
        || lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.contains("secret=")
        || lower.contains("bearer ")
        || lower.contains("private key")
}

fn default_auth(name: &str) -> &'static str {
    match name {
        "ollama" => "none",
        "claude" => "cli-login: claude",
        "codex" => "cli-login: codex",
        "azure-inference" => "cli-login: az",
        _ => "none",
    }
}

fn probe(
    runtime: Option<InferenceRuntimeKind>,
    name: &str,
    host: &str,
    auth: &AuthDescriptor,
) -> Result<Vec<String>, String> {
    probe_with_store(runtime, name, host, auth, &SystemCredentialStore)
}

pub fn probe_with_store(
    runtime: Option<InferenceRuntimeKind>,
    name: &str,
    host: &str,
    auth: &AuthDescriptor,
    store: &dyn CredentialStore,
) -> Result<Vec<String>, String> {
    let credential = check_auth_with_store(auth, store)?;
    #[cfg(test)]
    match host {
        "fixture-success:1" => return Ok(vec!["llama2".into(), "qwen3".into()]),
        "fixture-fail:1" => return Err("host unreachable".into()),
        _ => {}
    }
    if runtime == Some(InferenceRuntimeKind::Unsloth) {
        probe_unsloth(host, auth, credential.as_ref())
    } else if name == "ollama" {
        probe_ollama(host)
    } else if let AuthDescriptor::CliLogin(command) = auth {
        Ok(vec![command.clone()])
    } else {
        probe_ollama(host)
    }
}

fn probe_unsloth(
    host: &str,
    auth: &AuthDescriptor,
    credential: Option<&ResolvedCredential>,
) -> Result<Vec<String>, String> {
    validate_host(host)?;
    let token = match (auth, credential) {
        (AuthDescriptor::Env(_) | AuthDescriptor::CredentialStore(_), Some(value)) => {
            value.expose_for_request()
        }
        _ => {
            return Err(
                "unsloth auth must use an `env: NAME` or credential-store descriptor".into(),
            )
        }
    };
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| format!("could not build Unsloth probe client ({error})"))?
        .get(format!("http://{host}/v1/models"))
        .bearer_auth(token)
        .send()
        .map_err(|error| format!("Unsloth endpoint unreachable ({error})"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Unsloth model discovery returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let body: serde_json::Value = response
        .json()
        .map_err(|error| format!("invalid Unsloth /v1/models response ({error})"))?;
    let mut models = body
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("invalid Unsloth /v1/models response: missing data array")?
        .iter()
        .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err("Unsloth /v1/models returned no model identifiers".into());
    }
    for model in &models {
        validate_model_name(model)?;
    }
    Ok(models)
}

pub fn check_auth(auth: &AuthDescriptor) -> Result<Option<ResolvedCredential>, String> {
    check_auth_with_store(auth, &SystemCredentialStore)
}

pub fn check_auth_with_store(
    auth: &AuthDescriptor,
    store: &dyn CredentialStore,
) -> Result<Option<ResolvedCredential>, String> {
    match auth {
        AuthDescriptor::None => Ok(None),
        AuthDescriptor::Env(_) | AuthDescriptor::CredentialStore(_) => {
            resolve_auth_with_store(auth, store)
        }
        AuthDescriptor::SshAgent => {
            match std::env::var_os("SSH_AUTH_SOCK").filter(|value| !value.is_empty()) {
                Some(_) => Ok(None),
                None => Err(
                    "SSH agent unavailable — start an SSH agent and load the required key.".into(),
                ),
            }
        }
        AuthDescriptor::CliLogin(executable) => {
            let args: &[&str] = match executable.as_str() {
                "az" => &["account", "show"],
                "claude" => &["auth", "status"],
                "codex" => &["login", "status"],
                _ => &["--version"],
            };
            let output = Command::new(executable)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output();
            match output {
                Ok(output)
                    if output.status.success()
                        && (executable != "claude" || claude_status_logged_in(&output.stdout)) =>
                {
                    Ok(None)
                }
                _ if executable == "az" => Err("az CLI not authenticated — run `az login`.".into()),
                _ => Err(format!(
                    "{executable} CLI not authenticated — authenticate with `{executable}`."
                )),
            }
        }
    }
}

fn claude_status_logged_in(output: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(output)
        .ok()
        .and_then(|value| value.get("loggedIn").and_then(|v| v.as_bool()))
        == Some(true)
}

fn probe_ollama(host: &str) -> Result<Vec<String>, String> {
    validate_host(host)?;
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| "host unreachable".to_owned())?
        .get(format!("http://{host}/api/tags"))
        .send()
        .map_err(|_| "host unreachable".to_owned())?;
    if !response.status().is_success() {
        return Err("host unreachable".into());
    }
    let value: serde_json::Value = response
        .json()
        .map_err(|_| "provider returned malformed discovery".to_owned())?;
    let mut models = value
        .get("models")
        .and_then(|value| value.as_array())
        .ok_or("provider returned malformed discovery")?
        .iter()
        .filter_map(|entry| entry.get("name").and_then(|name| name.as_str()))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    for model in &models {
        validate_model_name(model)?;
    }
    Ok(models)
}

fn split_address(value: &str) -> Result<(&str, &str), String> {
    let (provider, model) = value
        .split_once('/')
        .ok_or_else(|| format!("invalid model address '{value}'; expected provider/model"))?;
    validate_name(provider, "provider")?;
    validate_model_name(model)?;
    Ok((provider, model))
}

fn validate_model_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
    {
        Err(format!("invalid model '{value}'"))
    } else {
        Ok(())
    }
}

fn parse_capability(value: &str) -> Result<WorkerCapabilityConfig, String> {
    match value {
        "planning" => Ok(WorkerCapabilityConfig::Planning),
        "implementation" => Ok(WorkerCapabilityConfig::Implementation),
        "review" => Ok(WorkerCapabilityConfig::Review),
        "remediation" => Ok(WorkerCapabilityConfig::Remediation),
        "narrow-task" => Ok(WorkerCapabilityConfig::NarrowTask),
        _ => Err(format!("unknown capability '{value}'")),
    }
}

fn verified_age(value: &str) -> String {
    let Ok(then) = DateTime::parse_from_rfc3339(value) else {
        return "never".into();
    };
    let seconds = (Utc::now() - then.with_timezone(&Utc)).num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn context() -> (tempfile::TempDir, ConfigContext) {
        let directory = tempfile::tempdir().unwrap();
        let context = ConfigContext {
            config_path: directory.path().join("config.toml"),
            data_dir: directory.path().join("data"),
        };
        (directory, context)
    }

    #[test]
    fn failed_probe_does_not_create_config() {
        let (_directory, context) = context();
        let error = execute_with_context(
            ConfigAction::ProviderAdd {
                name: "ollama".into(),
                kind: "inference".into(),
                mode: None,
                host: Some("fixture-fail:1".into()),
                auth: None,
                via: None,
                recipe: None,
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap_err();
        assert!(error.contains("nothing added"));
        assert!(!context.config_path.exists());
    }

    #[test]
    fn unsloth_probe_uses_bearer_auth_and_discovers_openai_models() {
        const ENV_NAME: &str = "FAMILIAR_AI_TEST_UNSLOTH_KEY";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::env::set_var(ENV_NAME, "test-only-secret");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("GET /v1/models "));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-only-secret"));
            let body = r#"{"data":[{"id":"unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_XL"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let models = probe_unsloth(
            &address.to_string(),
            &AuthDescriptor::Env(ENV_NAME.into()),
            Some(&ResolvedCredential("test-only-secret".into())),
        )
        .unwrap();
        server.join().unwrap();
        std::env::remove_var(ENV_NAME);
        assert_eq!(models, ["unsloth/Qwen3.8-27B-GGUF:UD-Q4_K_XL"]);
    }

    #[test]
    fn claude_login_requires_explicit_true_json_status() {
        assert!(claude_status_logged_in(br#"{"loggedIn":true}"#));
        assert!(!claude_status_logged_in(br#"{"loggedIn":false}"#));
        assert!(!claude_status_logged_in(b"not json"));
    }

    #[test]
    fn add_preserves_comments_stamps_and_records_decision() {
        let (_directory, context) = context();
        fs::write(
            &context.config_path,
            "# precious comment\n[logging]\n# level note\nlevel = \"info\"\n",
        )
        .unwrap();
        execute_with_context(
            ConfigAction::ProviderAdd {
                name: "ollama".into(),
                kind: "inference".into(),
                mode: None,
                host: Some("fixture-success:1".into()),
                auth: None,
                via: None,
                recipe: None,
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap();
        let contents = fs::read_to_string(&context.config_path).unwrap();
        assert!(contents.contains("# precious comment"));
        assert!(contents.contains("# level note"));
        assert!(
            contents.contains("# added by familiar-ai config provider add — human:test"),
            "{contents}"
        );
        assert!(!contents.contains("credential"));
        let config = load_config(&context).unwrap();
        let db = open_database(&context, &config).unwrap();
        let rows = ConfigDecisionRepository::new(&db).list(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].actor, "human:test");
        assert_eq!(rows[0].before_hash.len(), 64);
        assert_eq!(rows[0].after_hash.len(), 64);
    }

    #[test]
    fn model_enable_and_disable_updates_addressed_registry() {
        let (_directory, context) = context();
        execute_with_context(
            ConfigAction::ProviderAdd {
                name: "ollama".into(),
                kind: "inference".into(),
                mode: None,
                host: Some("fixture-success:1".into()),
                auth: None,
                via: None,
                recipe: None,
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap();
        execute_with_context(
            ConfigAction::ModelEnable {
                model: "ollama/qwen3".into(),
                capabilities: vec!["implementation".into()],
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap();
        let config = load_config(&context).unwrap();
        assert!(config
            .worker_registry
            .unwrap()
            .workers
            .contains_key("ollama/qwen3"));
        execute_with_context(
            ConfigAction::ModelDisable {
                model: "ollama/qwen3".into(),
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap();
        let config = load_config(&context).unwrap();
        assert!(!config
            .worker_registry
            .as_ref()
            .is_some_and(|registry| registry.workers.contains_key("ollama/qwen3")));
    }

    #[test]
    fn claude_provider_enables_the_claude_code_adapter() {
        let (_directory, context) = context();
        fs::write(
            &context.config_path,
            "[providers.claude]\nkind = \"inference\"\nhost = \"localhost:443\"\nauth = \"cli-login: claude\"\nmodels = [\"claude\"]\n",
        )
        .unwrap();
        execute_with_context(
            ConfigAction::ModelEnable {
                model: "claude/claude".into(),
                capabilities: vec!["review".into()],
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap();
        let worker = &load_config(&context)
            .unwrap()
            .worker_registry
            .unwrap()
            .workers["claude/claude"];
        assert_eq!(worker.adapter, Some(AgentAdapterKind::ClaudeCode));
    }

    #[test]
    fn missing_auth_names_exact_remedy_and_secret_like_input_is_not_echoed() {
        let (_directory, context) = context();
        std::env::remove_var("ACME_TEST_KEY");
        let error = execute_with_context(
            ConfigAction::ProviderAdd {
                name: "remote".into(),
                kind: "inference".into(),
                mode: None,
                host: Some("localhost:1".into()),
                auth: Some("env: ACME_TEST_KEY".into()),
                via: None,
                recipe: None,
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap_err();
        assert!(error.contains("export `ACME_TEST_KEY`"));
        assert!(!context.config_path.exists());
        let secret = "sk-super-secret";
        let error = parse_auth(secret).unwrap_err();
        assert!(!error.contains(secret));
        assert!(error.contains("BYO-auth"));
        let error = actor(Some("human:sk-super-secret")).unwrap_err();
        assert!(!error.contains("sk-super-secret"));
    }

    #[test]
    fn list_uses_cache_unless_refresh_is_requested() {
        let (_directory, context) = context();
        execute_with_context(
            ConfigAction::ProviderAdd {
                name: "ollama".into(),
                kind: "inference".into(),
                mode: None,
                host: Some("fixture-success:1".into()),
                auth: None,
                via: None,
                recipe: None,
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap();
        let contents = fs::read_to_string(&context.config_path)
            .unwrap()
            .replace("fixture-success:1", "fixture-fail:1");
        fs::write(&context.config_path, contents).unwrap();
        execute_with_context(
            ConfigAction::ProviderList {
                refresh: false,
                actor: None,
            },
            &context,
        )
        .unwrap();
        let error = execute_with_context(
            ConfigAction::ProviderList {
                refresh: true,
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap_err();
        assert!(error.contains("host unreachable"));
    }

    #[test]
    fn project_snapshot_drift_suspends_authority_and_reapproval_restores_it() {
        let (directory, context) = context();
        let repository = directory.path().join("repo");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("familiar.toml"), "profile='numbered-slug'\n[environments.prod]\nrequires='deploy-target'\nname='production'\n").unwrap();
        project_approve(&context, &repository, "human:test").unwrap();
        let (_, _, approved) = resolved_project(&context, &repository).unwrap();
        assert!(approved);

        fs::write(repository.join("familiar.toml"), "profile='canonical'\n[environments.prod]\nrequires='deploy-target'\nname='production'\n").unwrap();
        let (_, _, approved) = resolved_project(&context, &repository).unwrap();
        assert!(!approved);
        project_approve(&context, &repository, "human:test").unwrap();
        assert!(resolved_project(&context, &repository).unwrap().2);

        let (_, key) = repository_identity(&repository).unwrap();
        let config = load_config(&context).unwrap();
        let db = open_database(&context, &config).unwrap();
        let row = FamiliarTomlRepository::new(&db)
            .latest(&key)
            .unwrap()
            .unwrap();
        assert_eq!(row.actor, "human:test");
        assert!(!row.content_hash.is_empty());
        assert!(!row.created_at.is_empty());
    }

    #[test]
    fn removing_an_approved_project_file_remains_durable_drift() {
        let (directory, context) = context();
        let repository = directory.path().join("repo");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("familiar.toml"),
            "profile='numbered-slug'\n",
        )
        .unwrap();
        project_approve(&context, &repository, "human:test").unwrap();
        fs::remove_file(repository.join("familiar.toml")).unwrap();

        project_status(&context, &repository).unwrap();
        let (_, key) = repository_identity(&repository).unwrap();
        let row = approval(&context, &key).unwrap().unwrap();
        assert_eq!(row.decision, "approve");
        assert!(!row.content.is_empty());
        assert!(!resolved_project(&context, &repository).unwrap().2);
    }

    #[test]
    fn unapproved_project_cannot_be_bound() {
        let (directory, context) = context();
        let repository = directory.path().join("repo");
        fs::create_dir(&repository).unwrap();
        fs::write(
            repository.join("familiar.toml"),
            "[environments.prod]\nrequires='deploy-target'\nname='production'\n",
        )
        .unwrap();
        let error = provider_bind(
            &context,
            &repository,
            "production",
            "missing",
            Some("human:test"),
        )
        .unwrap_err();
        assert!(error.contains("not approved"));
    }

    #[test]
    fn approved_role_binds_only_in_machine_config() {
        let (directory, context) = context();
        let repository = directory.path().join("repo");
        fs::create_dir(&repository).unwrap();
        let shared = "[environments.staging]\nrequires='deploy-target'\nname='devbox'\n";
        fs::write(repository.join("familiar.toml"), shared).unwrap();
        fs::write(
            &context.config_path,
            "[providers.box]\nkind='deploy-target'\nhost='gpu-box'\nauth='ssh-agent'\n[providers.box.recipe]\nsync_argv=['true']\nrestart_argv=['true']\nsmoke_argv=['true']\n",
        )
        .unwrap();
        project_approve(&context, &repository, "human:test").unwrap();
        provider_bind(&context, &repository, "devbox", "box", Some("human:test")).unwrap();
        let machine = fs::read_to_string(&context.config_path).unwrap();
        assert!(machine.contains("devbox = \"box\""));
        assert_eq!(
            fs::read_to_string(repository.join("familiar.toml")).unwrap(),
            shared
        );
    }
}
