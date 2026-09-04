//! `familiar-ai batch-review` — PRD-071 batch-tier configuration and
//! observability. Batch tiering defaults off; `enable`/`disable` are
//! audited configuration mutations (`ConfigDecisionRepository`, via the
//! same `mutate` sink every other config verb uses) so an operator can
//! never silently downgrade a high-risk class's latency guarantee.
//! `pending` is read-only durable-state observation.

use clap::Subcommand;
use toml_edit::{value, Document, Item, Table};

use familiar_ai_core::config::ReviewTierPolicyConfig;

use crate::batch_review::resolved_tier_policy;
use crate::config_cli::{load_config, mutate, repository_identity, root_table, ConfigContext};

#[derive(Debug, Subcommand)]
pub enum BatchReviewCommand {
    /// Route the current repository's declared risk class through the
    /// provider batch interface, bounded by `--max-wait-ms`, submitted via
    /// the named `worker_registry.workers` entry (must run the
    /// `anthropic-api` runtime).
    Enable {
        risk_class: String,
        #[arg(long)]
        worker: String,
        #[arg(long)]
        max_wait_ms: u64,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Remove a declared risk class from the current repository's batch
    /// tier; it falls back to whatever tier its footprint or full-review
    /// declaration already selects.
    Disable {
        risk_class: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Durable batch-review rows still awaiting a provider result, across
    /// every repository — cached-local, never a network call.
    Pending,
}

pub fn batch_review_command(command: BatchReviewCommand) -> Result<(), String> {
    match command {
        BatchReviewCommand::Enable {
            risk_class,
            worker,
            max_wait_ms,
            actor,
        } => enable(&risk_class, &worker, max_wait_ms, actor.as_deref()),
        BatchReviewCommand::Disable { risk_class, actor } => disable(&risk_class, actor.as_deref()),
        BatchReviewCommand::Pending => pending(),
    }
}

fn actor_or_default(actor: Option<&str>) -> String {
    actor.map(str::to_owned).unwrap_or_else(|| {
        std::env::var("USER")
            .map(|user| format!("human:{user}"))
            .unwrap_or_else(|_| "human:unknown".into())
    })
}

fn enable(
    risk_class: &str,
    worker: &str,
    max_wait_ms: u64,
    actor: Option<&str>,
) -> Result<(), String> {
    let context = ConfigContext::resolve()?;
    enable_with_context(&context, risk_class, worker, max_wait_ms, actor)
}

fn enable_with_context(
    context: &ConfigContext,
    risk_class: &str,
    worker: &str,
    max_wait_ms: u64,
    actor: Option<&str>,
) -> Result<(), String> {
    if risk_class.trim().is_empty() || risk_class.trim() != risk_class {
        return Err("risk class must be non-empty and trimmed".into());
    }
    if max_wait_ms == 0 {
        return Err("--max-wait-ms must be positive".into());
    }
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let (_, key) = repository_identity(&current)?;
    let actor = actor_or_default(actor);
    let effective_policy = effective_tier_policy(context, &key)?;
    mutate(
        context,
        "familiar-ai batch-review enable",
        &actor,
        |document| {
            let policy = tier_policy_table(document, &key, &effective_policy)?;
            let classes = batch_risk_classes_array(policy)?;
            if !classes
                .iter()
                .any(|entry| entry.as_str() == Some(risk_class))
            {
                classes.push(risk_class);
            }
            policy.insert("max_batch_wait_ms", value(max_wait_ms as i64));
            policy.insert("batch_worker", value(worker));
            Ok(())
        },
    )?;
    println!("batch review enabled for risk class '{risk_class}' in {key} (worker={worker}, max_wait_ms={max_wait_ms})");
    Ok(())
}

fn disable(risk_class: &str, actor: Option<&str>) -> Result<(), String> {
    let context = ConfigContext::resolve()?;
    disable_with_context(&context, risk_class, actor)
}

fn disable_with_context(
    context: &ConfigContext,
    risk_class: &str,
    actor: Option<&str>,
) -> Result<(), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let (_, key) = repository_identity(&current)?;
    let actor = actor_or_default(actor);
    let effective_policy = effective_tier_policy(context, &key)?;
    mutate(
        context,
        "familiar-ai batch-review disable",
        &actor,
        |document| {
            let policy = tier_policy_table(document, &key, &effective_policy)?;
            let classes = batch_risk_classes_array(policy)?;
            let kept: Vec<String> = classes
                .iter()
                .filter_map(|entry| entry.as_str())
                .filter(|entry| *entry != risk_class)
                .map(str::to_owned)
                .collect();
            *classes = toml_edit::Array::new();
            for entry in &kept {
                classes.push(entry.as_str());
            }
            Ok(())
        },
    )?;
    println!("batch review disabled for risk class '{risk_class}' in {key}");
    Ok(())
}

fn pending() -> Result<(), String> {
    let db = super::shared::database()?;
    let repository = familiar_ai_storage::BatchReviewRepository::new(db.conn());
    let rows = repository.submitted().map_err(|error| error.to_string())?;
    if rows.is_empty() {
        println!("no batch reviews are awaiting a provider result");
        return Ok(());
    }
    for row in rows {
        println!(
            "{}\t{}\t{}\trisk_class={}\tbatch_id={}\tsubmitted_at={}\tdeadline_at={}",
            row.repository_key,
            row.prd_id,
            row.review_id,
            row.risk_class,
            row.provider_batch_id,
            row.submitted_at,
            row.deadline_at,
        );
    }
    Ok(())
}

/// Resolves the `tier_policy` currently in effect for `repository_key` —
/// its own repository-scoped override if one already exists, else the
/// global `[review.tier_policy]`, else the all-default policy — exactly
/// the precedence `resolved_tier_policy` applies at execution time. Used
/// to seed a freshly created repository-scoped table so enabling batch for
/// one risk class can never silently drop `independent_review_required`,
/// `full_review_risk_classes`, or `rules` that a repository previously
/// inherited from the global policy (or already declared for itself).
fn effective_tier_policy(
    context: &ConfigContext,
    repository_key: &str,
) -> Result<ReviewTierPolicyConfig, String> {
    let config = load_config(context)?;
    Ok(resolved_tier_policy(&config, repository_key)
        .cloned()
        .unwrap_or_default())
}

fn tier_policy_table<'a>(
    document: &'a mut Document,
    repository_key: &str,
    effective_policy: &ReviewTierPolicyConfig,
) -> Result<&'a mut Table, String> {
    let repositories = root_table(document, "repositories")?;
    if !repositories.contains_key(repository_key) {
        repositories.insert(repository_key, Item::Table(Table::new()));
    }
    let repository = repositories
        .get_mut(repository_key)
        .and_then(Item::as_table_mut)
        .ok_or("repository config is not a table")?;
    if !repository.contains_key("review") {
        repository.insert("review", Item::Table(Table::new()));
    }
    let review = repository
        .get_mut("review")
        .and_then(Item::as_table_mut)
        .ok_or("review config is not a table")?;
    if !review.contains_key("tier_policy") {
        review.insert("tier_policy", seed_tier_policy_item(effective_policy)?);
    }
    review
        .get_mut("tier_policy")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| "tier_policy config is not a table".into())
}

/// Round-trips `policy` through TOML serialization into an editable
/// `toml_edit::Item`, so a newly created repository-scoped `tier_policy`
/// table starts from the same field values the effective policy already
/// carries instead of an empty table that would silently default every
/// field `enable`/`disable` don't explicitly touch.
fn seed_tier_policy_item(policy: &ReviewTierPolicyConfig) -> Result<Item, String> {
    let encoded = toml::to_string(policy).map_err(|error| error.to_string())?;
    let document: Document = encoded
        .parse()
        .map_err(|error: toml_edit::TomlError| error.to_string())?;
    Ok(Item::Table(document.as_table().clone()))
}

fn batch_risk_classes_array(policy: &mut Table) -> Result<&mut toml_edit::Array, String> {
    if !policy.contains_key("batch_risk_classes") {
        policy.insert("batch_risk_classes", value(toml_edit::Array::new()));
    }
    policy
        .get_mut("batch_risk_classes")
        .and_then(Item::as_array_mut)
        .ok_or_else(|| "tier_policy.batch_risk_classes is not an array".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familiar_ai_core::Config;

    /// `Config::validate` requires every `repositories` key to be an
    /// absolute, canonicalizable worktree path, so the fixture repository
    /// must be a real directory rather than an arbitrary string.
    fn repository_key(root: &std::path::Path) -> String {
        let repository = root.join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        repository
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// A global `[review.tier_policy]` an operator declared before batch
    /// tiering existed: independent review required, one full-review risk
    /// class, and a footprint rule — plus the `worker_registry` entry and
    /// risk vocabulary `enable` needs to be well-formed once it adds the
    /// batch class.
    fn base_config(repository_key: &str) -> String {
        format!(
            r#"
[review.tier_policy]
independent_review_required = true
full_review_risk_classes = ["critical"]

[[review.tier_policy.rules]]
id = "critical-footprint"
tier = "full"
path_prefixes = ["src/"]

[worker_registry.workers.batch-anthropic]
provider = "anthropic"
model = "claude-review-model"
runtime = "anthropic-api"
capabilities = ["implementation"]

[repositories.{repository_key:?}]
risk_vocabulary = ["critical", "low-risk-docs"]
"#
        )
    }

    fn write_context(directory: &std::path::Path, contents: &str) -> ConfigContext {
        let context = ConfigContext {
            config_path: directory.join("config.toml"),
            data_dir: directory.join("data"),
        };
        std::fs::write(&context.config_path, contents).unwrap();
        context
    }

    /// f2 regression: a repository that has never declared its own
    /// `tier_policy` must resolve the global one — the same effective
    /// policy `enable` seeds a freshly created repository-scoped table
    /// from.
    #[test]
    fn effective_tier_policy_falls_back_to_the_global_policy() {
        let directory = tempfile::tempdir().unwrap();
        let key = repository_key(directory.path());
        let context = write_context(directory.path(), &base_config(&key));

        let effective = effective_tier_policy(&context, &key).unwrap();

        assert!(effective.independent_review_required);
        assert_eq!(
            effective.full_review_risk_classes,
            vec!["critical".to_string()]
        );
        assert_eq!(effective.rules.len(), 1);
        assert_eq!(effective.rules[0].id, "critical-footprint");
    }

    /// f2 regression: `enable` creating a repository-scoped `tier_policy`
    /// table for the first time must not silently revert
    /// `independent_review_required`, `full_review_risk_classes`, or
    /// `rules` to their type defaults — it must inherit the effective
    /// (global) policy's fields and only change the batch-specific ones.
    #[test]
    fn enabling_batch_for_one_class_preserves_inherited_tier_policy_fields() {
        let directory = tempfile::tempdir().unwrap();
        let key = repository_key(directory.path());
        let context = write_context(directory.path(), &base_config(&key));

        let effective = effective_tier_policy(&context, &key).unwrap();
        let mut document: Document = std::fs::read_to_string(&context.config_path)
            .unwrap()
            .parse()
            .unwrap();
        {
            let policy = tier_policy_table(&mut document, &key, &effective).unwrap();
            let classes = batch_risk_classes_array(policy).unwrap();
            classes.push("low-risk-docs");
            policy.insert("max_batch_wait_ms", value(3_600_000_i64));
            policy.insert("batch_worker", value("batch-anthropic"));
        }

        let rendered = document.to_string();
        let config: Config = toml::from_str(&rendered).unwrap();
        config.validate().unwrap();

        let scoped = config
            .repositories
            .get(&key)
            .and_then(|repository| repository.review.as_ref())
            .and_then(|review| review.tier_policy.as_ref())
            .expect("enable creates a repository-scoped tier_policy");

        // Inherited from the global policy — must survive, never revert to
        // the all-default `ReviewTierPolicyConfig`.
        assert!(scoped.independent_review_required);
        assert_eq!(
            scoped.full_review_risk_classes,
            vec!["critical".to_string()]
        );
        assert_eq!(scoped.rules.len(), 1);
        assert_eq!(scoped.rules[0].id, "critical-footprint");

        // The batch fields `enable` explicitly writes.
        assert_eq!(scoped.batch_risk_classes, vec!["low-risk-docs".to_string()]);
        assert_eq!(scoped.max_batch_wait_ms, 3_600_000);
        assert_eq!(scoped.batch_worker.as_deref(), Some("batch-anthropic"));
    }
}
