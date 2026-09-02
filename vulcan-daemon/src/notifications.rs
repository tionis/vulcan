use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter, Write as _};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use ulid::Ulid;
use vulcan_app::sync::GitRefName;
use vulcan_event_relay::{
    validate_git_source, SecretString, SourceDescriptor, SubscriptionBundle, GIT_PROFILE,
};

use crate::registry::{WikiId, WikiRegistration};

const STORE_VERSION: u32 = 1;
const CREDENTIAL_VERSION: u32 = 1;
const NOTIFICATIONS_DIRECTORY: &str = "notifications";
const SUBSCRIPTIONS_DIRECTORY: &str = "subscriptions";
const MANIFEST_FILE: &str = "subscription.json";
const CREDENTIAL_FILE: &str = "credential.json";
const MAX_SUBSCRIPTIONS: usize = 1_024;
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct NotificationSubscriptionId(Ulid);

impl NotificationSubscriptionId {
    pub fn parse(value: &str) -> Result<Self, NotificationStoreError> {
        Ulid::from_string(value)
            .map(Self)
            .map_err(|_| NotificationStoreError::InvalidId(value.to_string()))
    }

    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }

    #[must_use]
    pub fn as_string(self) -> String {
        self.0.to_string()
    }
}

impl Default for NotificationSubscriptionId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for NotificationSubscriptionId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotificationSubscription {
    pub version: u32,
    pub id: NotificationSubscriptionId,
    pub wiki_id: WikiId,
    pub registration_id: Ulid,
    pub source: String,
    pub refs: Vec<String>,
    pub descriptor: SourceDescriptor,
    pub credential_id: String,
    pub credential_scheme: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationMutationAction {
    Import,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NotificationMutationReport {
    pub action: NotificationMutationAction,
    pub dry_run: bool,
    pub subscription: NotificationSubscription,
}

#[derive(Debug, Clone)]
pub struct NotificationStore {
    root: PathBuf,
}

impl NotificationStore {
    pub fn user_default() -> Result<Self, NotificationStoreError> {
        let state_root = vulcan_core::vulcan_user_state_dir()
            .ok_or(NotificationStoreError::StateDirectoryUnavailable)?;
        Ok(Self::at(state_root))
    }

    #[must_use]
    pub fn at(state_root: impl AsRef<Path>) -> Self {
        Self {
            root: state_root.as_ref().join(NOTIFICATIONS_DIRECTORY),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn list(
        &self,
        wiki: Option<&WikiId>,
    ) -> Result<Vec<NotificationSubscription>, NotificationStoreError> {
        match fs::symlink_metadata(&self.root) {
            Ok(_) => validate_subscription_directory(&self.root)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
        let subscriptions = self.subscriptions_path();
        let metadata = match fs::symlink_metadata(&subscriptions) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(NotificationStoreError::InvalidStore(format!(
                "{} is not a regular directory",
                subscriptions.display()
            )));
        }
        let mut entries = fs::read_dir(&subscriptions)?.collect::<Result<Vec<_>, _>>()?;
        if entries.len() > MAX_SUBSCRIPTIONS {
            return Err(NotificationStoreError::InvalidStore(format!(
                "notification store exceeds the {MAX_SUBSCRIPTIONS} subscription limit"
            )));
        }
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut result = Vec::new();
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".tmp-") || name.starts_with(".removed-") {
                continue;
            }
            let id = NotificationSubscriptionId::parse(&name)?;
            let subscription = self.load_manifest(id)?;
            if wiki.is_none_or(|wiki| &subscription.wiki_id == wiki) {
                result.push(subscription);
            }
        }
        result.sort_by_key(|item| item.id);
        Ok(result)
    }

    pub fn show(
        &self,
        id: NotificationSubscriptionId,
    ) -> Result<NotificationSubscription, NotificationStoreError> {
        self.load_manifest(id)
    }

    pub fn credential(
        &self,
        id: NotificationSubscriptionId,
    ) -> Result<SecretString, NotificationStoreError> {
        let directory = self.subscription_path(id);
        let path = directory.join(CREDENTIAL_FILE);
        validate_regular_bounded_file(&path, MAX_CREDENTIAL_BYTES, true)?;
        let stored: StoredCredential = serde_json::from_slice(&fs::read(&path)?)?;
        if stored.version != CREDENTIAL_VERSION
            || stored.scheme != "bearer_capability"
            || !valid_credential_id(&stored.credential_id)
            || stored.token.len() < 43
        {
            return Err(NotificationStoreError::InvalidStore(format!(
                "credential for notification subscription `{id}` is invalid"
            )));
        }
        let manifest = self.load_manifest(id)?;
        if stored.scheme != manifest.credential_scheme
            || stored.credential_id != manifest.credential_id
            || credential_id(&stored.token) != stored.credential_id
        {
            return Err(NotificationStoreError::InvalidStore(format!(
                "credential for notification subscription `{id}` does not match its manifest"
            )));
        }
        Ok(SecretString::new(stored.token))
    }

    pub fn import(
        &self,
        registration: &WikiRegistration,
        source: String,
        refs: Vec<String>,
        bundle: SubscriptionBundle,
        dry_run: bool,
    ) -> Result<NotificationMutationReport, NotificationStoreError> {
        bundle.validate()?;
        validate_git_source(&source)?;
        if !bundle
            .descriptor
            .profiles
            .iter()
            .any(|profile| profile == GIT_PROFILE)
        {
            return Err(NotificationStoreError::InvalidBinding(
                "descriptor does not advertise the Git realtime profile".to_string(),
            ));
        }
        if registration.sync_backend.as_deref() != Some("git") {
            return Err(NotificationStoreError::InvalidBinding(format!(
                "wiki `{}` is not registered with the Git sync backend",
                registration.id
            )));
        }
        let refs = normalize_refs(refs)?;
        for existing in self.list(None)? {
            if existing.source == source
                && existing
                    .refs
                    .iter()
                    .any(|reference| refs.contains(reference))
            {
                return Err(NotificationStoreError::AmbiguousBinding {
                    source,
                    existing: existing.id,
                });
            }
        }
        let token = bundle.credential.token.expose_secret().to_string();
        let subscription = NotificationSubscription {
            version: STORE_VERSION,
            id: NotificationSubscriptionId::new(),
            wiki_id: registration.id.clone(),
            registration_id: registration.registration_id,
            source,
            refs,
            descriptor: bundle.descriptor,
            credential_id: credential_id(&token),
            credential_scheme: bundle.credential.scheme,
        };
        let report = NotificationMutationReport {
            action: NotificationMutationAction::Import,
            dry_run,
            subscription,
        };
        if !dry_run {
            self.write_subscription(&report.subscription, token)?;
        }
        Ok(report)
    }

    pub fn remove(
        &self,
        id: NotificationSubscriptionId,
        dry_run: bool,
    ) -> Result<NotificationMutationReport, NotificationStoreError> {
        let subscription = self.load_manifest(id)?;
        let report = NotificationMutationReport {
            action: NotificationMutationAction::Remove,
            dry_run,
            subscription,
        };
        if !dry_run {
            let source = self.subscription_path(id);
            let removed = self
                .subscriptions_path()
                .join(format!(".removed-{}", Ulid::new()));
            fs::rename(&source, &removed)?;
            fs::remove_dir_all(removed)?;
        }
        Ok(report)
    }

    fn subscriptions_path(&self) -> PathBuf {
        self.root.join(SUBSCRIPTIONS_DIRECTORY)
    }

    fn subscription_path(&self, id: NotificationSubscriptionId) -> PathBuf {
        self.subscriptions_path().join(id.to_string())
    }

    fn load_manifest(
        &self,
        id: NotificationSubscriptionId,
    ) -> Result<NotificationSubscription, NotificationStoreError> {
        let directory = self.subscription_path(id);
        validate_subscription_directory(&directory)?;
        let path = directory.join(MANIFEST_FILE);
        validate_regular_bounded_file(&path, MAX_MANIFEST_BYTES, false)?;
        let subscription: NotificationSubscription = serde_json::from_slice(&fs::read(path)?)?;
        validate_subscription(&subscription, id)?;
        Ok(subscription)
    }

    fn write_subscription(
        &self,
        subscription: &NotificationSubscription,
        token: String,
    ) -> Result<(), NotificationStoreError> {
        let subscriptions = self.subscriptions_path();
        let state_root = self.root.parent().ok_or_else(|| {
            NotificationStoreError::InvalidStore("store root has no parent".to_string())
        })?;
        fs::create_dir_all(state_root)?;
        ensure_owner_only_directory(&self.root)?;
        ensure_owner_only_directory(&subscriptions)?;
        let temporary = subscriptions.join(format!(".tmp-{}", Ulid::new()));
        fs::create_dir(&temporary)?;
        set_owner_only_directory(&temporary)?;
        let result = (|| {
            write_owner_only_json(&temporary.join(MANIFEST_FILE), subscription)?;
            write_owner_only_json(
                &temporary.join(CREDENTIAL_FILE),
                &StoredCredential {
                    version: CREDENTIAL_VERSION,
                    scheme: subscription.credential_scheme.clone(),
                    credential_id: subscription.credential_id.clone(),
                    token,
                },
            )?;
            let destination = self.subscription_path(subscription.id);
            fs::rename(&temporary, &destination).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    NotificationStoreError::DuplicateId(subscription.id)
                } else {
                    NotificationStoreError::Io(error)
                }
            })?;
            Ok(())
        })();
        if result.is_err() && temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredCredential {
    version: u32,
    scheme: String,
    credential_id: String,
    token: String,
}

fn validate_subscription(
    subscription: &NotificationSubscription,
    expected_id: NotificationSubscriptionId,
) -> Result<(), NotificationStoreError> {
    if subscription.version != STORE_VERSION || subscription.id != expected_id {
        return Err(NotificationStoreError::InvalidStore(format!(
            "notification subscription `{expected_id}` has an invalid version or identity"
        )));
    }
    subscription.descriptor.validate()?;
    validate_git_source(&subscription.source)?;
    if normalize_refs(subscription.refs.clone())? != subscription.refs {
        return Err(NotificationStoreError::InvalidStore(format!(
            "notification subscription `{expected_id}` refs are not normalized"
        )));
    }
    if !valid_credential_id(&subscription.credential_id)
        || subscription.credential_scheme != "bearer_capability"
    {
        return Err(NotificationStoreError::InvalidStore(format!(
            "notification subscription `{expected_id}` has invalid credential metadata"
        )));
    }
    Ok(())
}

fn normalize_refs(refs: Vec<String>) -> Result<Vec<String>, NotificationStoreError> {
    if refs.is_empty() || refs.len() > 64 {
        return Err(NotificationStoreError::InvalidBinding(
            "one to 64 complete Git refs are required".to_string(),
        ));
    }
    let mut normalized = BTreeSet::new();
    for reference in refs {
        let reference = GitRefName::parse(reference).map_err(|error| {
            NotificationStoreError::InvalidBinding(format!("invalid notification ref: {error}"))
        })?;
        normalized.insert(reference.to_string());
    }
    Ok(normalized.into_iter().collect())
}

fn credential_id(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut id = String::with_capacity(24);
    for byte in &digest[..12] {
        write!(id, "{byte:02x}").expect("writing to a String cannot fail");
    }
    id
}

fn ensure_owner_only_directory(path: &Path) -> Result<(), NotificationStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_subscription_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            set_owner_only_directory(path)?;
            validate_subscription_directory(path)
        }
        Err(error) => Err(error.into()),
    }
}

fn valid_credential_id(value: &str) -> bool {
    value.len() == 24 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_subscription_directory(path: &Path) -> Result<(), NotificationStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            NotificationStoreError::Missing(path.to_path_buf())
        } else {
            NotificationStoreError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(NotificationStoreError::InvalidStore(format!(
            "{} is not a regular directory",
            path.display()
        )));
    }
    validate_owner_only(&metadata, path)
}

fn validate_regular_bounded_file(
    path: &Path,
    limit: u64,
    require_owner_only: bool,
) -> Result<(), NotificationStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            NotificationStoreError::Missing(path.to_path_buf())
        } else {
            NotificationStoreError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit {
        return Err(NotificationStoreError::InvalidStore(format!(
            "{} must be a regular file no larger than {limit} bytes",
            path.display()
        )));
    }
    if require_owner_only {
        validate_owner_only(&metadata, path)?;
    }
    Ok(())
}

fn write_owner_only_json<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), NotificationStoreError> {
    let parent = path.parent().ok_or_else(|| {
        NotificationStoreError::InvalidStore("store path has no parent".to_string())
    })?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    set_owner_only_file(temporary.path())?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| NotificationStoreError::Io(error.error))?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), NotificationStoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<(), NotificationStoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file(path: &Path) -> Result<(), NotificationStoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::permissions_set_readonly_false)]
fn set_owner_only_file(path: &Path) -> Result<(), NotificationStoreError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn validate_owner_only(metadata: &fs::Metadata, path: &Path) -> Result<(), NotificationStoreError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(NotificationStoreError::InvalidStore(format!(
            "{} is accessible by group or other users",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_owner_only(
    _metadata: &fs::Metadata,
    _path: &Path,
) -> Result<(), NotificationStoreError> {
    Ok(())
}

#[derive(Debug)]
pub enum NotificationStoreError {
    StateDirectoryUnavailable,
    InvalidId(String),
    Missing(PathBuf),
    DuplicateId(NotificationSubscriptionId),
    AmbiguousBinding {
        source: String,
        existing: NotificationSubscriptionId,
    },
    InvalidBinding(String),
    InvalidStore(String),
    Protocol(vulcan_event_relay::ValidationError),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl Display for NotificationStoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StateDirectoryUnavailable => {
                formatter.write_str("the user state directory is unavailable")
            }
            Self::InvalidId(id) => write!(formatter, "invalid notification subscription ID `{id}`"),
            Self::Missing(path) => write!(formatter, "notification state is missing at {}", path.display()),
            Self::DuplicateId(id) => write!(formatter, "notification subscription `{id}` already exists"),
            Self::AmbiguousBinding { source, existing } => write!(
                formatter,
                "Git event source `{source}` overlaps existing notification subscription `{existing}`"
            ),
            Self::InvalidBinding(detail) | Self::InvalidStore(detail) => formatter.write_str(detail),
            Self::Protocol(error) => Display::fmt(error, formatter),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for NotificationStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for NotificationStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for NotificationStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<vulcan_event_relay::ValidationError> for NotificationStoreError {
    fn from(error: vulcan_event_relay::ValidationError) -> Self {
        Self::Protocol(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vulcan_event_relay::SubscriberCredential;

    fn registration(id: &str) -> WikiRegistration {
        WikiRegistration {
            id: WikiId::parse(id).expect("wiki ID"),
            registration_id: Ulid::new(),
            path: PathBuf::from("/vault"),
            groups: Vec::new(),
            git_dir: None,
            permissions_profile: None,
            sync_backend: Some("git".to_string()),
            platform_profile: None,
            sync_paused: false,
        }
    }

    fn bundle(token: &str) -> SubscriptionBundle {
        serde_json::from_value(serde_json::json!({
            "spec":"event-relay-subscription/1",
            "descriptor":{
                "spec":"event-relay/1",
                "id":"urn:event-relay-channel:01K00000000000000000000000",
                "profiles":[GIT_PROFILE],
                "bindings":[{
                    "type":"nats",
                    "endpoint":"tls://events.example.net:4222",
                    "subject_filter":"events.channels.01K00000000000000000000000.>"
                }],
                "authorization":["bearer_capability"],
                "retention":[{
                    "id":"all",
                    "types":["*"],
                    "class":"ephemeral"
                }],
                "limits":{"event_bytes":65536}
            },
            "credential":{
                "scheme":"bearer_capability",
                "token":token
            }
        }))
        .expect("bundle")
    }

    #[test]
    fn import_is_atomic_redacted_and_round_trips_credential() {
        let temporary = TempDir::new().expect("temporary directory");
        let store = NotificationStore::at(temporary.path());
        let token = "er1.client.0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
        let report = store
            .import(
                &registration("notes"),
                "urn:git-repository:01K00000000000000000000000".to_string(),
                vec!["refs/heads/__vulcan-sync/live".to_string()],
                bundle(token),
                false,
            )
            .expect("import");
        assert!(!format!("{report:?}").contains(token));
        assert_eq!(store.list(None).expect("list").len(), 1);
        assert_eq!(
            store
                .credential(report.subscription.id)
                .expect("credential")
                .expose_secret(),
            token
        );
        let manifest = fs::read_to_string(
            store
                .subscription_path(report.subscription.id)
                .join(MANIFEST_FILE),
        )
        .expect("manifest");
        assert!(!manifest.contains(token));
    }

    #[test]
    fn dry_run_and_remove_have_atomic_mutation_boundaries() {
        let temporary = TempDir::new().expect("temporary directory");
        let store = NotificationStore::at(temporary.path());
        let preview = store
            .import(
                &registration("notes"),
                "urn:git-repository:01K00000000000000000000000".to_string(),
                vec!["refs/heads/main".to_string()],
                bundle("er1.client.0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"),
                true,
            )
            .expect("preview");
        assert!(preview.dry_run);
        assert!(!store.root().exists());

        let imported = store
            .import(
                &registration("notes"),
                "urn:git-repository:01K00000000000000000000001".to_string(),
                vec!["refs/heads/main".to_string()],
                bundle("er1.client.1123456789abcdefghijklmnopqrstuvwxyzABCDEFG"),
                false,
            )
            .expect("import");
        store
            .remove(imported.subscription.id, true)
            .expect("removal preview");
        assert_eq!(store.list(None).expect("list after preview").len(), 1);
        store
            .remove(imported.subscription.id, false)
            .expect("remove");
        assert!(store.list(None).expect("empty list").is_empty());
    }

    #[test]
    fn overlapping_source_and_ref_bindings_are_rejected() {
        let temporary = TempDir::new().expect("temporary directory");
        let store = NotificationStore::at(temporary.path());
        let source = "urn:git-repository:01K00000000000000000000000";
        store
            .import(
                &registration("one"),
                source.to_string(),
                vec!["refs/heads/main".to_string()],
                bundle("er1.client.0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"),
                false,
            )
            .expect("first import");
        let error = store
            .import(
                &registration("two"),
                source.to_string(),
                vec!["refs/heads/main".to_string()],
                bundle("er1.client.1123456789abcdefghijklmnopqrstuvwxyzABCDEFG"),
                false,
            )
            .expect_err("ambiguous binding");
        assert!(matches!(
            error,
            NotificationStoreError::AmbiguousBinding { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn credential_load_rejects_broad_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TempDir::new().expect("temporary directory");
        let store = NotificationStore::at(temporary.path());
        let imported = store
            .import(
                &registration("notes"),
                "urn:git-repository:01K00000000000000000000000".to_string(),
                vec!["refs/heads/main".to_string()],
                bundle("er1.client.0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"),
                false,
            )
            .expect("import");
        let path = store
            .subscription_path(imported.subscription.id)
            .join(CREDENTIAL_FILE);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("permissions");
        assert!(matches!(
            store.credential(imported.subscription.id),
            Err(NotificationStoreError::InvalidStore(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_a_symlinked_store_root_without_writing_through_it() {
        use std::os::unix::fs::symlink;

        let temporary = TempDir::new().expect("temporary directory");
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).expect("outside directory");
        symlink(&outside, temporary.path().join(NOTIFICATIONS_DIRECTORY)).expect("store symlink");
        let store = NotificationStore::at(temporary.path());
        assert!(matches!(
            store.import(
                &registration("notes"),
                "urn:git-repository:01K00000000000000000000000".to_string(),
                vec!["refs/heads/main".to_string()],
                bundle("er1.client.0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"),
                false,
            ),
            Err(NotificationStoreError::InvalidStore(_))
        ));
        assert!(!outside.join(SUBSCRIPTIONS_DIRECTORY).exists());
    }

    #[test]
    fn subscriber_credential_debug_remains_redacted_in_store_inputs() {
        let credential = SubscriberCredential {
            scheme: "bearer_capability".to_string(),
            token: SecretString::new("secret"),
        };
        assert!(!format!("{credential:?}").contains("secret"));
    }
}
