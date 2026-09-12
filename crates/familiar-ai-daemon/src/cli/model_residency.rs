//! PRD-073 `familiar-ai model-residency` command bodies.
//!
//! Enabling residency for a model artifact is an explicit, audited
//! configuration mutation — the same probe-before-persist, comment-
//! preserving, decision-row-recording boundary PRD-047 established for
//! `config model enable`. Nothing here starts or stops a process: the
//! daemon owns residents, so the CLI records the operator's decision and
//! the daemon's next health interval reconciles the running set to it,
//! stopping and recording any resident the new configuration no longer
//! enables.

use familiar_ai_core::config::{ModelResidencyConfig, ResidentModelConfig};
use familiar_ai_storage::repos::model_residency::ModelResidencyRepository;
use toml_edit::{value, Array, Document, Item, Table};

use crate::config_cli::{actor, load_config, mutate, open_database, root_table, ConfigContext};

/// What the operator asked of residency.
#[derive(Debug, Clone)]
pub enum ResidencyAction {
    Enable {
        key: String,
        worker: String,
        launch: Vec<String>,
        memory_mb: Option<u64>,
        ready_timeout_secs: u64,
        actor: Option<String>,
    },
    Disable {
        key: String,
        actor: Option<String>,
    },
    Status {
        limit: usize,
    },
}

pub fn execute(action: ResidencyAction) -> Result<(), String> {
    let context = ConfigContext::resolve()?;
    execute_with_context(action, &context)
}

pub fn execute_with_context(
    action: ResidencyAction,
    context: &ConfigContext,
) -> Result<(), String> {
    match action {
        ResidencyAction::Enable {
            key,
            worker,
            launch,
            memory_mb,
            ready_timeout_secs,
            actor: supplied,
        } => enable(
            context,
            &key,
            &worker,
            &launch,
            memory_mb,
            ready_timeout_secs,
            supplied.as_deref(),
        ),
        ResidencyAction::Disable {
            key,
            actor: supplied,
        } => disable(context, &key, supplied.as_deref()),
        ResidencyAction::Status { limit } => status(context, limit),
    }
}

fn enable(
    context: &ConfigContext,
    key: &str,
    worker: &str,
    launch: &[String],
    memory_mb: Option<u64>,
    ready_timeout_secs: u64,
    supplied_actor: Option<&str>,
) -> Result<(), String> {
    if launch.is_empty() {
        return Err(
            "--launch is required: Familiar never guesses how a local serving runtime is started"
                .into(),
        );
    }
    let config = load_config(context)?;
    // Build the candidate and validate it against the real worker registry
    // before writing a byte, so a bad residency entry is refused with the
    // offending value named rather than persisted and failed at startup.
    let mut candidate: ModelResidencyConfig = config.model_residency.clone();
    candidate.enabled = true;
    if candidate
        .residents
        .insert(
            key.to_string(),
            ResidentModelConfig {
                enabled: true,
                worker: worker.to_string(),
                launch: launch.to_vec(),
                memory_mb,
                ready_timeout_secs,
            },
        )
        .is_some_and(|previous| previous.enabled)
    {
        return Err(format!("resident '{key}' is already enabled"));
    }
    candidate.validate(config.worker_registry.as_ref())?;

    let actor = actor(supplied_actor)?;
    let now = chrono::Utc::now().to_rfc3339();
    let launch = launch.to_vec();
    let key_owned = key.to_string();
    let worker_owned = worker.to_string();
    let actor_for_edit = actor.clone();
    mutate(
        context,
        "familiar-ai model-residency enable",
        &actor,
        move |document: &mut Document| {
            let residency = root_table(document, "model_residency")?;
            residency["enabled"] = value(true);
            if !residency.contains_key("residents") {
                residency.insert("residents", Item::Table(Table::new()));
            }
            let residents = residency
                .get_mut("residents")
                .and_then(Item::as_table_mut)
                .ok_or("model_residency.residents is not a table")?;
            let mut table = Table::new();
            table.decor_mut().set_prefix(format!(
                "# added by familiar-ai model-residency enable — {actor_for_edit} {now}\n"
            ));
            table["enabled"] = value(true);
            table["worker"] = value(worker_owned.as_str());
            let mut argv = Array::new();
            for argument in &launch {
                argv.push(argument.as_str());
            }
            table["launch"] = value(argv);
            if let Some(memory_mb) = memory_mb {
                table["memory_mb"] = value(memory_mb as i64);
            }
            table["ready_timeout_secs"] = value(ready_timeout_secs as i64);
            residents.insert(&key_owned, Item::Table(table));
            Ok(())
        },
    )?;
    println!(
        "model residency: '{key}' enabled for worker '{worker}' ({actor})\n\
         the daemon holds it resident from its next health interval; \
         `familiar-ai model-residency status` shows recorded lifecycle events"
    );
    Ok(())
}

fn disable(context: &ConfigContext, key: &str, supplied_actor: Option<&str>) -> Result<(), String> {
    let config = load_config(context)?;
    if !config.model_residency.residents.contains_key(key) {
        return Err(format!("no configured resident '{key}'"));
    }
    let actor = actor(supplied_actor)?;
    let key_owned = key.to_string();
    mutate(
        context,
        "familiar-ai model-residency disable",
        &actor,
        move |document: &mut Document| {
            let residency = root_table(document, "model_residency")?;
            let residents = residency
                .get_mut("residents")
                .and_then(Item::as_table_mut)
                .ok_or("model_residency.residents is not a table")?;
            let resident = residents
                .get_mut(&key_owned)
                .and_then(Item::as_table_mut)
                .ok_or_else(|| format!("model_residency.residents.{key_owned} is not a table"))?;
            // The entry is disabled rather than deleted: the operator's
            // declared launch command and budget survive so re-enabling is
            // not a re-derivation, and the audit trail shows a toggle.
            resident["enabled"] = value(false);
            Ok(())
        },
    )?;
    println!(
        "model residency: '{key}' disabled ({actor})\n\
         the daemon stops the resident server at its next health interval and records the stop"
    );
    Ok(())
}

fn status(context: &ConfigContext, limit: usize) -> Result<(), String> {
    let config = load_config(context)?;
    let residency = &config.model_residency;
    println!(
        "model residency: {} (max_residents {}, memory_ceiling_mb {}, health every {}s, max_restarts {})",
        if residency.enabled { "enabled" } else { "disabled" },
        residency.max_residents,
        residency
            .memory_ceiling_mb
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unset".into()),
        residency.health_interval_secs,
        residency.max_restarts,
    );
    if residency.residents.is_empty() {
        println!("  no residents configured");
    }
    for (key, resident) in &residency.residents {
        println!(
            "  {key}: {} worker={} launch={:?} memory_mb={}",
            if resident.enabled && residency.enabled {
                "active"
            } else {
                "inactive"
            },
            resident.worker,
            resident.launch,
            resident
                .memory_mb
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unset".into()),
        );
    }
    let db = open_database(context, &config)?;
    let events = ModelResidencyRepository::new(db.conn())
        .recent(limit)
        .map_err(|error| error.to_string())?;
    if events.is_empty() {
        println!("no recorded residency events");
        return Ok(());
    }
    println!("recent residency events:");
    for event in events {
        println!(
            "  {} {} {} server={} {}",
            event.recorded_at,
            event.resident_key,
            event.event.as_str(),
            event.server_identity,
            event.reason.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}
