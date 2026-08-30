use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::{DateTime, Utc};
use familiar_ai_core::config::{
    validate_host, AgentAdapterKind, AuthDescriptor, RegistryWorkerConfig, WorkerCapabilityConfig,
};
use familiar_ai_core::{AppPaths, Config};
use familiar_ai_storage::{ConfigDecisionRepository, Database};
use ring::digest::{digest, SHA256};
use toml_edit::{value, Array, Document, Item, Table};

const BYO_AUTH_REMEDY: &str =
    "configure BYO-auth with a descriptor (`cli-login: NAME`, `env: NAME`, `ssh-agent`, or `none`); never pass a credential";

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
        host: Option<String>,
        auth: Option<String>,
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
            host,
            auth,
            actor,
        } => provider_add(
            context,
            &name,
            &kind,
            host.as_deref(),
            auth.as_deref(),
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
        ConfigAction::ModelEnable {
            model,
            capabilities,
            actor,
        } => model_enable(context, &model, &capabilities, actor.as_deref()),
        ConfigAction::ModelDisable { model, actor } => {
            model_disable(context, &model, actor.as_deref())
        }
        ConfigAction::ModelList => model_list(context),
        ConfigAction::History { limit } => history(context, limit),
    }
}

fn provider_add(
    context: &ConfigContext,
    name: &str,
    kind: &str,
    host: Option<&str>,
    auth: Option<&str>,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    validate_name(name, "provider")?;
    if kind != "inference" {
        return Err(format!("unknown provider kind '{kind}'"));
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
    let models = probe(name, host, &descriptor)
        .map_err(|error| format!("{name}: {error} — nothing added."))?;
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
            table["kind"] = value("inference");
            table["host"] = value(host);
            table["auth"] = value(String::from(descriptor.clone()));
            table["models"] = array_value(&models);
            table["verified_at"] = value(&verified_at);
            Ok(())
        },
    )?;
    println!(
        "Added {name} (inference) at {host} — {} models discovered.",
        models.len()
    );
    Ok(())
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
    let models =
        probe(name, &provider.host, &provider.auth).map_err(|error| format!("{name}: {error}"))?;
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
            let models = probe(name, &provider.host, &provider.auth)
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
                        table["models"] = array_value(models);
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
    for (name, provider) in config.providers {
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
    let provider = config
        .providers
        .get(provider_name)
        .ok_or_else(|| format!("unknown provider '{provider_name}'"))?;
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
    let adapter = if provider_name == "ollama" {
        AgentAdapterKind::Ollama
    } else {
        AgentAdapterKind::Codex
    };
    let worker = RegistryWorkerConfig {
        adapter,
        provider: provider_name.into(),
        model: model.into(),
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
            table["adapter"] = value(if adapter == AgentAdapterKind::Ollama {
                "ollama"
            } else {
                "codex"
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
                .map(|value| format!("{value:?}").to_lowercase())
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
    for (name, provider) in &candidate.providers {
        provider.validate(name)?;
    }
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

fn probe(name: &str, host: &str, auth: &AuthDescriptor) -> Result<Vec<String>, String> {
    check_auth(auth)?;
    #[cfg(test)]
    match host {
        "fixture-success:1" => return Ok(vec!["llama2".into(), "qwen3".into()]),
        "fixture-fail:1" => return Err("host unreachable".into()),
        _ => {}
    }
    if name == "ollama" {
        probe_ollama(host)
    } else if let AuthDescriptor::CliLogin(command) = auth {
        Ok(vec![command.clone()])
    } else {
        probe_ollama(host)
    }
}

fn check_auth(auth: &AuthDescriptor) -> Result<(), String> {
    match auth {
        AuthDescriptor::None => Ok(()),
        AuthDescriptor::Env(name) => match std::env::var_os(name).filter(|value| !value.is_empty())
        {
            Some(_) => Ok(()),
            None => Err(format!(
                "required environment variable {name} is missing — export `{name}`."
            )),
        },
        AuthDescriptor::SshAgent => {
            match std::env::var_os("SSH_AUTH_SOCK").filter(|value| !value.is_empty()) {
                Some(_) => Ok(()),
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
            let status = Command::new(executable)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match status {
                Ok(status) if status.success() => Ok(()),
                _ if executable == "az" => Err("az CLI not authenticated — run `az login`.".into()),
                _ => Err(format!(
                    "{executable} CLI not authenticated — authenticate with `{executable}`."
                )),
            }
        }
    }
}

fn probe_ollama(host: &str) -> Result<Vec<String>, String> {
    validate_host(host)?;
    let address = host
        .to_socket_addrs()
        .map_err(|_| "host unreachable".to_owned())?
        .next()
        .ok_or("host unreachable")?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|_| "host unreachable".to_owned())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|_| "host unreachable".to_owned())?;
    write!(
        stream,
        "GET /api/tags HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|_| "host unreachable".to_owned())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|_| "host unreachable".to_owned())?;
    let text =
        String::from_utf8(response).map_err(|_| "provider returned invalid UTF-8".to_owned())?;
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .ok_or("provider returned malformed HTTP")?;
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.contains(" 200 "))
    {
        return Err("host unreachable".into());
    }
    let value: serde_json::Value = serde_json::from_str(body)
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
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
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
                host: Some("fixture-fail:1".into()),
                auth: None,
                actor: Some("human:test".into()),
            },
            &context,
        )
        .unwrap_err();
        assert!(error.contains("nothing added"));
        assert!(!context.config_path.exists());
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
                host: Some("fixture-success:1".into()),
                auth: None,
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
                host: Some("fixture-success:1".into()),
                auth: None,
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
    fn missing_auth_names_exact_remedy_and_secret_like_input_is_not_echoed() {
        let (_directory, context) = context();
        std::env::remove_var("ACME_TEST_KEY");
        let error = execute_with_context(
            ConfigAction::ProviderAdd {
                name: "remote".into(),
                kind: "inference".into(),
                host: Some("localhost:1".into()),
                auth: Some("env: ACME_TEST_KEY".into()),
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
                host: Some("fixture-success:1".into()),
                auth: None,
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
}
