use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use ulid::Ulid;

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
            vaults: Vec::new(),
        }
    }
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
    MissingDirectory(PathBuf),
    DuplicateId(WikiId),
    DuplicatePath { id: WikiId, path: PathBuf },
    DuplicateGitDir { id: WikiId, path: PathBuf },
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
        load_config(&self.path)
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

    fn mutate<T>(
        &self,
        dry_run: bool,
        operation: impl FnOnce(&mut DaemonConfig) -> Result<T, RegistryError>,
    ) -> Result<T, RegistryError> {
        let _lock = RegistryLock::acquire(&self.path)?;
        let mut config = load_config(&self.path)?;
        let result = operation(&mut config)?;
        if !dry_run {
            save_config(&self.path, &config)?;
        }
        Ok(result)
    }
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
}
