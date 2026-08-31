use std::fs;

use familiar_ai_core::{AppPaths, Config};
use familiar_ai_storage::{AccountingRepository, ConfigDecisionRepository, Database};
use ring::digest::{digest, SHA256};
use toml_edit::{value, Document, Item, Table};

fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn context() -> Result<(AppPaths, Config), String> {
    let paths = AppPaths::resolve().map_err(|error| error.to_string())?;
    let config = Config::load(Some(&paths.config_dir.join("config.toml")))
        .map_err(|error| error.to_string())?;
    Ok((paths, config))
}

pub fn configure_output(stage: &str, register: &str, actor: &str) -> Result<(), String> {
    if !matches!(stage, "implementation" | "review" | "remediation") {
        return Err("stage must be implementation, review, or remediation".into());
    }
    if register != "compact" {
        return Err("register must be compact".into());
    }
    mutate(
        &format!("compress output-enable {stage} {register}"),
        actor,
        |doc| {
            let compression = table(doc, "compression");
            let registers = child_table(compression, "output_registers");
            registers[stage] = value(register);
        },
    )
}

pub fn configure_input(provider: &str, transform: &str, actor: &str) -> Result<(), String> {
    if transform != "native-rle" {
        return Err("input transform must be native-rle".into());
    }
    mutate(
        &format!("compress input-enable {provider} {transform}"),
        actor,
        |doc| {
            let compression = table(doc, "compression");
            let providers = child_table(compression, "input_providers");
            providers[provider] = value(transform);
        },
    )
}

pub fn configure_experiment(label: &str, lane: &str, actor: &str) -> Result<(), String> {
    if label.trim().is_empty() {
        return Err("experiment label must be non-empty".into());
    }
    if !matches!(lane, "off" | "on") {
        return Err("experiment lane must be off or on".into());
    }
    mutate(
        &format!("compress experiment {label} --lane {lane}"),
        actor,
        |doc| {
            let compression = table(doc, "compression");
            compression["experiment_label"] = value(label);
            compression["experiment_lane"] = value(lane);
        },
    )
}

fn mutate(command: &str, actor: &str, apply: impl FnOnce(&mut Document)) -> Result<(), String> {
    if actor.trim().is_empty() {
        return Err("--actor must be non-empty".into());
    }
    let (paths, before_config) = context()?;
    let path = paths.config_dir.join("config.toml");
    let before = fs::read_to_string(&path).unwrap_or_default();
    let mut document = before
        .parse::<Document>()
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    apply(&mut document);
    let after = document.to_string();
    let temp = path.with_extension("toml.compression.tmp");
    fs::write(&temp, &after).map_err(|error| error.to_string())?;
    Config::load(Some(&temp)).map_err(|error| error.to_string())?;
    fs::rename(&temp, &path).map_err(|error| error.to_string())?;
    let db = Database::open(&before_config.database.resolve_path(&paths.data_dir))
        .map_err(|error| error.to_string())?;
    db.run_migrations().map_err(|error| error.to_string())?;
    ConfigDecisionRepository::new(&db)
        .record(
            command,
            actor,
            &hash(before.as_bytes()),
            &hash(after.as_bytes()),
        )
        .map_err(|error| error.to_string())?;
    println!("{command}: enabled actor={actor}");
    Ok(())
}

fn table<'a>(document: &'a mut Document, key: &str) -> &'a mut Table {
    if !document.as_table().contains_key(key) {
        document[key] = Item::Table(Table::new());
    }
    document[key].as_table_mut().expect("table initialized")
}

fn child_table<'a>(parent: &'a mut Table, key: &str) -> &'a mut Table {
    if !parent.contains_key(key) {
        parent[key] = Item::Table(Table::new());
    }
    parent[key].as_table_mut().expect("table initialized")
}

pub fn experiment(label: &str) -> Result<(), String> {
    let (paths, config) = context()?;
    let db = Database::open(&config.database.resolve_path(&paths.data_dir))
        .map_err(|error| error.to_string())?;
    db.run_migrations().map_err(|error| error.to_string())?;
    let result = AccountingRepository::new(db.conn())
        .compression_experiment(label)
        .map_err(|error| error.to_string())?;
    println!("compression experiment {label}");
    print_lane("off", &result.off);
    print_lane("on", &result.on);
    print_delta(
        "uncached_input",
        result.off.uncached_input_tokens,
        result.on.uncached_input_tokens,
    );
    print_delta(
        "cache_read",
        result.off.cache_read_tokens,
        result.on.cache_read_tokens,
    );
    print_delta(
        "cache_write",
        result.off.cache_write_tokens,
        result.on.cache_write_tokens,
    );
    print_delta("output", result.off.output_tokens, result.on.output_tokens);
    print_delta(
        "known_cost_nanousd",
        result.off.known_nanousd,
        result.on.known_nanousd,
    );
    Ok(())
}

fn print_lane(name: &str, lane: &familiar_ai_storage::CompressionLaneSummary) {
    println!("lane={name} observations={}", lane.observations);
}

fn print_delta(category: &str, off: Option<u64>, on: Option<u64>) {
    match (off, on) {
        (Some(off), Some(on)) => println!(
            "{category}: off={off} on={on} delta={}",
            i128::from(on) - i128::from(off)
        ),
        _ => println!("{category}: unobserved"),
    }
}
