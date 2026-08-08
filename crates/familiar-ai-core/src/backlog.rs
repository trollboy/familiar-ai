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

/// A PRD identity: a number plus an optional single lowercase letter marking an
/// epic child. The canonical grammar never produces a suffix, so its identity
/// space, ordering, display, and persistence are unchanged in every observable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrdId {
    number: u64,
    suffix: Option<char>,
}

impl PrdId {
    pub fn new(number: u64) -> Self {
        Self {
            number,
            suffix: None,
        }
    }
    /// Build a suffixed identity. Rejects anything but one ASCII lowercase
    /// letter, so an invalid suffix cannot enter the identity space at all.
    pub fn with_suffix(number: u64, suffix: char) -> Option<Self> {
        suffix.is_ascii_lowercase().then_some(Self {
            number,
            suffix: Some(suffix),
        })
    }
    pub fn number(&self) -> u64 {
        self.number
    }
    pub fn suffix(&self) -> Option<char> {
        self.suffix
    }
    /// The sort key realising the epic-aware total order: ascending by number,
    /// and within one number every suffixed identity strictly before the
    /// unsuffixed one. In the motivating convention an unsuffixed PRD sharing
    /// its number with suffixed siblings is the epic umbrella, completed after
    /// its children; selecting it first would execute a close-out document
    /// before the work it closes.
    fn sort_key(&self) -> (u64, u8, char) {
        match self.suffix {
            Some(letter) => (self.number, 0, letter),
            None => (self.number, 1, '\0'),
        }
    }
}

impl Ord for PrdId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl PartialOrd for PrdId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for PrdId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PRD-{}", self.number)?;
        match self.suffix {
            Some(letter) => write!(f, "{letter}"),
            None => Ok(()),
        }
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

/// A closed, named backlog grammar. Profiles are code, shipped and versioned
/// with Familiar: there is no user-defined grammar, no regex configuration, and
/// no content sniffing. The target repository cannot choose how it is parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BacklogProfileKind {
    /// `docs/prds/PRD-<digits>.md` with `# PRD-<digits>: <title>` headings and
    /// `**Depends on:**` dependency declarations.
    #[default]
    Canonical,
    /// `<digits><suffix?>-<slug>.md` with `# PRD <identity> — <title>` headings.
    /// Declares no dependencies: eligibility is identity order.
    NumberedSlug,
}

impl BacklogProfileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::NumberedSlug => "numbered-slug",
        }
    }
    /// Whether this grammar declares dependencies at all. Under `numbered-slug`
    /// the dependency set is empty *by grammar*: a `**Depends on:**` field in
    /// such a file is opaque content, not a declaration.
    pub fn declares_dependencies(self) -> bool {
        matches!(self, Self::Canonical)
    }
}

/// A profile bound to one repository's directories. Selection is operator
/// authority; a repository with no configured entry resolves to `canonical` at
/// the canonical locations, which is exactly today's behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogProfile {
    pub kind: BacklogProfileKind,
    pub active_dir: String,
    pub archived_dir: String,
}

impl Default for BacklogProfile {
    fn default() -> Self {
        Self {
            kind: BacklogProfileKind::Canonical,
            active_dir: ACTIVE_PRD_DIR.to_owned(),
            archived_dir: ARCHIVED_PRD_DIR.to_owned(),
        }
    }
}

impl BacklogProfile {
    /// Recover a location from a stored repository-relative path under this
    /// profile's directories.
    pub fn location_of(&self, path: &RepositoryPath) -> PrdLocation {
        if path
            .as_str()
            .starts_with(&format!("{}/", self.archived_dir))
        {
            PrdLocation::Archived
        } else {
            PrdLocation::Active
        }
    }
}

/// Why one identity was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacklogConflictKind {
    /// One PRD present in both the active and archived directories.
    DualLocation,
    /// Distinct PRDs claiming the same identity.
    DuplicateIdentity,
}

/// One refused identity, naming every path that claims it. A conflict removes
/// exactly that identity from the backlog: one ambiguous number must not hold
/// every unambiguous one hostage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogConflict {
    pub id: PrdId,
    pub kind: BacklogConflictKind,
    pub paths: Vec<RepositoryPath>,
}

impl fmt::Display for BacklogConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let paths = self
            .paths
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        match self.kind {
            BacklogConflictKind::DualLocation => write!(
                f,
                "PRD {} is present in both locations: {paths}",
                self.id.number()
            ),
            BacklogConflictKind::DuplicateIdentity => {
                write!(f, "duplicate PRD identity {}: {paths}", self.id)
            }
        }
    }
}

/// What discovery found: the usable backlog, plus every identity it refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BacklogDiscoveryOutcome {
    pub prds: Vec<DiscoveredPrd>,
    pub conflicts: Vec<BacklogConflict>,
}

impl BacklogDiscoveryOutcome {
    pub fn is_empty(&self) -> bool {
        self.prds.is_empty()
    }
    /// A single diagnostic naming every refused identity, for the callers that
    /// report conflicts alongside whatever work remains usable.
    pub fn conflict_report(&self) -> Option<String> {
        (!self.conflicts.is_empty()).then(|| {
            self.conflicts
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })
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
    /// The identity as the repository writes it, for human-facing output only.
    /// Internal identity remains numeric.
    pub display: String,
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
    /// The identity as the repository writes it, for human-facing output.
    pub display: String,
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
    fn discover(
        &self,
        repository: &RepositoryIdentity,
    ) -> Result<BacklogDiscoveryOutcome, BacklogError>;
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
        validate_graph(&discovered.prds)?;
        let mut entries = self
            .store
            .reconcile_and_snapshot(&repository, &discovered.prds)?;
        // Selection follows the domain order exactly: ascending by identity,
        // with epic children strictly before their umbrella.
        entries.sort_by(|a, b| {
            (&a.prd.id, a.prd.path.as_str().as_bytes())
                .cmp(&(&b.prd.id, b.prd.path.as_str().as_bytes()))
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
                        display: entry.prd.display,
                    })
                }
                BacklogStatus::Pending => IneligibilityReason::DependenciesIncomplete {
                    dependencies: incomplete,
                },
                BacklogStatus::InProgress => IneligibilityReason::StatusInProgress,
                BacklogStatus::Completed => IneligibilityReason::StatusCompleted,
                BacklogStatus::Blocked => IneligibilityReason::StatusBlocked,
            };
            reasons.push(format!("{}={}", entry.prd.display, reason_text(&reason)));
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

/// Filesystem discovery under one resolved profile. `Default` is `canonical` at
/// the canonical locations, so a repository nobody described behaves exactly as
/// it always has.
#[derive(Default)]
pub struct FilesystemBacklogDiscovery {
    profile: BacklogProfile,
}

impl FilesystemBacklogDiscovery {
    pub fn with_profile(profile: BacklogProfile) -> Self {
        Self { profile }
    }
    pub fn profile(&self) -> &BacklogProfile {
        &self.profile
    }
}

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
    ) -> Result<BacklogDiscoveryOutcome, BacklogError> {
        // The active directory must exist and every candidate in it must parse.
        // The archived directory is optional, and a candidate that does not
        // parse there is skipped rather than fatal: `done/` accumulates
        // documents from earlier naming conventions that no dependency
        // references, and refusing to run because of a finished document would
        // fail closed against history rather than against risk.
        let mut parsed = scan_directory(
            repository,
            &self.profile.active_dir,
            PrdLocation::Active,
            &self.profile,
        )?;
        parsed.extend(scan_directory(
            repository,
            &self.profile.archived_dir,
            PrdLocation::Archived,
            &self.profile,
        )?);
        parsed.sort_by(|a, b| {
            (&a.id, a.path.as_str().as_bytes()).cmp(&(&b.id, b.path.as_str().as_bytes()))
        });
        Ok(refuse_conflicting_identities(parsed))
    }
}

/// Partition discovered PRDs into the usable backlog and the identities that
/// must be refused. An identity claimed by more than one file is ambiguous, and
/// Familiar does not guess which copy is authoritative — but it refuses only
/// that identity, leaving every unambiguous one drivable.
fn refuse_conflicting_identities(parsed: Vec<DiscoveredPrd>) -> BacklogDiscoveryOutcome {
    let mut outcome = BacklogDiscoveryOutcome::default();
    let mut group: Vec<DiscoveredPrd> = Vec::new();
    let flush = |group: &mut Vec<DiscoveredPrd>, outcome: &mut BacklogDiscoveryOutcome| {
        match group.len() {
            0 => {}
            1 => outcome.prds.push(group.remove(0)),
            _ => {
                // One PRD in two places is a strictly more specific diagnosis
                // than two PRDs sharing a number, so name it as such.
                let names: Vec<_> = group.iter().map(|p| basename(p.path.as_str())).collect();
                let locations: Vec<_> = group.iter().map(|p| p.location).collect();
                let kind = if names.windows(2).all(|w| w[0] == w[1])
                    && locations.contains(&PrdLocation::Active)
                    && locations.contains(&PrdLocation::Archived)
                {
                    BacklogConflictKind::DualLocation
                } else {
                    BacklogConflictKind::DuplicateIdentity
                };
                outcome.conflicts.push(BacklogConflict {
                    id: group[0].id.clone(),
                    kind,
                    paths: group.iter().map(|p| p.path.clone()).collect(),
                });
                group.clear();
            }
        }
    };
    for prd in parsed {
        if group.first().is_some_and(|first| first.id != prd.id) {
            flush(&mut group, &mut outcome);
        }
        group.push(prd);
    }
    flush(&mut group, &mut outcome);
    outcome
}

fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Read one directory, one level deep, collecting the PRDs it declares.
fn scan_directory(
    repository: &RepositoryIdentity,
    relative_dir: &str,
    location: PrdLocation,
    profile: &BacklogProfile,
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
        if filename_identity(profile.kind, &name).is_some() {
            candidates.push((name, item.path()));
        }
    }
    candidates.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for (name, path) in candidates {
        let rel = format!("{relative_dir}/{name}");
        match parse_candidate(&path, &rel, &name, location, profile.kind) {
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

/// The identity a filename declares, retaining the digits exactly as written so
/// human-facing output can reproduce the repository's own zero-padding.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FilenameIdentity {
    digits: String,
    suffix: Option<char>,
}

impl FilenameIdentity {
    fn number(&self) -> Option<u64> {
        self.digits.parse().ok()
    }
}

/// Does this filename declare a PRD under `kind`? Returning `None` means "not a
/// candidate" (skipped silently); a candidate that later fails to parse is an
/// error in the active directory and tolerated in the archived one.
fn filename_identity(kind: BacklogProfileKind, name: &str) -> Option<FilenameIdentity> {
    match kind {
        BacklogProfileKind::Canonical => {
            let digits = name.strip_prefix("PRD-")?.strip_suffix(".md")?;
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| {
                FilenameIdentity {
                    digits: digits.to_owned(),
                    suffix: None,
                }
            })
        }
        BacklogProfileKind::NumberedSlug => {
            let stem = name.strip_suffix(".md")?;
            let (identity, slug) = stem.split_once('-')?;
            if slug.is_empty() {
                return None;
            }
            let (digits, suffix) = split_identity(identity)?;
            Some(FilenameIdentity { digits, suffix })
        }
    }
}

/// Split `0139a` into `("0139", Some('a'))`. One to six digits, at most one
/// trailing ASCII lowercase letter — a seven-digit number or a multi-letter
/// suffix is not an identity.
fn split_identity(value: &str) -> Option<(String, Option<char>)> {
    let mut chars = value.chars();
    let mut digits = String::new();
    let mut suffix = None;
    for c in chars.by_ref() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if c.is_ascii_lowercase() {
            suffix = Some(c);
            break;
        } else {
            return None;
        }
    }
    // Anything after the suffix letter disqualifies the identity.
    if chars.next().is_some() {
        return None;
    }
    (!digits.is_empty() && digits.len() <= 6).then_some((digits, suffix))
}

fn parse_candidate(
    path: &Path,
    relative: &str,
    name: &str,
    location: PrdLocation,
    kind: BacklogProfileKind,
) -> Result<DiscoveredPrd, BacklogError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| malformed(relative, format!("cannot inspect file: {e}")))?;
    if metadata.file_type().is_symlink() {
        return Err(malformed(relative, "matching symlinks are forbidden"));
    }
    if !metadata.is_file() {
        return Err(malformed(relative, "matching entry is not a regular file"));
    }
    let file_identity = filename_identity(kind, name).expect("candidate already matched");
    let number = file_identity
        .number()
        .ok_or_else(|| malformed(relative, "filename number overflows u64"))?;
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
    let title = parse_heading(kind, heading, &file_identity, number, relative)?;
    let id = identity_of(&file_identity, number, relative)?;
    let display = render_identity(kind, &file_identity);
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
    // Under a grammar that declares no dependencies the field is opaque
    // content, never a declaration, and eligibility is pure identity order.
    if !kind.declares_dependencies() {
        dependency_value = None;
    }
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
        id,
        number,
        path: RepositoryPath::new(relative.to_owned())?,
        title: title.to_owned(),
        dependencies,
        content_hash: hash,
        location,
        display,
    })
}

/// Build the identity a filename declares, refusing a suffix the identity space
/// cannot represent.
fn identity_of(
    file_identity: &FilenameIdentity,
    number: u64,
    relative: &str,
) -> Result<PrdId, BacklogError> {
    match file_identity.suffix {
        None => Ok(PrdId::new(number)),
        Some(letter) => PrdId::with_suffix(number, letter)
            .ok_or_else(|| malformed(relative, format!("invalid identity suffix '{letter}'"))),
    }
}

/// Render an identity the way the repository writes it, so an operator can grep
/// their own backlog with Familiar's output.
fn render_identity(kind: BacklogProfileKind, file_identity: &FilenameIdentity) -> String {
    let suffix = file_identity.suffix.map(String::from).unwrap_or_default();
    match kind {
        // Byte-identical to today: the number as written in the heading space,
        // never the filename's zero-padding.
        BacklogProfileKind::Canonical => {
            format!("PRD-{}{suffix}", file_identity.number().unwrap_or_default())
        }
        BacklogProfileKind::NumberedSlug => format!("PRD {}{suffix}", file_identity.digits),
    }
}

/// Validate the level-one heading under `kind` and return its title. The heading
/// must agree with the filename numerically and by suffix.
fn parse_heading<'a>(
    kind: BacklogProfileKind,
    heading: &'a str,
    file_identity: &FilenameIdentity,
    number: u64,
    relative: &str,
) -> Result<&'a str, BacklogError> {
    let (identity, title) = match kind {
        BacklogProfileKind::Canonical => {
            let body = heading.strip_prefix("# PRD-").ok_or_else(|| {
                malformed(
                    relative,
                    "first nonblank content is not a PRD level-one heading",
                )
            })?;
            body.split_once(": ")
                .ok_or_else(|| malformed(relative, "heading must be '# PRD-<digits>: <title>'"))?
        }
        BacklogProfileKind::NumberedSlug => {
            let body = heading.strip_prefix("# PRD ").ok_or_else(|| {
                malformed(
                    relative,
                    "first nonblank content is not a PRD level-one heading",
                )
            })?;
            // The separator is an em-dash or a colon, and nothing else.
            let split = body
                .split_once(" — ")
                .or_else(|| body.split_once(": "))
                .ok_or_else(|| {
                    malformed(
                        relative,
                        "heading must be '# PRD <identity> — <title>' or '# PRD <identity>: <title>'",
                    )
                })?;
            split
        }
    };
    let (heading_digits, heading_suffix) = match kind {
        BacklogProfileKind::Canonical => {
            if identity.is_empty() || !identity.bytes().all(|b| b.is_ascii_digit()) {
                return Err(malformed(relative, "heading has an invalid PRD identity"));
            }
            (identity.to_owned(), None)
        }
        BacklogProfileKind::NumberedSlug => split_identity(identity)
            .ok_or_else(|| malformed(relative, "heading has an invalid PRD identity"))?,
    };
    let heading_number: u64 = heading_digits
        .parse()
        .map_err(|_| malformed(relative, "heading number overflows u64"))?;
    if heading_number != number || heading_suffix != file_identity.suffix {
        let written = |digits: &str, suffix: Option<char>| {
            format!("{digits}{}", suffix.map(String::from).unwrap_or_default())
        };
        return Err(malformed(
            relative,
            format!(
                "heading identity {} does not match filename identity {}",
                written(&heading_digits, heading_suffix),
                written(&file_identity.digits, file_identity.suffix)
            ),
        ));
    }
    if title.trim().is_empty() || title.contains(['\t', '\r', '\n']) {
        return Err(malformed(
            relative,
            "title is empty or contains forbidden whitespace",
        ));
    }
    Ok(title)
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
            display: String::new(),
        };
        let dependency = DiscoveredPrd {
            id: PrdId::new(1),
            number: 1,
            path: RepositoryPath::new("docs/prds/PRD-001.md").unwrap(),
            title: "One".into(),
            dependencies: vec![],
            content_hash: "one".into(),
            location: PrdLocation::Active,
            display: String::new(),
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
        let found = FilesystemBacklogDiscovery::default()
            .discover(&repo)
            .unwrap()
            .prds;
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
            fn discover(
                &self,
                r: &RepositoryIdentity,
            ) -> Result<BacklogDiscoveryOutcome, BacklogError> {
                FilesystemBacklogDiscovery::default().discover(r)
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
            display: String::new(),
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
                PrdLocation::Active,
                BacklogProfileKind::Canonical
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
        let found = FilesystemBacklogDiscovery::default()
            .discover(&repo)
            .unwrap()
            .prds;
        assert_eq!(found.len(), 2);
        validate_graph(&found).expect("dependency on archived PRD must resolve");

        struct Discovery(RepositoryIdentity);
        impl BacklogDiscovery for Discovery {
            fn resolve(&self, _: &Path) -> Result<RepositoryIdentity, BacklogError> {
                Ok(self.0.clone())
            }
            fn discover(
                &self,
                r: &RepositoryIdentity,
            ) -> Result<BacklogDiscoveryOutcome, BacklogError> {
                FilesystemBacklogDiscovery::default().discover(r)
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
            display: String::new(),
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
    fn one_prd_in_both_locations_is_refused_naming_both_paths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        write(root.path(), "PRD-001.md", "# PRD-001: One\n");
        archive(root.path(), "PRD-001.md", "# PRD-001: One\n");
        // An unrelated PRD must survive the conflict: one ambiguous identity
        // does not hold an unambiguous one hostage.
        write(root.path(), "PRD-002.md", "# PRD-002: Two\n");
        let outcome = FilesystemBacklogDiscovery::default()
            .discover(&repo_at(root.path()))
            .unwrap();
        assert_eq!(
            outcome.prds.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            outcome.conflict_report().unwrap(),
            "PRD 1 is present in both locations: docs/prds/PRD-001.md, docs/prds/done/PRD-001.md"
        );
        assert_eq!(outcome.conflicts[0].kind, BacklogConflictKind::DualLocation);
    }

    #[test]
    fn duplicate_identity_across_locations_is_refused_naming_both_paths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        // Same identity, different files: a genuine numbering collision rather
        // than one PRD in two places, and diagnosed as such.
        write(root.path(), "PRD-1.md", "# PRD-1: One\n");
        archive(root.path(), "PRD-001.md", "# PRD-001: Other\n");
        let outcome = FilesystemBacklogDiscovery::default()
            .discover(&repo_at(root.path()))
            .unwrap();
        assert!(outcome.prds.is_empty());
        assert_eq!(
            outcome.conflict_report().unwrap(),
            "duplicate PRD identity PRD-1: docs/prds/PRD-1.md, docs/prds/done/PRD-001.md"
        );
        assert_eq!(
            outcome.conflicts[0].kind,
            BacklogConflictKind::DuplicateIdentity
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
        let found = FilesystemBacklogDiscovery::default()
            .discover(&repo_at(root.path()))
            .unwrap()
            .prds;
        assert_eq!(found.iter().map(|p| p.number).collect::<Vec<_>>(), vec![2]);

        write(root.path(), "PRD-005.md", "not a heading at all\n");
        assert!(matches!(
            FilesystemBacklogDiscovery::default().discover(&repo_at(root.path())),
            Err(BacklogError::Discovery(_))
        ));
    }

    #[test]
    fn a_repository_that_never_archived_behaves_exactly_as_before() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join(ACTIVE_PRD_DIR)).unwrap();
        write(root.path(), "PRD-001.md", "# PRD-001: One\n");
        // No done/ directory at all: absence is normal, not an error.
        let found = FilesystemBacklogDiscovery::default()
            .discover(&repo_at(root.path()))
            .unwrap()
            .prds;
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

    fn numbered_slug_repo(root: &Path) -> RepositoryIdentity {
        fs::create_dir_all(root.join("docs/prd/todo")).unwrap();
        fs::create_dir_all(root.join("docs/prd/done")).unwrap();
        RepositoryIdentity {
            worktree: root.into(),
            key: "spectra".into(),
        }
    }
    fn numbered_slug() -> BacklogProfile {
        BacklogProfile {
            kind: BacklogProfileKind::NumberedSlug,
            active_dir: "docs/prd/todo".into(),
            archived_dir: "docs/prd/done".into(),
        }
    }
    fn todo(root: &Path, name: &str, body: &str) {
        fs::write(root.join("docs/prd/todo").join(name), body).unwrap();
    }

    /// An epic umbrella is completed *after* its children, so selecting it first
    /// would execute a close-out document before the work it closes.
    #[test]
    fn epic_children_order_strictly_before_their_umbrella() {
        let ids = [
            PrdId::with_suffix(139, 'a').unwrap(),
            PrdId::with_suffix(139, 'b').unwrap(),
            PrdId::new(139),
            PrdId::new(140),
        ];
        let mut shuffled = vec![
            ids[3].clone(),
            ids[2].clone(),
            ids[1].clone(),
            ids[0].clone(),
        ];
        shuffled.sort();
        assert_eq!(shuffled, ids.to_vec());
        assert!(ids[0] < ids[1] && ids[1] < ids[2] && ids[2] < ids[3]);
        // A suffix outside one ASCII lowercase letter is not an identity.
        assert!(PrdId::with_suffix(139, 'A').is_none());
        assert!(PrdId::with_suffix(139, '1').is_none());
    }

    #[test]
    fn numbered_slug_discovers_orders_and_renders_the_repositorys_own_spelling() {
        let root = tempdir().unwrap();
        let repo = numbered_slug_repo(root.path());
        todo(
            root.path(),
            "0140-endpoint-posture.md",
            "# PRD 0140 — Posture\n",
        );
        todo(
            root.path(),
            "0139-device-bound-keys.md",
            "# PRD 0139 — Umbrella\n",
        );
        todo(
            root.path(),
            "0139b-apple-attestation.md",
            "# PRD 0139b — Apple\n",
        );
        todo(
            root.path(),
            "0139a-native-keystore.md",
            "# PRD 0139a — Keystore\n",
        );
        let outcome = FilesystemBacklogDiscovery::with_profile(numbered_slug())
            .discover(&repo)
            .unwrap();
        assert_eq!(
            outcome
                .prds
                .iter()
                .map(|p| p.display.as_str())
                .collect::<Vec<_>>(),
            vec!["PRD 0139a", "PRD 0139b", "PRD 0139", "PRD 0140"]
        );
        // Zero-padding is taken from the filename, so an operator can grep their
        // own backlog with Familiar's output.
        assert_eq!(outcome.prds[0].id, PrdId::with_suffix(139, 'a').unwrap());
        assert_eq!(outcome.prds[2].id, PrdId::new(139));
    }

    /// Under a grammar that declares no dependencies, a `**Depends on:**` field
    /// is opaque content and eligibility is pure identity order.
    #[test]
    fn numbered_slug_never_parses_dependencies() {
        let root = tempdir().unwrap();
        let repo = numbered_slug_repo(root.path());
        todo(
            root.path(),
            "0002-second.md",
            "# PRD 0002 — Second\n**Depends on:** PRD-9999\n",
        );
        let outcome = FilesystemBacklogDiscovery::with_profile(numbered_slug())
            .discover(&repo)
            .unwrap();
        assert!(outcome.prds[0].dependencies.is_empty());
        // A dependency that would have been missing cannot fail the graph,
        // because the grammar never declared one.
        validate_graph(&outcome.prds).unwrap();
    }

    #[test]
    fn numbered_slug_grammar_refuses_exactly_what_it_should() {
        let root = tempdir().unwrap();
        let repo = numbered_slug_repo(root.path());
        let dir = root.path().join("docs/prd/todo");
        let discover_one = |name: &str, body: &str| {
            for existing in fs::read_dir(&dir).unwrap() {
                fs::remove_file(existing.unwrap().path()).unwrap();
            }
            fs::write(dir.join(name), body).unwrap();
            FilesystemBacklogDiscovery::with_profile(numbered_slug()).discover(&repo)
        };

        // Content-level violations are candidates that fail closed, each pinned
        // to its exact diagnostic.
        let content_cases = [
            (
                "0001-a.md",
                "# PRD 0002 — Mismatch\n",
                "malformed PRD docs/prd/todo/0001-a.md: heading identity 0002 does not match filename identity 0001",
            ),
            (
                "0001a-a.md",
                "# PRD 0001 — Suffix dropped\n",
                "malformed PRD docs/prd/todo/0001a-a.md: heading identity 0001 does not match filename identity 0001a",
            ),
            (
                "0001-a.md",
                "# PRD 0001 Missing separator\n",
                "malformed PRD docs/prd/todo/0001-a.md: heading must be '# PRD <identity> — <title>' or '# PRD <identity>: <title>'",
            ),
            (
                "0001-a.md",
                "# PRD 0001 — \n",
                "malformed PRD docs/prd/todo/0001-a.md: title is empty or contains forbidden whitespace",
            ),
        ];
        for (name, body, expected) in content_cases {
            match discover_one(name, body) {
                Err(BacklogError::Discovery(message)) => assert_eq!(message, expected),
                other => panic!("{name}: expected a closed refusal, got {other:?}"),
            }
        }

        // Filename-level violations are not candidates at all: a seven-digit
        // number, a multi-letter suffix, and an empty slug are not identities.
        for (name, body) in [
            ("1234567-a.md", "# PRD 1234567 — Too long\n"),
            ("0001ab-a.md", "# PRD 0001ab — Multi letter\n"),
            ("0001-.md", "# PRD 0001 — Empty slug\n"),
        ] {
            let found = discover_one(name, body).unwrap();
            assert!(found.prds.is_empty(), "{name} was accepted");
            assert!(found.conflicts.is_empty());
        }

        // A colon separator is equally valid, and six digits is the limit.
        let accepted = discover_one("123456-ok.md", "# PRD 123456: Colon form\n").unwrap();
        assert_eq!(accepted.prds.len(), 1);
        assert_eq!(accepted.prds[0].title, "Colon form");
    }

    /// PRD-023's location semantics transfer whole to a different convention.
    #[test]
    fn numbered_slug_keeps_location_as_truth() {
        let root = tempdir().unwrap();
        let repo = numbered_slug_repo(root.path());
        todo(root.path(), "0021-remaining.md", "# PRD 0021 — Remaining\n");
        fs::write(
            root.path().join("docs/prd/done/0020-finished.md"),
            "# PRD 0020 — Finished\n",
        )
        .unwrap();
        // A third-generation document in done/ is tolerated, exactly as PRD-023
        // tolerates wave-one documents.
        fs::write(
            root.path().join("docs/prd/done/0001-scaffold.md"),
            "# Spec 1: Project Scaffold\n",
        )
        .unwrap();
        let outcome = FilesystemBacklogDiscovery::with_profile(numbered_slug())
            .discover(&repo)
            .unwrap();
        assert_eq!(
            outcome
                .prds
                .iter()
                .map(|p| (p.number, p.location))
                .collect::<Vec<_>>(),
            vec![(20, PrdLocation::Archived), (21, PrdLocation::Active)]
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
            BacklogProfileKind::Canonical,
        )
        .unwrap();
        assert!(parsed.dependencies.is_empty());
    }
}
