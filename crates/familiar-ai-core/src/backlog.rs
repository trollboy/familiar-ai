use ring::digest::{digest, SHA256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Repository-relative directory holding active (unfinished) PRDs.
pub const ACTIVE_PRD_DIR: &str = "docs/prds";
/// Repository-relative directory holding archived (completed) PRDs. Read one
/// level deep only; nested directories below it are not interpreted.
pub const ARCHIVED_PRD_DIR: &str = "docs/prds/done";
/// Actor recorded when reconciliation corrects a stored status to match a
/// PRD's archived location, so the correction is visible rather than silent.
pub const ARCHIVED_LOCATION_ACTOR: &str = "system:archived-location";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrdId(u64);

impl PrdId {
    pub fn new(number: u64) -> Self {
        Self(number)
    }
    pub fn number(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for PrdId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PRD-{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    pub fn new(path: impl Into<String>) -> Result<Self, BacklogError> {
        let path = path.into();
        if path.is_empty()
            || path.starts_with('/')
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(BacklogError::Discovery(format!(
                "invalid repository-relative path '{path}'"
            )));
        }
        Ok(Self(path))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RepositoryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIdentity {
    pub worktree: PathBuf,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacklogStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacklogRecoveryAction {
    Release,
    ManualCompleteOverride,
    RecordedComplete,
}

impl BacklogRecoveryAction {
    pub fn target_status(self) -> BacklogStatus {
        match self {
            Self::Release => BacklogStatus::Pending,
            Self::ManualCompleteOverride => BacklogStatus::Completed,
            Self::RecordedComplete => BacklogStatus::Completed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::ManualCompleteOverride => "manual_complete_override",
            Self::RecordedComplete => "recorded_complete",
        }
    }
}

pub fn validate_recovery_attribution(
    action: BacklogRecoveryAction,
    actor: &str,
    reason: &str,
) -> Result<(String, String), BacklogStoreError> {
    let actor = actor.trim();
    let reason = reason.trim();
    if actor.is_empty() || actor.len() > 256 || actor.chars().any(char::is_control) {
        return Err(BacklogStoreError::InvalidRecoveryActor);
    }
    if reason.is_empty() || reason.len() > 2048 || reason.chars().any(char::is_control) {
        return Err(BacklogStoreError::InvalidRecoveryReason);
    }
    if matches!(
        action,
        BacklogRecoveryAction::ManualCompleteOverride | BacklogRecoveryAction::RecordedComplete
    ) && !matches!(actor.strip_prefix("human:"), Some(identity) if !identity.trim().is_empty())
    {
        return Err(BacklogStoreError::HumanAuthorityRequired);
    }
    Ok((actor.to_owned(), reason.to_owned()))
}

impl BacklogStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }
    pub fn parse(value: &str) -> Result<Self, BacklogStoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "blocked" => Ok(Self::Blocked),
            _ => Err(BacklogStoreError::InvalidStatus(value.into())),
        }
    }
}

/// Where discovery found a PRD. The filesystem, not the database, is
/// authoritative: an archived PRD is completed by virtue of its location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PrdLocation {
    Active,
    Archived,
}

impl PrdLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
    pub fn is_archived(self) -> bool {
        matches!(self, Self::Archived)
    }
    /// Recover a location from a stored repository-relative path. Location is
    /// carried by the path itself, so persisted rows need no extra column.
    pub fn from_repository_path(path: &RepositoryPath) -> Self {
        let archived_prefix = format!("{ARCHIVED_PRD_DIR}/");
        if path.as_str().starts_with(&archived_prefix) {
            Self::Archived
        } else {
            Self::Active
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPrd {
    pub id: PrdId,
    pub number: u64,
    pub path: RepositoryPath,
    pub title: String,
    pub dependencies: Vec<PrdId>,
    pub content_hash: String,
    pub location: PrdLocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogEntry {
    pub prd: DiscoveredPrd,
    pub status: BacklogStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextPrd {
    pub id: PrdId,
    pub path: RepositoryPath,
    pub title: String,
    pub status: BacklogStatus,
    pub dependencies: Vec<PrdId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IneligibilityReason {
    StatusInProgress,
    StatusCompleted,
    StatusBlocked,
    DependenciesIncomplete { dependencies: Vec<PrdId> },
}

#[derive(Debug, thiserror::Error)]
pub enum BacklogStoreError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("invalid persisted backlog status '{0}'")]
    InvalidStatus(String),
    #[error("status transition conflict for {path}: expected {expected}, found {actual}")]
    Conflict {
        path: RepositoryPath,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("status transition actor must not be empty")]
    EmptyActor,
    #[error("backlog entry not found: {0}")]
    NotFound(RepositoryPath),
    #[error("recovery actor must be non-empty, printable, and at most 256 bytes")]
    InvalidRecoveryActor,
    #[error("recovery reason must be non-empty, printable, and at most 2048 bytes")]
    InvalidRecoveryReason,
    #[error("manual completion override requires --actor human:<identity>")]
    HumanAuthorityRequired,
    #[error("backlog recovery audit lineage is corrupt for {0}")]
    RecoveryAuditCorrupt(RepositoryPath),
    #[error("cannot record completion for {path}: incomplete dependencies [{dependencies}]")]
    IncompleteDependencies {
        path: RepositoryPath,
        dependencies: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum BacklogError {
    #[error("repository resolution failed: {0}")]
    Repository(String),
    #[error("backlog discovery failed: {0}")]
    Discovery(String),
    #[error("malformed PRD {path}: {message}")]
    Malformed { path: String, message: String },
    #[error("duplicate PRD identity {id}: {paths}")]
    DuplicateIdentity { id: PrdId, paths: String },
    #[error("PRD {id} is present in both locations: {active}, {archived}")]
    DualLocation {
        id: PrdId,
        active: RepositoryPath,
        archived: RepositoryPath,
    },
    #[error(
        "dependency target {dependency} referenced by {path} is absent from the active backlog"
    )]
    MissingDependency {
        path: RepositoryPath,
        dependency: PrdId,
    },
    #[error("PRD {path} depends on itself ({id})")]
    SelfDependency { path: RepositoryPath, id: PrdId },
    #[error("dependency cycle: {0}")]
    Cycle(String),
    #[error("backlog storage failed: {0}")]
    Store(#[from] BacklogStoreError),
    #[error("backlog is empty")]
    EmptyBacklog,
    #[error("no eligible PRD: {0}")]
    NoEligiblePrd(String),
    #[error("run path admission failed: {0}")]
    RunPath(String),
    #[error("cannot run {path}: backlog status is {status}")]
    RunStatus {
        path: RepositoryPath,
        status: &'static str,
    },
    #[error("cannot run {path}: incomplete dependencies [{dependencies}]")]
    RunDependencies {
        path: RepositoryPath,
        dependencies: String,
    },
    #[error("cannot run {path}: PRD is archived and already completed")]
    RunArchived { path: RepositoryPath },
}

/// Resolve a user supplied run path to the exact bytes discovered for one active PRD.
pub fn resolve_run_prd(
    repository: &RepositoryIdentity,
    discovered: &[DiscoveredPrd],
    supplied: &Path,
) -> Result<DiscoveredPrd, BacklogError> {
    let candidate = if supplied.is_absolute() {
        supplied.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| BacklogError::RunPath(format!("cannot resolve current directory: {e}")))?
            .join(supplied)
    };
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|e| BacklogError::RunPath(format!("{}: {e}", supplied.display())))?;
    if metadata.file_type().is_symlink() {
        return Err(BacklogError::RunPath(format!(
            "{} is symlinked",
            supplied.display()
        )));
    }
    if !metadata.is_file() {
        return Err(BacklogError::RunPath(format!(
            "{} is not a regular file",
            supplied.display()
        )));
    }
    let canonical = candidate
        .canonicalize()
        .map_err(|e| BacklogError::RunPath(format!("{}: {e}", supplied.display())))?;
    let relative = canonical.strip_prefix(&repository.worktree).map_err(|_| {
        BacklogError::RunPath(format!("{} is outside the repository", supplied.display()))
    })?;
    let relative = relative
        .to_str()
        .ok_or_else(|| BacklogError::RunPath("run path is not UTF-8".into()))?
        .replace('\\', "/");
    let prd = discovered
        .iter()
        .find(|prd| prd.path.as_str() == relative)
        .cloned()
        .ok_or_else(|| {
            BacklogError::RunPath(format!(
                "{} is not an active backlog entry",
                supplied.display()
            ))
        })?;
    let bytes = fs::read(&canonical)
        .map_err(|e| BacklogError::RunPath(format!("{}: {e}", supplied.display())))?;
    let hash: String = digest(&SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if hash != prd.content_hash {
        return Err(BacklogError::RunPath(format!(
            "{} changed after backlog discovery",
            supplied.display()
        )));
    }
    Ok(prd)
}

/// The status a PRD has for dependency purposes. Location outranks the stored
/// value: an archived PRD is completed even if a store has not yet recorded the
/// correction, so dependency resolution never depends on reconciliation having
/// already run.
fn effective_status(entry: &BacklogEntry) -> BacklogStatus {
    if entry.prd.location.is_archived() {
        BacklogStatus::Completed
    } else {
        entry.status
    }
}

pub fn admit_run_prd(entries: &[BacklogEntry], target: &DiscoveredPrd) -> Result<(), BacklogError> {
    if target.location.is_archived() {
        return Err(BacklogError::RunArchived {
            path: target.path.clone(),
        });
    }
    let entry = entries
        .iter()
        .find(|entry| entry.prd.path == target.path)
        .ok_or_else(|| BacklogError::RunPath(format!("{} is not active", target.path)))?;
    if entry.status != BacklogStatus::Pending {
        return Err(BacklogError::RunStatus {
            path: target.path.clone(),
            status: entry.status.as_str(),
        });
    }
    let statuses: BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.prd.id.clone(), effective_status(entry)))
        .collect();
    let incomplete: Vec<_> = target
        .dependencies
        .iter()
        .filter(|id| statuses.get(*id) != Some(&BacklogStatus::Completed))
        .cloned()
        .collect();
    if !incomplete.is_empty() {
        return Err(BacklogError::RunDependencies {
            path: target.path.clone(),
            dependencies: incomplete
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        });
    }
    Ok(())
}

pub trait BacklogDiscovery {
    fn resolve(&self, path: &Path) -> Result<RepositoryIdentity, BacklogError>;
    fn discover(&self, repository: &RepositoryIdentity)
        -> Result<Vec<DiscoveredPrd>, BacklogError>;
}

pub trait BacklogStatusStore {
    fn reconcile_and_snapshot(
        &mut self,
        repository: &RepositoryIdentity,
        discovered: &[DiscoveredPrd],
    ) -> Result<Vec<BacklogEntry>, BacklogStoreError>;
    fn transition(
        &mut self,
        repository: &RepositoryIdentity,
        path: &RepositoryPath,
        expected: BacklogStatus,
        next: BacklogStatus,
        actor: &str,
    ) -> Result<BacklogEntry, BacklogStoreError>;
}

pub struct BacklogManager<D, S> {
    discovery: D,
    store: S,
}

impl<D, S> BacklogManager<D, S>
where
    D: BacklogDiscovery,
    S: BacklogStatusStore,
{
    pub fn new(discovery: D, store: S) -> Self {
        Self { discovery, store }
    }
    pub fn next(&mut self, path: &Path) -> Result<NextPrd, BacklogError> {
        let repository = self.discovery.resolve(path)?;
        let discovered = self.discovery.discover(&repository)?;
        if discovered.is_empty() {
            return Err(BacklogError::EmptyBacklog);
        }
        validate_graph(&discovered)?;
        let mut entries = self
            .store
            .reconcile_and_snapshot(&repository, &discovered)?;
        entries.sort_by(|a, b| {
            (a.prd.number, a.prd.path.as_str().as_bytes())
                .cmp(&(b.prd.number, b.prd.path.as_str().as_bytes()))
        });
        let statuses: BTreeMap<_, _> = entries
            .iter()
            .map(|e| (e.prd.id.clone(), effective_status(e)))
            .collect();
        let mut reasons = Vec::new();
        for entry in entries {
            // Archived work is completed by location. Reconciliation already
            // records that, so this guard is redundant by construction — but
            // selection is exactly where a regression would be most expensive,
            // and archived PRDs do not belong in the ineligibility report.
            if entry.prd.location.is_archived() {
                continue;
            }
            let incomplete: Vec<_> = entry
                .prd
                .dependencies
                .iter()
                .filter(|id| statuses.get(*id) != Some(&BacklogStatus::Completed))
                .cloned()
                .collect();
            let reason = match entry.status {
                BacklogStatus::Pending if incomplete.is_empty() => {
                    return Ok(NextPrd {
                        id: entry.prd.id,
                        path: entry.prd.path,
                        title: entry.prd.title,
                        status: entry.status,
                        dependencies: entry.prd.dependencies,
                    })
                }
                BacklogStatus::Pending => IneligibilityReason::DependenciesIncomplete {
                    dependencies: incomplete,
                },
                BacklogStatus::InProgress => IneligibilityReason::StatusInProgress,
                BacklogStatus::Completed => IneligibilityReason::StatusCompleted,
                BacklogStatus::Blocked => IneligibilityReason::StatusBlocked,
            };
            reasons.push(format!("{}={}", entry.prd.id, reason_text(&reason)));
        }
        Err(BacklogError::NoEligiblePrd(reasons.join(", ")))
    }
    pub fn into_store(self) -> S {
        self.store
    }
}

fn reason_text(reason: &IneligibilityReason) -> String {
    match reason {
        IneligibilityReason::StatusInProgress => "in_progress".into(),
        IneligibilityReason::StatusCompleted => "completed".into(),
        IneligibilityReason::StatusBlocked => "blocked".into(),
        IneligibilityReason::DependenciesIncomplete { dependencies } => format!(
            "dependencies incomplete [{}]",
            dependencies
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

#[derive(Default)]
pub struct FilesystemBacklogDiscovery;

impl BacklogDiscovery for FilesystemBacklogDiscovery {
    fn resolve(&self, path: &Path) -> Result<RepositoryIdentity, BacklogError> {
        let cwd = path.canonicalize().map_err(|e| {
            BacklogError::Repository(format!("cannot resolve current directory: {e}"))
        })?;
        let git = |args: &[&str]| -> Result<PathBuf, BacklogError> {
            let output = Command::new("git")
                .args(["-C"])
                .arg(&cwd)
                .args(args)
                .output()
                .map_err(|e| BacklogError::Repository(format!("cannot run git: {e}")))?;
            if !output.status.success() {
                return Err(BacklogError::Repository(
                    String::from_utf8_lossy(&output.stderr).trim().to_string(),
                ));
            }
            let value = String::from_utf8(output.stdout)
                .map_err(|_| BacklogError::Repository("git returned a non-UTF-8 path".into()))?;
            Ok(PathBuf::from(value.trim()))
        };
        let worktree = git(&["rev-parse", "--show-toplevel"])?
            .canonicalize()
            .map_err(|e| BacklogError::Repository(format!("cannot canonicalize worktree: {e}")))?;
        let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?
            .canonicalize()
            .map_err(|e| {
                BacklogError::Repository(format!("cannot resolve Git common directory: {e}"))
            })?;
        let key = common
            .to_str()
            .ok_or_else(|| BacklogError::Repository("Git common directory is not UTF-8".into()))?
            .replace('\\', "/");
        Ok(RepositoryIdentity { worktree, key })
    }

    fn discover(
        &self,
        repository: &RepositoryIdentity,
    ) -> Result<Vec<DiscoveredPrd>, BacklogError> {
        // The active directory must exist and every candidate in it must parse.
        // The archived directory is optional, and a candidate that does not
        // parse there is skipped rather than fatal: `done/` accumulates
        // documents from earlier naming conventions that no dependency
        // references, and refusing to run because of a finished document would
        // fail closed against history rather than against risk.
        let mut parsed = scan_directory(repository, ACTIVE_PRD_DIR, PrdLocation::Active)?;
        parsed.extend(scan_directory(
            repository,
            ARCHIVED_PRD_DIR,
            PrdLocation::Archived,
        )?);
        parsed.sort_by(|a, b| {
            (a.number, a.path.as_str().as_bytes()).cmp(&(b.number, b.path.as_str().as_bytes()))
        });
        for pair in parsed.windows(2) {
            if pair[0].id != pair[1].id {
                continue;
            }
            // Both copies of one PRD is a strictly more specific diagnosis than
            // two PRDs sharing a number, so name it as such.
            let (first, second) = (&pair[0], &pair[1]);
            if first.location != second.location
                && basename(first.path.as_str()) == basename(second.path.as_str())
            {
                let (active, archived) = if first.location == PrdLocation::Active {
                    (first, second)
                } else {
                    (second, first)
                };
                return Err(BacklogError::DualLocation {
                    id: first.id.clone(),
                    active: active.path.clone(),
                    archived: archived.path.clone(),
                });
            }
            return Err(BacklogError::DuplicateIdentity {
                id: first.id.clone(),
                paths: format!("{}, {}", first.path, second.path),
            });
        }
        Ok(parsed)
    }
}

fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Read one directory, one level deep, collecting the PRDs it declares.
fn scan_directory(
    repository: &RepositoryIdentity,
    relative_dir: &str,
    location: PrdLocation,
) -> Result<Vec<DiscoveredPrd>, BacklogError> {
    let root = repository.worktree.join(relative_dir);
    let read = match fs::read_dir(&root) {
        Ok(read) => read,
        // An unarchived repository is a normal repository.
        Err(e) if location.is_archived() && e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new())
        }
        Err(e) => {
            return Err(BacklogError::Discovery(format!(
                "cannot read {relative_dir}: {e}"
            )))
        }
    };
    let mut candidates = Vec::new();
    for item in read {
        let item = item.map_err(|e| {
            BacklogError::Discovery(format!("cannot enumerate {relative_dir}: {e}"))
        })?;
        let name = match item.file_name().into_string() {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Subdirectories are never candidates, which is what keeps `done/`
        // one level deep and keeps `done/` itself out of the active scan.
        if filename_number(&name).is_some() {
            candidates.push((name, item.path()));
        }
    }
    candidates.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for (name, path) in candidates {
        let rel = format!("{relative_dir}/{name}");
        match parse_candidate(&path, &rel, &name, location) {
            Ok(v) => parsed.push(v),
            Err(e) if location.is_archived() => {
                tracing::debug!(path = %rel, error = %e, "skipping unparseable archived PRD");
            }
            Err(e) => errors.push(e.to_string()),
        }
    }
    if !errors.is_empty() {
        return Err(BacklogError::Discovery(errors.join("; ")));
    }
    Ok(parsed)
}

fn filename_number(name: &str) -> Option<Result<u64, std::num::ParseIntError>> {
    let digits = name.strip_prefix("PRD-")?.strip_suffix(".md")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(digits.parse())
}

fn parse_candidate(
    path: &Path,
    relative: &str,
    name: &str,
    location: PrdLocation,
) -> Result<DiscoveredPrd, BacklogError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| malformed(relative, format!("cannot inspect file: {e}")))?;
    if metadata.file_type().is_symlink() {
        return Err(malformed(relative, "matching symlinks are forbidden"));
    }
    if !metadata.is_file() {
        return Err(malformed(relative, "matching entry is not a regular file"));
    }
    let number = filename_number(name)
        .expect("candidate already matched")
        .map_err(|_| malformed(relative, "filename number overflows u64"))?;
    let bytes =
        fs::read(path).map_err(|e| malformed(relative, format!("cannot read file: {e}")))?;
    let content = std::str::from_utf8(&bytes)
        .map_err(|_| malformed(relative, "content is not valid UTF-8"))?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.lines();
    let heading = lines
        .by_ref()
        .find(|line| !line.trim_matches([' ', '\t', '\r']).is_empty())
        .ok_or_else(|| malformed(relative, "missing level-one heading"))?;
    let heading = heading.strip_suffix('\r').unwrap_or(heading);
    let body = heading.strip_prefix("# PRD-").ok_or_else(|| {
        malformed(
            relative,
            "first nonblank content is not a PRD level-one heading",
        )
    })?;
    let (digits, title) = body
        .split_once(": ")
        .ok_or_else(|| malformed(relative, "heading must be '# PRD-<digits>: <title>'"))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed(relative, "heading has an invalid PRD identity"));
    }
    let heading_number: u64 = digits
        .parse()
        .map_err(|_| malformed(relative, "heading number overflows u64"))?;
    if heading_number != number {
        return Err(malformed(
            relative,
            format!("heading identity PRD-{heading_number} does not match filename PRD-{number}"),
        ));
    }
    if title.trim().is_empty() || title.contains(['\t', '\r', '\n']) {
        return Err(malformed(
            relative,
            "title is empty or contains forbidden whitespace",
        ));
    }
    let mut dependency_value = None;
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim_matches([' ', '\t']).is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("**") {
            if let Some((label, value)) = rest.split_once(":** ") {
                if label == "Depends on" && dependency_value.replace(value).is_some() {
                    return Err(malformed(relative, "repeated Depends on field"));
                }
                continue;
            }
        }
        if line.starts_with("**Depends on") {
            return Err(malformed(relative, "malformed Depends on field"));
        }
        break;
    }
    let mut dependencies = Vec::new();
    if let Some(value) = dependency_value {
        if value == "none" { /* empty */
        } else {
            for token in value.split(',') {
                let token = token.trim_matches([' ', '\t']);
                if token.is_empty() {
                    return Err(malformed(relative, "empty dependency list element"));
                }
                let digits = token
                    .strip_prefix("PRD-")
                    .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                    .ok_or_else(|| {
                        malformed(relative, format!("invalid dependency token '{token}'"))
                    })?;
                let dep = PrdId::new(digits.parse().map_err(|_| {
                    malformed(relative, format!("dependency '{token}' overflows u64"))
                })?);
                if dep.number() == number {
                    return Err(malformed(relative, format!("self-dependency {dep}")));
                }
                if dependencies.contains(&dep) {
                    return Err(malformed(relative, format!("duplicate dependency {dep}")));
                }
                dependencies.push(dep);
            }
            dependencies.sort();
        }
    }
    let hash = digest(&SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(DiscoveredPrd {
        id: PrdId::new(number),
        number,
        path: RepositoryPath::new(relative.to_owned())?,
        title: title.to_owned(),
        dependencies,
        content_hash: hash,
        location,
    })
}

fn malformed(path: &str, message: impl Into<String>) -> BacklogError {
    BacklogError::Malformed {
        path: path.into(),
        message: message.into(),
    }
}

pub fn validate_graph(prds: &[DiscoveredPrd]) -> Result<(), BacklogError> {
    let by_id: BTreeMap<_, _> = prds.iter().map(|p| (p.id.clone(), p)).collect();
    for prd in prds {
        for dependency in &prd.dependencies {
            if dependency == &prd.id {
                return Err(BacklogError::SelfDependency {
                    path: prd.path.clone(),
                    id: prd.id.clone(),
                });
            }
            if !by_id.contains_key(dependency) {
                return Err(BacklogError::MissingDependency {
                    path: prd.path.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }
    let mut state = BTreeMap::<PrdId, u8>::new();
    let mut stack = Vec::new();
    fn visit(
        id: &PrdId,
        by_id: &BTreeMap<PrdId, &DiscoveredPrd>,
        state: &mut BTreeMap<PrdId, u8>,
        stack: &mut Vec<PrdId>,
    ) -> Option<Vec<PrdId>> {
        state.insert(id.clone(), 1);
        stack.push(id.clone());
        for dep in &by_id[id].dependencies {
            match state.get(dep).copied().unwrap_or(0) {
                0 => {
                    if let Some(c) = visit(dep, by_id, state, stack) {
                        return Some(c);
                    }
                }
                1 => {
                    let pos = stack.iter().position(|v| v == dep).unwrap();
                    let mut cycle = stack[pos..].to_vec();
                    cycle.push(dep.clone());
                    return Some(cycle);
                }
                _ => {}
            }
        }
        stack.pop();
        state.insert(id.clone(), 2);
        None
    }
    for id in by_id.keys() {
        if state.get(id).copied().unwrap_or(0) == 0 {
            if let Some(cycle) = visit(id, &by_id, &mut state, &mut stack) {
                let mut members = cycle[..cycle.len() - 1].to_vec();
                let start = members
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, member)| *member)
                    .map(|(index, _)| index)
                    .unwrap();
                members.rotate_left(start);
                members.push(members[0].clone());
                return Err(BacklogError::Cycle(
                    members
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" -> "),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn recovery_attribution_requires_reason_and_human_completion_authority() {
        assert!(validate_recovery_attribution(
            BacklogRecoveryAction::Release,
            "ops:queue",
            "retry with review enabled"
        )
        .is_ok());
        assert!(matches!(
            validate_recovery_attribution(
                BacklogRecoveryAction::ManualCompleteOverride,
                "system:familiar-ai-run:1",
                "accepted"
            ),
            Err(BacklogStoreError::HumanAuthorityRequired)
        ));
        assert!(validate_recovery_attribution(
            BacklogRecoveryAction::ManualCompleteOverride,
            " human:alice ",
            " reviewed externally "
        )
        .is_ok());
        assert!(matches!(
            validate_recovery_attribution(BacklogRecoveryAction::Release, "alice", "  "),
            Err(BacklogStoreError::InvalidRecoveryReason)
        ));
        assert!(matches!(
            validate_recovery_attribution(
                BacklogRecoveryAction::RecordedComplete,
                "ops:queue",
                "merged before tracking existed"
            ),
            Err(BacklogStoreError::HumanAuthorityRequired)
        ));
        assert!(validate_recovery_attribution(
            BacklogRecoveryAction::RecordedComplete,
            "human:trollboy",
            "implemented, reviewed, and merged before this database existed"
        )
        .is_ok());
    }

    #[derive(Default)]
    struct MemoryStore {
        statuses: HashMap<String, BacklogStatus>,
    }
    impl BacklogStatusStore for MemoryStore {
        fn reconcile_and_snapshot(
            &mut self,
            _: &RepositoryIdentity,
            discovered: &[DiscoveredPrd],
        ) -> Result<Vec<BacklogEntry>, BacklogStoreError> {
            Ok(discovered
                .iter()
                .cloned()
                .map(|prd| {
                    let seed = if prd.location.is_archived() {
                        BacklogStatus::Completed
                    } else {
                        BacklogStatus::Pending
                    };
                    let status = *self.statuses.entry(prd.path.to_string()).or_insert(seed);
                    BacklogEntry { prd, status }
                })
                .collect())
        }
        fn transition(
            &mut self,
            _: &RepositoryIdentity,
            _: &RepositoryPath,
            _: BacklogStatus,
            _: BacklogStatus,
            _: &str,
        ) -> Result<BacklogEntry, BacklogStoreError> {
            unreachable!()
        }
    }
    fn write(root: &Path, name: &str, body: &str) {
        fs::write(root.join("docs/prds").join(name), body).unwrap();
    }
    #[test]
    fn run_admission_is_exact_and_dependency_aware() {
        let target = DiscoveredPrd {
            id: PrdId::new(2),
            number: 2,
            path: RepositoryPath::new("docs/prds/PRD-002.md").unwrap(),
            title: "Two".into(),
            dependencies: vec![PrdId::new(1)],
            content_hash: "two".into(),
            location: PrdLocation::Active,
        };
        let dependency = DiscoveredPrd {
            id: PrdId::new(1),
            number: 1,
            path: RepositoryPath::new("docs/prds/PRD-001.md").unwrap(),
            title: "One".into(),
            dependencies: vec![],
            content_hash: "one".into(),
            location: PrdLocation::Active,
        };
        let mut entries = vec![
            BacklogEntry {
                prd: dependency,
                status: BacklogStatus::Pending,
            },
            BacklogEntry {
                prd: target.clone(),
                status: BacklogStatus::Pending,
            },
        ];
        assert!(matches!(
            admit_run_prd(&entries, &target),
            Err(BacklogError::RunDependencies { .. })
        ));
        entries[0].status = BacklogStatus::Completed;
        assert!(admit_run_prd(&entries, &target).is_ok());
        entries[1].status = BacklogStatus::InProgress;
        assert!(matches!(
            admit_run_prd(&entries, &target),
            Err(BacklogError::RunStatus {
                status: "in_progress",
                ..
            })
        ));
    }
    #[test]
    fn discovery_parses_and_sorts_dependencies() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("docs/prds/done")).unwrap();
        write(
            root.path(),
            "PRD-010.md",
            "# PRD-10: Ten\n\n**Depends on:** PRD-9, PRD-002\n\ntext",
        );
        write(root.path(), "PRD-002.md", "# PRD-002: Two\n");
        write(root.path(), "PRD-9.md", "# PRD-009: Nine\n");
        write(root.path(), "vision.md", "ignored");
        fs::write(root.path().join("docs/prds/done/PRD-1.md"), "# PRD-1: Done").unwrap();
        let repo = RepositoryIdentity {
            worktree: root.path().into(),
            key: "key".into(),
        };
        let found = FilesystemBacklogDiscovery.discover(&repo).unwrap();
        // PRD-1 is archived, so it is discovered alongside the active PRDs and
        // carries its location; it is not selectable, but it is present.
        assert_eq!(
            found
                .iter()
                .map(|p| (p.number, p.location))
                .collect::<Vec<_>>(),
            vec![
                (1, PrdLocation::Archived),
                (2, PrdLocation::Active),
                (9, PrdLocation::Active),
                (10, PrdLocation::Active),
            ]
        );
        let found: Vec<_> = found.into_iter().filter(|p| p.number != 1).collect();
        assert_eq!(
            found.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![2, 9, 10]
        );
        assert_eq!(found[2].dependencies, vec![PrdId::new(2), PrdId::new(9)]);
    }
    #[test]
    fn manager_selects_lowest_eligible_number() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("docs/prds")).unwrap();
        write(root.path(), "PRD-010.md", "# PRD-10: Ten\n");
        write(root.path(), "PRD-009.md", "# PRD-9: Nine\n");
        struct Discovery(RepositoryIdentity);
        impl BacklogDiscovery for Discovery {
            fn resolve(&self, _: &Path) -> Result<RepositoryIdentity, BacklogError> {
                Ok(self.0.clone())
            }
            fn discover(&self, r: &RepositoryIdentity) -> Result<Vec<DiscoveredPrd>, BacklogError> {
                FilesystemBacklogDiscovery.discover(r)
            }
        }
        let mut manager = BacklogManager::new(
            Discovery(RepositoryIdentity {
                worktree: root.path().into(),
                key: "k".into(),
            }),
            MemoryStore::default(),
        );
        assert_eq!(manager.next(root.path()).unwrap().id, PrdId::new(9));
    }
    #[test]
    fn rejects_missing_dependency_and_cycle() {
        let p = |n, deps| DiscoveredPrd {
            id: PrdId::new(n),
            number: n,
            path: RepositoryPath::new(format!("docs/prds/PRD-{n}.md")).unwrap(),
            title: n.to_string(),
            dependencies: deps,
            content_hash: "x".into(),
            location: PrdLocation::Active,
        };
        assert!(matches!(
            validate_graph(&[p(1, vec![PrdId::new(2)])]),
            Err(BacklogError::MissingDependency { .. })
        ));
        assert!(matches!(
            validate_graph(&[p(1, vec![PrdId::new(2)]), p(2, vec![PrdId::new(1)])]),
            Err(BacklogError::Cycle(_))
        ));
    }

    #[test]
    fn parser_rejects_invalid_dependency_metadata() {
        let root = tempdir().unwrap();
        let cases = [
            "# PRD-1: One\n**Depends on:** none\n**Depends on:** none\n",
            "# PRD-1: One\n**Depends on:** PRD-2, \n",
            "# PRD-1: One\n**Depends on:** PRD-2, PRD-002\n",
            "# PRD-1: One\n**Depends on:** none, PRD-2\n",
            "# PRD-1: One\n**Depends on:**PRD-2\n",
        ];
        for body in cases {
            let path = root.path().join("PRD-001.md");
            fs::write(&path, body).unwrap();
            assert!(parse_candidate(
                &path,
                "docs/prds/PRD-001.md",
                "PRD-001.md",
                PrdLocation::Active
            )
            .is_err());
        }
    }

    fn archive(root: &Path, name: &str, body: &str) {
        let dir = root.join(ARCHIVED_PRD_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), body).unwrap();
    }
    fn repo_at(root: &Path) -> RepositoryIdentity {
        RepositoryIdentity {
            worktree: root.into(),
            key: "key".into(),
        }
    }

    #[test]
    fn archived_prds_resolve_as_dependencies_and_are_never_selected() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        // PRD-2 depends on archived PRD-1. Before this PRD that combination was
        // a hard MissingDependency; it must now validate and stay selectable.
        write(
            root.path(),
            "PRD-002.md",
            "# PRD-002: Two\n**Depends on:** PRD-1\n",
        );
        archive(root.path(), "PRD-001.md", "# PRD-001: One\n");
        let repo = repo_at(root.path());
        let found = FilesystemBacklogDiscovery.discover(&repo).unwrap();
        assert_eq!(found.len(), 2);
        validate_graph(&found).expect("dependency on archived PRD must resolve");

        struct Discovery(RepositoryIdentity);
        impl BacklogDiscovery for Discovery {
            fn resolve(&self, _: &Path) -> Result<RepositoryIdentity, BacklogError> {
                Ok(self.0.clone())
            }
            fn discover(&self, r: &RepositoryIdentity) -> Result<Vec<DiscoveredPrd>, BacklogError> {
                FilesystemBacklogDiscovery.discover(r)
            }
        }
        // The memory store reports everything pending, so if selection relied on
        // status alone it would return the archived PRD-1 first. It must not.
        let mut manager = BacklogManager::new(Discovery(repo), MemoryStore::default());
        assert_eq!(manager.next(root.path()).unwrap().id, PrdId::new(2));
    }

    #[test]
    fn admission_refuses_archived_work_by_exact_diagnostic() {
        let archived = DiscoveredPrd {
            id: PrdId::new(1),
            number: 1,
            path: RepositoryPath::new("docs/prds/done/PRD-001.md").unwrap(),
            title: "One".into(),
            dependencies: vec![],
            content_hash: "one".into(),
            location: PrdLocation::Archived,
        };
        let entries = vec![BacklogEntry {
            prd: archived.clone(),
            status: BacklogStatus::Completed,
        }];
        let error = admit_run_prd(&entries, &archived).unwrap_err();
        assert_eq!(
            error.to_string(),
            "cannot run docs/prds/done/PRD-001.md: PRD is archived and already completed"
        );
    }

    #[test]
    fn one_prd_in_both_locations_fails_closed_naming_both_paths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        write(root.path(), "PRD-001.md", "# PRD-001: One\n");
        archive(root.path(), "PRD-001.md", "# PRD-001: One\n");
        let error = FilesystemBacklogDiscovery
            .discover(&repo_at(root.path()))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "PRD PRD-1 is present in both locations: docs/prds/PRD-001.md, docs/prds/done/PRD-001.md"
        );
    }

    #[test]
    fn duplicate_identity_across_locations_fails_closed_naming_both_paths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        // Same identity, different files: a genuine numbering collision rather
        // than one PRD in two places, and diagnosed as such.
        write(root.path(), "PRD-1.md", "# PRD-1: One\n");
        archive(root.path(), "PRD-001.md", "# PRD-001: Other\n");
        let error = FilesystemBacklogDiscovery
            .discover(&repo_at(root.path()))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "duplicate PRD identity PRD-1: docs/prds/PRD-1.md, docs/prds/done/PRD-001.md"
        );
    }

    #[test]
    fn archived_directory_tolerates_what_the_active_directory_refuses() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        write(root.path(), "PRD-002.md", "# PRD-002: Two\n");
        // Wave-one naming: skipped by filename, never interpreted as wave two.
        archive(
            root.path(),
            "001-daemon-skeleton.md",
            "# Spec 1: Skeleton\n",
        );
        // Matching filename but a heading the grammar rejects: tolerated here,
        // fatal in the active directory.
        archive(root.path(), "PRD-003.md", "not a heading at all\n");
        // Nested directories below done/ are not read.
        let nested = root.path().join(ARCHIVED_PRD_DIR).join("done");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("PRD-004.md"), "# PRD-004: Nested\n").unwrap();
        let found = FilesystemBacklogDiscovery
            .discover(&repo_at(root.path()))
            .unwrap();
        assert_eq!(found.iter().map(|p| p.number).collect::<Vec<_>>(), vec![2]);

        write(root.path(), "PRD-005.md", "not a heading at all\n");
        assert!(matches!(
            FilesystemBacklogDiscovery.discover(&repo_at(root.path())),
            Err(BacklogError::Discovery(_))
        ));
    }

    #[test]
    fn a_repository_that_never_archived_behaves_exactly_as_before() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        write(root.path(), "PRD-001.md", "# PRD-001: One\n");
        // No done/ directory at all: absence is normal, not an error.
        let found = FilesystemBacklogDiscovery
            .discover(&repo_at(root.path()))
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].location, PrdLocation::Active);
    }

    #[test]
    fn location_is_recovered_from_a_stored_path() {
        let archived = RepositoryPath::new("docs/prds/done/PRD-001.md").unwrap();
        let active = RepositoryPath::new("docs/prds/PRD-001.md").unwrap();
        // A path merely starting with the same characters is not archived.
        let lookalike = RepositoryPath::new("docs/prds/done-later/PRD-001.md").unwrap();
        assert_eq!(
            PrdLocation::from_repository_path(&archived),
            PrdLocation::Archived
        );
        assert_eq!(
            PrdLocation::from_repository_path(&active),
            PrdLocation::Active
        );
        assert_eq!(
            PrdLocation::from_repository_path(&lookalike),
            PrdLocation::Active
        );
    }

    #[test]
    fn dependency_prose_after_metadata_block_is_not_parsed() {
        let root = tempdir().unwrap();
        let path = root.path().join("PRD-001.md");
        fs::write(&path, "# PRD-1: One\n\nProse.\n**Depends on:** PRD-999\n").unwrap();
        let parsed = parse_candidate(
            &path,
            "docs/prds/PRD-001.md",
            "PRD-001.md",
            PrdLocation::Active,
        )
        .unwrap();
        assert!(parsed.dependencies.is_empty());
    }
}
