use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use ulid::Ulid;
use vulcan_app::sync::{GitRefName, GitRemote};

const DAEMON_CONFIG_FILE: &str = "daemon.toml";
const DEFAULT_BIND: &str = "127.0.0.1:3210";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WikiId(String);

impl WikiId {
    pub fn parse(value: impl Into<String>) -> Result<Self, RegistryError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value.bytes().enumerate().all(|(index, byte)| match byte {
                b'a'..=b'z' | b'0'..=b'9' => true,
                b'-' | b'_' => index > 0,
                _ => false,
            });
        if !valid {
            return Err(RegistryError::InvalidWikiId(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for WikiId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiRegistration {
    pub id: WikiId,
    pub registration_id: Ulid,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_profile: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub sync_paused: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub device_id: Ulid,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_agent: Option<DaemonAgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_agent: Option<DaemonAgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_worker: Option<DaemonSemanticWorkerConfig>,
    #[serde(default, rename = "vault")]
    pub vaults: Vec<WikiRegistration>,
}

fn default_bind() -> String {
    DEFAULT_BIND.to_string()
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            device_id: Ulid::new(),
            bind: default_bind(),
            resolution_agent: None,
            semantic_agent: None,
            semantic_worker: None,
            vaults: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonAgentConfig {
    pub base_url: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonSemanticWorkerConfig {
    pub wikis: Vec<WikiId>,
    #[serde(default = "default_semantic_ref")]
    pub semantic_ref: String,
    #[serde(default = "default_sync_remote")]
    pub remote: String,
    #[serde(default = "default_sync_live_ref")]
    pub live_ref: String,
    #[serde(default = "default_true")]
    pub publish: bool,
    #[serde(default = "default_semantic_quiet_seconds")]
    pub quiet_seconds: u64,
    #[serde(default = "default_semantic_maximum_wait_seconds")]
    pub maximum_wait_seconds: u64,
    #[serde(default = "default_semantic_poll_seconds")]
    pub poll_seconds: u64,
}

fn default_semantic_ref() -> String {
    "refs/heads/main".to_string()
}

fn default_sync_remote() -> String {
    "origin".to_string()
}

fn default_sync_live_ref() -> String {
    "refs/heads/__vulcan-sync/live".to_string()
}

const fn default_true() -> bool {
    true
}

const fn default_semantic_quiet_seconds() -> u64 {
    900
}

const fn default_semantic_maximum_wait_seconds() -> u64 {
    21_600
}

const fn default_semantic_poll_seconds() -> u64 {
    30
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonAgentKind {
    Resolution,
    Semantic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddWikiRequest {
    pub id: WikiId,
    pub path: PathBuf,
    pub groups: Vec<String>,
    pub git_dir: Option<PathBuf>,
    pub permissions_profile: Option<String>,
    pub sync_backend: Option<String>,
    pub platform_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateWikiRequest {
    pub groups_to_add: Vec<String>,
    pub groups_to_remove: Vec<String>,
    pub permissions_profile: Option<Option<String>>,
    pub sync_paused: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WikiRegistrationStatus {
    #[serde(flatten)]
    pub registration: WikiRegistration,
    pub available: bool,
    pub indexed: bool,
    pub git_repository: bool,
}

impl WikiRegistrationStatus {
    #[must_use]
    pub fn from_registration(registration: &WikiRegistration) -> Self {
        let available = registration.path.is_dir();
        let indexed = registration.path.join(".vulcan/cache.db").is_file();
        let git_repository = registration
            .git_dir
            .as_ref()
            .is_some_and(|path| path.is_dir())
            || registration.path.join(".git").exists();
        Self {
            registration: registration.clone(),
            available,
            indexed,
            git_repository,
        }
    }
}

#[derive(Debug)]
pub enum RegistryError {
    ConfigDirectoryUnavailable,
    InvalidWikiId(String),
    InvalidGroup(String),
    InvalidDaemonSetting(String),
    MissingDirectory(PathBuf),
    DuplicateId(WikiId),
    DuplicatePath { id: WikiId, path: PathBuf },
    DuplicateGitDir { id: WikiId, path: PathBuf },
    UnregisteredPath(PathBuf),
    UnknownWiki(WikiId),
    InvalidConfig { path: PathBuf, detail: String },
    Io(std::io::Error),
}

impl Display for RegistryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfigDirectoryUnavailable => formatter.write_str(
                "cannot determine the Vulcan user config directory; set XDG_CONFIG_HOME or HOME",
            ),
            Self::InvalidWikiId(id) => write!(
                formatter,
                "invalid wiki ID `{id}`; use 1-64 lowercase ASCII letters, digits, `-`, or `_`, starting with a letter or digit"
            ),
            Self::InvalidGroup(group) => write!(
                formatter,
                "invalid wiki group `{group}`; group names use the same syntax as wiki IDs"
            ),
            Self::InvalidDaemonSetting(detail) => {
                write!(formatter, "invalid daemon setting: {detail}")
            }
            Self::MissingDirectory(path) => {
                write!(formatter, "wiki directory does not exist: {}", path.display())
            }
            Self::DuplicateId(id) => write!(formatter, "wiki `{id}` is already registered"),
            Self::DuplicatePath { id, path } => write!(
                formatter,
                "{} is already registered as wiki `{id}`",
                path.display()
            ),
            Self::DuplicateGitDir { id, path } => write!(
                formatter,
                "Git directory {} is already registered for wiki `{id}`",
                path.display()
            ),
            Self::UnregisteredPath(path) => write!(
                formatter,
                "vault path {} is not registered on this device",
                path.display()
            ),
            Self::UnknownWiki(id) => write!(formatter, "unknown registered wiki `{id}`"),
            Self::InvalidConfig { path, detail } => {
                write!(formatter, "invalid registry {}: {detail}", path.display())
            }
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for RegistryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RegistryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone)]
pub struct WikiRegistry {
    path: PathBuf,
}

impl WikiRegistry {
    pub fn user_default() -> Result<Self, RegistryError> {
        let directory = vulcan_core::vulcan_user_config_dir()
            .ok_or(RegistryError::ConfigDirectoryUnavailable)?;
        Ok(Self::at(directory.join(DAEMON_CONFIG_FILE)))
    }

    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<DaemonConfig, RegistryError> {
        let config = load_config(&self.path)?;
        validate_daemon_config(&config)?;
        Ok(config)
    }

    pub fn list(&self, group: Option<&str>) -> Result<Vec<WikiRegistrationStatus>, RegistryError> {
        if let Some(group) = group {
            validate_groups(&[group.to_string()])?;
        }
        Ok(self
            .load()?
            .vaults
            .iter()
            .filter(|wiki| group.is_none_or(|group| wiki.groups.iter().any(|item| item == group)))
            .map(WikiRegistrationStatus::from_registration)
            .collect())
    }

    pub fn show(&self, id: &WikiId) -> Result<WikiRegistrationStatus, RegistryError> {
        self.load()?
            .vaults
            .iter()
            .find(|wiki| &wiki.id == id)
            .map(WikiRegistrationStatus::from_registration)
            .ok_or_else(|| RegistryError::UnknownWiki(id.clone()))
    }

    pub fn find_by_path(&self, path: &Path) -> Result<WikiRegistration, RegistryError> {
        let path = canonical_directory(path)?;
        self.load()?
            .vaults
            .into_iter()
            .find(|wiki| wiki.path == path)
            .ok_or(RegistryError::UnregisteredPath(path))
    }

    pub fn add(
        &self,
        request: &AddWikiRequest,
        dry_run: bool,
    ) -> Result<WikiRegistration, RegistryError> {
        self.mutate(dry_run, |config| {
            validate_groups(&request.groups)?;
            let path = canonical_directory(&request.path)?;
            let git_dir = request
                .git_dir
                .as_deref()
                .map(canonical_directory)
                .transpose()?;
            if config.vaults.iter().any(|wiki| wiki.id == request.id) {
                return Err(RegistryError::DuplicateId(request.id.clone()));
            }
            if let Some(existing) = config.vaults.iter().find(|wiki| wiki.path == path) {
                return Err(RegistryError::DuplicatePath {
                    id: existing.id.clone(),
                    path,
                });
            }
            if let Some((existing, git_dir)) = git_dir.as_ref().and_then(|git_dir| {
                config
                    .vaults
                    .iter()
                    .find(|wiki| wiki.git_dir.as_ref() == Some(git_dir))
                    .map(|wiki| (wiki, git_dir))
            }) {
                return Err(RegistryError::DuplicateGitDir {
                    id: existing.id.clone(),
                    path: git_dir.clone(),
                });
            }
            let mut groups = request.groups.clone();
            groups.sort();
            groups.dedup();
            let registration = WikiRegistration {
                id: request.id.clone(),
                registration_id: Ulid::new(),
                path,
                groups,
                git_dir,
                permissions_profile: request.permissions_profile.clone(),
                sync_backend: request.sync_backend.clone(),
                platform_profile: request.platform_profile.clone(),
                sync_paused: false,
            };
            config.vaults.push(registration.clone());
            config.vaults.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(registration)
        })
    }

    pub fn update(
        &self,
        id: &WikiId,
        request: &UpdateWikiRequest,
        dry_run: bool,
    ) -> Result<WikiRegistration, RegistryError> {
        self.mutate(dry_run, |config| {
            validate_groups(&request.groups_to_add)?;
            validate_groups(&request.groups_to_remove)?;
            let wiki = config
                .vaults
                .iter_mut()
                .find(|wiki| &wiki.id == id)
                .ok_or_else(|| RegistryError::UnknownWiki(id.clone()))?;
            let mut groups = wiki.groups.iter().cloned().collect::<BTreeSet<_>>();
            groups.extend(request.groups_to_add.iter().cloned());
            for group in &request.groups_to_remove {
                groups.remove(group);
            }
            wiki.groups = groups.into_iter().collect();
            if let Some(profile) = &request.permissions_profile {
                wiki.permissions_profile.clone_from(profile);
            }
            if let Some(paused) = request.sync_paused {
                wiki.sync_paused = paused;
            }
            Ok(wiki.clone())
        })
    }

    pub fn remove(&self, id: &WikiId, dry_run: bool) -> Result<WikiRegistration, RegistryError> {
        self.mutate(dry_run, |config| {
            let index = config
                .vaults
                .iter()
                .position(|wiki| &wiki.id == id)
                .ok_or_else(|| RegistryError::UnknownWiki(id.clone()))?;
            Ok(config.vaults.remove(index))
        })
    }

    pub fn set_bind(&self, bind: &str, dry_run: bool) -> Result<DaemonConfig, RegistryError> {
        self.mutate(dry_run, |config| {
            validate_bind(bind)?;
            config.bind = bind.to_string();
            Ok(config.clone())
        })
    }

    pub fn set_agent(
        &self,
        kind: DaemonAgentKind,
        agent: DaemonAgentConfig,
        dry_run: bool,
    ) -> Result<DaemonConfig, RegistryError> {
        self.mutate(dry_run, |config| {
            validate_agent_config(&agent)?;
            match kind {
                DaemonAgentKind::Resolution => config.resolution_agent = Some(agent),
                DaemonAgentKind::Semantic => config.semantic_agent = Some(agent),
            }
            Ok(config.clone())
        })
    }

    pub fn clear_agent(
        &self,
        kind: DaemonAgentKind,
        dry_run: bool,
    ) -> Result<DaemonConfig, RegistryError> {
        self.mutate(dry_run, |config| {
            match kind {
                DaemonAgentKind::Resolution => config.resolution_agent = None,
                DaemonAgentKind::Semantic => config.semantic_agent = None,
            }
            Ok(config.clone())
        })
    }

    pub fn set_semantic_worker(
        &self,
        worker: DaemonSemanticWorkerConfig,
        dry_run: bool,
    ) -> Result<DaemonConfig, RegistryError> {
        self.mutate(dry_run, |config| {
            validate_semantic_worker_config(&worker)?;
            config.semantic_worker = Some(worker);
            Ok(config.clone())
        })
    }

    pub fn clear_semantic_worker(&self, dry_run: bool) -> Result<DaemonConfig, RegistryError> {
        self.mutate(dry_run, |config| {
            config.semantic_worker = None;
            Ok(config.clone())
        })
    }

    fn mutate<T>(
        &self,
        dry_run: bool,
        operation: impl FnOnce(&mut DaemonConfig) -> Result<T, RegistryError>,
    ) -> Result<T, RegistryError> {
        let _lock = RegistryLock::acquire(&self.path)?;
        let mut config = self.load()?;
        let result = operation(&mut config)?;
        if !dry_run {
            save_config(&self.path, &config)?;
        }
        Ok(result)
    }
}

fn validate_daemon_config(config: &DaemonConfig) -> Result<(), RegistryError> {
    validate_bind(&config.bind)?;
    if let Some(agent) = &config.resolution_agent {
        validate_agent_config(agent)?;
    }
    if let Some(agent) = &config.semantic_agent {
        validate_agent_config(agent)?;
    }
    if let Some(worker) = &config.semantic_worker {
        validate_semantic_worker_config(worker)?;
    }
    Ok(())
}

fn validate_semantic_worker_config(
    worker: &DaemonSemanticWorkerConfig,
) -> Result<(), RegistryError> {
    if worker.wikis.is_empty() {
        return Err(RegistryError::InvalidDaemonSetting(
            "semantic worker requires at least one explicit wiki".to_string(),
        ));
    }
    let unique = worker.wikis.iter().collect::<BTreeSet<_>>();
    if unique.len() != worker.wikis.len() {
        return Err(RegistryError::InvalidDaemonSetting(
            "semantic worker wiki IDs must be unique".to_string(),
        ));
    }
    GitRefName::parse(worker.semantic_ref.clone()).map_err(|error| {
        RegistryError::InvalidDaemonSetting(format!("invalid semantic ref: {error}"))
    })?;
    GitRemote::parse(worker.remote.clone()).map_err(|error| {
        RegistryError::InvalidDaemonSetting(format!("invalid semantic worker remote: {error}"))
    })?;
    GitRefName::parse(worker.live_ref.clone()).map_err(|error| {
        RegistryError::InvalidDaemonSetting(format!("invalid semantic worker live ref: {error}"))
    })?;
    if worker.maximum_wait_seconds == 0 {
        return Err(RegistryError::InvalidDaemonSetting(
            "semantic worker maximum wait must be at least one second".to_string(),
        ));
    }
    if !(1..=3_600).contains(&worker.poll_seconds) {
        return Err(RegistryError::InvalidDaemonSetting(
            "semantic worker poll interval must be between 1 and 3600 seconds".to_string(),
        ));
    }
    Ok(())
}

fn validate_bind(bind: &str) -> Result<(), RegistryError> {
    let address = bind.parse::<SocketAddr>().map_err(|error| {
        RegistryError::InvalidDaemonSetting(format!("bind address `{bind}` is invalid: {error}"))
    })?;
    if !address.ip().is_loopback() {
        return Err(RegistryError::InvalidDaemonSetting(format!(
            "bind address must be loopback, got `{address}`"
        )));
    }
    Ok(())
}

fn validate_agent_config(agent: &DaemonAgentConfig) -> Result<(), RegistryError> {
    let base_url = agent.base_url.as_str();
    let scheme_length = if base_url.starts_with("https://") {
        8
    } else if base_url.starts_with("http://") {
        7
    } else {
        return Err(RegistryError::InvalidDaemonSetting(
            "agent base URL must use http:// or https://".to_string(),
        ));
    };
    if base_url.len() > 2048
        || base_url[scheme_length..]
            .split('/')
            .next()
            .is_none_or(|authority| authority.is_empty() || authority.contains('@'))
        || base_url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(RegistryError::InvalidDaemonSetting(
            "agent base URL must be bounded, have a host, and contain no credentials or whitespace"
                .to_string(),
        ));
    }
    if agent.model.is_empty()
        || agent.model.len() > 256
        || agent.model.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(RegistryError::InvalidDaemonSetting(
            "agent model must contain 1-256 non-control bytes".to_string(),
        ));
    }
    if let Some(name) = &agent.api_key_env {
        let valid = !name.is_empty()
            && name.len() <= 128
            && name.bytes().enumerate().all(|(index, byte)| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'_' => true,
                b'0'..=b'9' => index > 0,
                _ => false,
            });
        if !valid {
            return Err(RegistryError::InvalidDaemonSetting(format!(
                "agent API-key environment variable `{name}` is invalid"
            )));
        }
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, RegistryError> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RegistryError::MissingDirectory(path.to_path_buf())
        } else {
            RegistryError::Io(error)
        }
    })?;
    if !canonical.is_dir() {
        return Err(RegistryError::MissingDirectory(path.to_path_buf()));
    }
    Ok(canonical)
}

fn validate_groups(groups: &[String]) -> Result<(), RegistryError> {
    for group in groups {
        WikiId::parse(group.clone()).map_err(|_| RegistryError::InvalidGroup(group.clone()))?;
    }
    Ok(())
}

fn load_config(path: &Path) -> Result<DaemonConfig, RegistryError> {
    match fs::read_to_string(path) {
        Ok(source) => toml::from_str(&source).map_err(|error| RegistryError::InvalidConfig {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DaemonConfig::default()),
        Err(error) => Err(RegistryError::Io(error)),
    }
}

fn save_config(path: &Path, config: &DaemonConfig) -> Result<(), RegistryError> {
    let parent = path.parent().ok_or_else(|| RegistryError::InvalidConfig {
        path: path.to_path_buf(),
        detail: "registry path has no parent directory".to_string(),
    })?;
    fs::create_dir_all(parent)?;
    let rendered =
        toml::to_string_pretty(config).map_err(|error| RegistryError::InvalidConfig {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temporary, rendered.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| RegistryError::Io(error.error))?;
    Ok(())
}

struct RegistryLock {
    _file: File,
}

impl RegistryLock {
    fn acquire(config_path: &Path) -> Result<Self, RegistryError> {
        let parent = config_path
            .parent()
            .ok_or_else(|| RegistryError::InvalidConfig {
                path: config_path.to_path_buf(),
                detail: "registry path has no parent directory".to_string(),
            })?;
        fs::create_dir_all(parent)?;
        let lock_path = parent.join("daemon.toml.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn request(id: &str, path: &Path) -> AddWikiRequest {
        AddWikiRequest {
            id: WikiId::parse(id).expect("valid ID"),
            path: path.to_path_buf(),
            groups: vec!["zeta".to_string(), "daily".to_string(), "daily".to_string()],
            git_dir: None,
            permissions_profile: None,
            sync_backend: Some("git".to_string()),
            platform_profile: None,
        }
    }

    #[test]
    fn wiki_ids_are_url_safe_and_bounded() {
        for valid in ["personal", "work-2", "notes_2026", "a"] {
            assert_eq!(WikiId::parse(valid).expect("valid ID").as_str(), valid);
        }
        for invalid in ["", "Personal", "-work", "has space", "slash/wiki"] {
            assert!(WikiId::parse(invalid).is_err(), "accepted `{invalid}`");
        }
    }

    #[test]
    fn add_update_remove_round_trip_is_sorted_and_persistent() {
        let temporary = tempdir().expect("temporary directory");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir(&first).expect("first wiki");
        fs::create_dir(&second).expect("second wiki");
        let registry = WikiRegistry::at(temporary.path().join("config/daemon.toml"));

        registry
            .add(&request("work", &second), false)
            .expect("add work");
        let personal = registry
            .add(&request("personal", &first), false)
            .expect("add personal");
        let loaded = registry.load().expect("load registry");
        assert_eq!(loaded.vaults[0].id.as_str(), "personal");
        assert_eq!(loaded.vaults[1].id.as_str(), "work");
        assert_eq!(loaded.vaults[0].groups, ["daily", "zeta"]);

        let updated = registry
            .update(
                &personal.id,
                &UpdateWikiRequest {
                    groups_to_add: vec!["mobile".to_string()],
                    groups_to_remove: vec!["zeta".to_string()],
                    permissions_profile: Some(Some("readonly".to_string())),
                    sync_paused: Some(true),
                },
                false,
            )
            .expect("update personal");
        assert_eq!(updated.groups, ["daily", "mobile"]);
        assert_eq!(updated.permissions_profile.as_deref(), Some("readonly"));
        assert!(updated.sync_paused);

        let removed = registry
            .remove(&personal.id, false)
            .expect("remove personal");
        assert_eq!(removed.registration_id, personal.registration_id);
        assert_eq!(registry.load().expect("reload").vaults.len(), 1);
        assert!(first.is_dir(), "unregistering must preserve the worktree");
    }

    #[test]
    fn dry_run_validates_but_does_not_create_registry_state() {
        let temporary = tempdir().expect("temporary directory");
        let wiki = temporary.path().join("wiki");
        fs::create_dir(&wiki).expect("wiki directory");
        let config = temporary.path().join("config/daemon.toml");
        let registry = WikiRegistry::at(config.clone());

        let planned = registry
            .add(&request("personal", &wiki), true)
            .expect("plan add");

        assert_eq!(planned.id.as_str(), "personal");
        assert!(!config.exists());
        assert!(registry.load().expect("load empty").vaults.is_empty());
    }

    #[test]
    fn duplicate_ids_and_paths_are_rejected() {
        let temporary = tempdir().expect("temporary directory");
        let wiki = temporary.path().join("wiki");
        fs::create_dir(&wiki).expect("wiki directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(&request("personal", &wiki), false)
            .expect("first add");

        assert!(matches!(
            registry.add(&request("personal", &wiki), false),
            Err(RegistryError::DuplicateId(_))
        ));
        assert!(matches!(
            registry.add(&request("other", &wiki), false),
            Err(RegistryError::DuplicatePath { .. })
        ));

        let detached_git = temporary.path().join("git");
        let first_worktree = temporary.path().join("first-worktree");
        let second_worktree = temporary.path().join("second-worktree");
        fs::create_dir(&detached_git).expect("Git directory");
        fs::create_dir(&first_worktree).expect("first worktree");
        fs::create_dir(&second_worktree).expect("second worktree");
        let mut first = request("first", &first_worktree);
        first.git_dir = Some(detached_git.clone());
        registry.add(&first, false).expect("first detached wiki");
        let mut second = request("second", &second_worktree);
        second.git_dir = Some(detached_git);
        assert!(matches!(
            registry.add(&second, false),
            Err(RegistryError::DuplicateGitDir { .. })
        ));
    }

    #[test]
    fn registrations_can_be_resolved_by_canonical_path() {
        let temporary = tempdir().expect("temporary directory");
        let wiki = temporary.path().join("wiki");
        fs::create_dir(&wiki).expect("wiki directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let registered = registry
            .add(&request("personal", &wiki), false)
            .expect("register wiki");

        assert_eq!(
            registry
                .find_by_path(&wiki.join("."))
                .expect("resolve registered path"),
            registered
        );
        assert!(matches!(
            registry.find_by_path(temporary.path()),
            Err(RegistryError::UnregisteredPath(_))
        ));
    }

    #[test]
    fn daemon_settings_are_validated_persisted_and_clearable() {
        let temporary = tempdir().expect("temporary directory");
        let config_path = temporary.path().join("config/daemon.toml");
        let registry = WikiRegistry::at(config_path.clone());
        let agent = DaemonAgentConfig {
            base_url: "https://agents.example.test/v1".to_string(),
            model: "planner-1".to_string(),
            api_key_env: Some("VULCAN_AGENT_KEY".to_string()),
        };

        let preview = registry
            .set_agent(DaemonAgentKind::Resolution, agent.clone(), true)
            .expect("preview agent");
        assert_eq!(preview.resolution_agent.as_ref(), Some(&agent));
        assert!(!config_path.exists());

        registry
            .set_bind("[::1]:4321", false)
            .expect("set loopback bind");
        registry
            .set_agent(DaemonAgentKind::Semantic, agent.clone(), false)
            .expect("set semantic agent");
        let loaded = registry.load().expect("load configured registry");
        assert_eq!(loaded.bind, "[::1]:4321");
        assert_eq!(loaded.semantic_agent.as_ref(), Some(&agent));
        assert!(loaded.resolution_agent.is_none());

        let cleared = registry
            .clear_agent(DaemonAgentKind::Semantic, false)
            .expect("clear semantic agent");
        assert!(cleared.semantic_agent.is_none());
        assert!(registry
            .load()
            .expect("load cleared registry")
            .semantic_agent
            .is_none());

        let worker = DaemonSemanticWorkerConfig {
            wikis: vec![WikiId::parse("personal").expect("wiki ID")],
            semantic_ref: "refs/heads/semantic".to_string(),
            remote: "origin".to_string(),
            live_ref: "refs/heads/__vulcan-sync/live".to_string(),
            publish: true,
            quiet_seconds: 120,
            maximum_wait_seconds: 3_600,
            poll_seconds: 30,
        };
        let configured = registry
            .set_semantic_worker(worker.clone(), false)
            .expect("set semantic worker");
        assert_eq!(configured.semantic_worker.as_ref(), Some(&worker));
        assert!(registry
            .clear_semantic_worker(false)
            .expect("clear semantic worker")
            .semantic_worker
            .is_none());
    }

    #[test]
    fn daemon_settings_reject_remote_binds_embedded_credentials_and_invalid_env_names() {
        let temporary = tempdir().expect("temporary directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        assert!(matches!(
            registry.set_bind("0.0.0.0:3210", true),
            Err(RegistryError::InvalidDaemonSetting(_))
        ));
        for agent in [
            DaemonAgentConfig {
                base_url: "https://secret@agents.example.test/v1".to_string(),
                model: "planner".to_string(),
                api_key_env: None,
            },
            DaemonAgentConfig {
                base_url: "https://agents.example.test/v1".to_string(),
                model: "planner".to_string(),
                api_key_env: Some("bad-name".to_string()),
            },
        ] {
            assert!(matches!(
                registry.set_agent(DaemonAgentKind::Resolution, agent, true),
                Err(RegistryError::InvalidDaemonSetting(_))
            ));
        }
        let invalid_worker = DaemonSemanticWorkerConfig {
            wikis: Vec::new(),
            semantic_ref: "refs/heads/main".to_string(),
            remote: "origin".to_string(),
            live_ref: "refs/heads/__vulcan-sync/live".to_string(),
            publish: true,
            quiet_seconds: 900,
            maximum_wait_seconds: 21_600,
            poll_seconds: 30,
        };
        assert!(matches!(
            registry.set_semantic_worker(invalid_worker, true),
            Err(RegistryError::InvalidDaemonSetting(_))
        ));
    }
}
