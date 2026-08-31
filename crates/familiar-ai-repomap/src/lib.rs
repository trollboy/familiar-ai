//! Incremental, deterministic repository symbol maps.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub signature: String,
    pub line: u32,
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileMap {
    pub path: String,
    pub content_hash: String,
    pub symbols: Vec<Symbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissingCoverage {
    pub path: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RepositoryMap {
    files: BTreeMap<String, FileMap>,
    missing: BTreeMap<String, MissingCoverage>,
    #[serde(skip)]
    reindex_counts: BTreeMap<String, u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("path is outside repository: {0}")]
    OutsideRepository(PathBuf),
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unsupported or unparseable file: {0}")]
    Unparseable(String),
}

impl RepositoryMap {
    pub fn new(watch_covered: bool) -> Self {
        let mut value = Self::default();
        if !watch_covered {
            value.mark_missing(None, "repository has no watch coverage");
        }
        value
    }
    pub fn files(&self) -> &BTreeMap<String, FileMap> {
        &self.files
    }
    pub fn missing_coverage(&self) -> impl Iterator<Item = &MissingCoverage> {
        self.missing.values()
    }
    pub fn reindex_count(&self, path: &str) -> u64 {
        self.reindex_counts.get(path).copied().unwrap_or(0)
    }
    pub fn mark_stale(&mut self, reason: impl Into<String>) {
        self.mark_missing(None, reason);
    }
    pub fn mark_missing(&mut self, path: Option<String>, reason: impl Into<String>) {
        let key = path.clone().unwrap_or_else(|| "<repository>".into());
        self.missing.insert(
            key,
            MissingCoverage {
                path,
                reason: reason.into(),
            },
        );
    }
    pub fn remove_file(&mut self, repository: &Path, path: &Path) -> Result<(), MapError> {
        let relative = relative(repository, path)?;
        self.files.remove(&relative);
        self.missing.remove(&relative);
        Ok(())
    }
    /// Replaces exactly one file region and its outgoing edges.
    pub fn reindex_file(&mut self, repository: &Path, path: &Path) -> Result<(), MapError> {
        let relative = relative(repository, path)?;
        *self.reindex_counts.entry(relative.clone()).or_default() += 1;
        match parse_file(path, &relative) {
            Ok(file) => {
                self.files.insert(relative.clone(), file);
                self.missing.remove(&relative);
                Ok(())
            }
            Err(error) => {
                self.files.remove(&relative);
                self.mark_missing(Some(relative), error.to_string());
                Err(error)
            }
        }
    }
    /// One length-delimited JSON region per path. Ordering and bytes are stable;
    /// editing a file can only replace its region (plus the coverage trailer).
    pub fn serialize(&self, max_symbols: usize) -> Vec<u8> {
        let mut out = b"FAMILIAR-REPOMAP-v1\n".to_vec();
        for (path, file) in &self.files {
            let mut file = file.clone();
            // Connectivity ranking is file-local so another file's edit can
            // never perturb this content-addressed region.
            file.symbols.sort_by_key(|s| {
                (
                    std::cmp::Reverse(s.references.len()),
                    s.line,
                    s.name.clone(),
                )
            });
            file.symbols.truncate(max_symbols.saturating_sub(0));
            let bytes = serde_json::to_vec(&file).expect("serializable map");
            out.extend_from_slice(format!("FILE {path} {:010}\n", bytes.len()).as_bytes());
            out.extend(bytes);
            out.push(b'\n');
        }
        let missing: Vec<_> = self.missing.values().collect();
        let bytes = serde_json::to_vec(&missing).expect("serializable coverage");
        out.extend_from_slice(format!("COVERAGE {:010}\n", bytes.len()).as_bytes());
        out.extend(bytes);
        out.push(b'\n');
        out
    }
}

pub fn repository_cache_key(repository: &Path) -> String {
    stable_hash(repository.to_string_lossy().as_bytes())
}

fn relative(repository: &Path, path: &Path) -> Result<String, MapError> {
    path.strip_prefix(repository)
        .map_err(|_| MapError::OutsideRepository(path.to_owned()))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}
fn parse_file(path: &Path, relative: &str) -> Result<FileMap, MapError> {
    let text = fs::read_to_string(path).map_err(|source| MapError::Read {
        path: path.into(),
        source,
    })?;
    let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
    let pattern = match ext {
        "rs" => {
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|type|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)"
        }
        "py" => r"^\s*(?:async\s+)?(?:def|class)\s+([A-Za-z_][A-Za-z0-9_]*)",
        "js" | "jsx" | "ts" | "tsx" => {
            r"^\s*(?:export\s+)?(?:async\s+)?(?:function|class|interface|type|const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)"
        }
        "go" => r"^\s*(?:func|type|const|var)\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)",
        _ => {
            return Err(MapError::Unparseable(format!(
                "{relative}: unsupported language"
            )))
        }
    };
    let definition = Regex::new(pattern).expect("definition regex");
    let words = Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").unwrap();
    let mut symbols = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if let Some(c) = definition.captures(line) {
            let name = c[1].to_owned();
            let references = words
                .find_iter(line)
                .map(|m| m.as_str().to_owned())
                .filter(|w| w != &name)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            symbols.push(Symbol {
                name,
                signature: line.trim().to_owned(),
                line: (index + 1) as u32,
                references,
            });
        }
    }
    let hash = stable_hash(text.as_bytes());
    Ok(FileMap {
        path: relative.into(),
        content_hash: hash,
        symbols,
    })
}
fn stable_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_and_stable_regions() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a.rs");
        let b = t.path().join("b.rs");
        fs::write(&a, "pub fn a() {}\n").unwrap();
        fs::write(&b, "pub fn b() {}\n").unwrap();
        let mut m = RepositoryMap::new(true);
        m.reindex_file(t.path(), &a).unwrap();
        m.reindex_file(t.path(), &b).unwrap();
        let before = m.serialize(100);
        fs::write(&a, "pub fn changed() {}\n").unwrap();
        m.reindex_file(t.path(), &a).unwrap();
        assert_eq!(m.reindex_count("a.rs"), 2);
        assert_eq!(m.reindex_count("b.rs"), 1);
        let after = m.serialize(100);
        let br = region(&before, "b.rs");
        assert_eq!(br, region(&after, "b.rs"));
    }
    fn region<'a>(x: &'a [u8], path: &str) -> &'a [u8] {
        let s = std::str::from_utf8(x).unwrap();
        let start = s.find(&format!("FILE {path} ")).unwrap();
        let end = s[start..].find('\n').unwrap() + start + 1;
        let len: usize = s[start..end]
            .split_whitespace()
            .last()
            .unwrap()
            .parse()
            .unwrap();
        &x[start..end + len]
    }
}
