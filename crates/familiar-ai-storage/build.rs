use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn migration_files(dir: &Path) -> Result<Vec<(i64, PathBuf)>, String> {
    let mut migrations = Vec::new();
    for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("sql") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("migration path is not UTF-8: {}", path.display()))?;
        let prefix = name
            .split('_')
            .next()
            .ok_or_else(|| format!("migration filename lacks a version: {name}"))?;
        if prefix.len() != 3 || !prefix.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("migration filename must begin NNN_: {name}"));
        }
        let version = prefix.parse::<i64>().map_err(|error| error.to_string())?;
        migrations.push((version, path));
    }
    migrations.sort_by_key(|(version, _)| *version);
    for pair in migrations.windows(2) {
        if pair[0].0 == pair[1].0 {
            return Err(format!(
                "migration version {} is claimed by {} and {}",
                pair[0].0,
                pair[0].1.display(),
                pair[1].1.display()
            ));
        }
    }
    Ok(migrations)
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let directory = manifest.join("migrations");
    println!("cargo:rerun-if-changed={}", directory.display());
    let migrations = migration_files(&directory).unwrap_or_else(|error| panic!("{}", error));
    assert!(!migrations.is_empty(), "no migrations found");

    let mut generated = String::from("const MIGRATIONS: &[Migration] = &[\n");
    for (version, path) in migrations {
        generated.push_str(&format!(
            "    Migration {{ version: {version}, sql: include_str!({path:?}) }},\n"
        ));
    }
    generated.push_str("];\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("migrations.rs");
    fs::write(output, generated).expect("write generated migration registry");
}
