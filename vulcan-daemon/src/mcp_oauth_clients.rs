//! Durable dynamic OAuth client registrations shared by MCP hosting modes.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use subtle::ConstantTimeEq;
use vulcan_secrets::{
    SecretBytes, SecretName, SecretProvider, SecretReference, SecretStore, SecretStoreError,
};

const REGISTRY_VERSION: u32 = 1;
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RegisteredOAuthClient {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
    pub token_endpoint_auth_method: String,
    pub client_id_issued_at: u64,
}

impl std::fmt::Debug for RegisteredOAuthClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredOAuthClient")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field(
                "token_endpoint_auth_method",
                &self.token_endpoint_auth_method,
            )
            .finish_non_exhaustive()
    }
}

pub enum OAuthClientRegistryError {
    Io(io::Error),
    Json(serde_json::Error),
    Invalid(String),
    SecretStore(SecretStoreError),
    MigrationRequired,
}

impl std::fmt::Debug for OAuthClientRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for OAuthClientRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "OAuth client registry I/O error: {error}"),
            Self::Json(error) => write!(formatter, "OAuth client registry JSON error at line {} column {}", error.line(), error.column()),
            Self::Invalid(error) => write!(formatter, "invalid OAuth client registry: {error}"),
            Self::SecretStore(error) => write!(formatter, "OAuth client credential custody: {error}"),
            Self::MigrationRequired => formatter.write_str("OAuth client credentials require explicit secret-store migration before named remote startup"),
        }
    }
}

impl From<SecretStoreError> for OAuthClientRegistryError {
    fn from(error: SecretStoreError) -> Self {
        Self::SecretStore(error)
    }
}

/// Trusted host configuration, never resolved from an MCP client request.
#[derive(Debug, Clone)]
pub struct OAuthClientSecretCustody {
    store: Arc<dyn SecretStore>,
    namespace: SecretName,
}

impl OAuthClientSecretCustody {
    pub fn new(
        store: Arc<dyn SecretStore>,
        namespace: SecretName,
    ) -> Result<Self, OAuthClientRegistryError> {
        if namespace.as_str().len() > 56 {
            return Err(OAuthClientRegistryError::Invalid(
                "credential namespace exceeds its limit".into(),
            ));
        }
        Ok(Self { store, namespace })
    }

    #[must_use]
    pub fn reference(&self, client_id: &str) -> SecretReference {
        SecretReference {
            provider: SecretProvider::ProtectedFileV1,
            name: SecretName::parse(format!(
                "{}.client.{}",
                self.namespace.as_str(),
                blake3::hash(client_id.as_bytes()).to_hex()
            ))
            .expect("bounded generated credential name"),
        }
    }

    fn ensure_secret(
        &self,
        client: &RegisteredOAuthClient,
    ) -> Result<(), OAuthClientRegistryError> {
        let name = self.reference(&client.client_id).name;
        let value = SecretBytes::new(client.client_secret.as_bytes().to_vec())?;
        match self.store.get(&name) {
            Ok(existing) => compare_secret(&existing, &value),
            Err(SecretStoreError::Missing) => match self.store.create(&name, &value) {
                Ok(()) => Ok(()),
                Err(SecretStoreError::AlreadyExists) => {
                    compare_secret(&self.store.get(&name)?, &value)
                }
                Err(error) => Err(error.into()),
            },
            Err(error) => Err(error.into()),
        }
    }
}

fn compare_secret(
    existing: &SecretBytes,
    expected: &SecretBytes,
) -> Result<(), OAuthClientRegistryError> {
    if bool::from(existing.expose().ct_eq(expected.expose())) {
        Ok(())
    } else {
        Err(OAuthClientRegistryError::Invalid(
            "existing credential differs; replacement refused".into(),
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OAuthClientSecretMigration {
    pub dry_run: bool,
    pub registry_present: bool,
    pub migrated_clients: Option<usize>,
}

impl std::error::Error for OAuthClientRegistryError {}

impl From<io::Error> for OAuthClientRegistryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for OAuthClientRegistryError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
pub struct OAuthClientRegistry {
    path: Option<PathBuf>,
    custody: Option<OAuthClientSecretCustody>,
    ephemeral: Mutex<BTreeMap<String, RegisteredOAuthClient>>,
}

impl OAuthClientRegistry {
    /// Use a durable, cross-process registry. Existing bare-array files are read without mutation.
    pub fn at(path: PathBuf) -> Result<Self, OAuthClientRegistryError> {
        let _lock = RegistryLock::acquire(&path)?;
        let _ = load_clients(&path)?;
        Ok(Self {
            path: Some(path),
            custody: None,
            ephemeral: Mutex::new(BTreeMap::new()),
        })
    }

    /// Named hosts require reference-only metadata and fail closed on legacy secrets.
    pub fn with_secret_store(
        path: PathBuf,
        custody: OAuthClientSecretCustody,
    ) -> Result<Self, OAuthClientRegistryError> {
        let _lock = RegistryLock::acquire(&path)?;
        let _ = load_clients_with_custody(&path, Some(&custody), false)?;
        Ok(Self {
            path: Some(path),
            custody: Some(custody),
            ephemeral: Mutex::new(BTreeMap::new()),
        })
    }

    /// Explicit, resumable migration. Dry-run only inspects the source metadata:
    /// no credential reads, provider writes, registry lock creation, or publication.
    pub fn migrate_secrets(
        path: &Path,
        custody: &OAuthClientSecretCustody,
        dry_run: bool,
    ) -> Result<OAuthClientSecretMigration, OAuthClientRegistryError> {
        let present = match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() > MAX_REGISTRY_BYTES
                {
                    return Err(OAuthClientRegistryError::Invalid(
                        "migration source must be a bounded regular registry".into(),
                    ));
                }
                require_owner_only(path, &metadata)?;
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if dry_run || !present {
            return Ok(OAuthClientSecretMigration {
                dry_run,
                registry_present: present,
                migrated_clients: None,
            });
        }
        let _lock = RegistryLock::acquire(path)?;
        let clients = load_clients_with_custody(path, Some(custody), true)?;
        save_clients_with_custody(path, &clients, Some(custody))?;
        Ok(OAuthClientSecretMigration {
            dry_run: false,
            registry_present: true,
            migrated_clients: Some(clients.len()),
        })
    }

    /// Keep direct test and temporary issuer state in memory without creating device files.
    #[must_use]
    pub fn ephemeral() -> Self {
        Self {
            path: None,
            custody: None,
            ephemeral: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn get(
        &self,
        client_id: &str,
    ) -> Result<Option<RegisteredOAuthClient>, OAuthClientRegistryError> {
        if let Some(path) = &self.path {
            let _lock = RegistryLock::acquire(path)?;
            return Ok(
                load_clients_with_custody(path, self.custody.as_ref(), false)?.remove(client_id),
            );
        }
        Ok(self
            .ephemeral
            .lock()
            .expect("ephemeral OAuth clients lock should not be poisoned")
            .get(client_id)
            .cloned())
    }

    pub fn list(&self) -> Result<Vec<RegisteredOAuthClient>, OAuthClientRegistryError> {
        if let Some(path) = &self.path {
            let _lock = RegistryLock::acquire(path)?;
            return Ok(
                load_clients_with_custody(path, self.custody.as_ref(), false)?
                    .into_values()
                    .collect(),
            );
        }
        Ok(self
            .ephemeral
            .lock()
            .expect("ephemeral OAuth clients lock should not be poisoned")
            .values()
            .cloned()
            .collect())
    }

    /// Publish a client under the same lock used by all durable readers and writers.
    pub fn register(&self, client: RegisteredOAuthClient) -> Result<(), OAuthClientRegistryError> {
        if client.client_id.is_empty() {
            return Err(OAuthClientRegistryError::Invalid(
                "client ID must not be empty".to_string(),
            ));
        }
        if let Some(path) = &self.path {
            let _lock = RegistryLock::acquire(path)?;
            let mut clients = load_clients_with_custody(path, self.custody.as_ref(), false)?;
            if clients.contains_key(&client.client_id) {
                return Err(OAuthClientRegistryError::Invalid(format!(
                    "duplicate client ID `{}`",
                    client.client_id
                )));
            }
            clients.insert(client.client_id.clone(), client);
            return save_clients_with_custody(path, &clients, self.custody.as_ref());
        }
        let mut clients = self
            .ephemeral
            .lock()
            .expect("ephemeral OAuth clients lock should not be poisoned");
        if clients.contains_key(&client.client_id) {
            return Err(OAuthClientRegistryError::Invalid(format!(
                "duplicate client ID `{}`",
                client.client_id
            )));
        }
        clients.insert(client.client_id.clone(), client);
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    clients: Vec<RegisteredOAuthClient>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SecretRegistryFile {
    version: u32,
    namespace: SecretName,
    clients: Vec<StoredOAuthClient>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredOAuthClient {
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_reference: Option<SecretReference>,
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    token_endpoint_auth_method: String,
    client_id_issued_at: u64,
}

impl StoredOAuthClient {
    fn from_client(
        client: &RegisteredOAuthClient,
        custody: &OAuthClientSecretCustody,
    ) -> Result<Self, OAuthClientRegistryError> {
        let reference = match (
            client.token_endpoint_auth_method.as_str(),
            client.client_secret.is_empty(),
        ) {
            ("none", true) => None,
            ("client_secret_basic" | "client_secret_post", false)
                if client.client_secret.len() <= vulcan_secrets::MAX_SECRET_BYTES =>
            {
                Some(custody.reference(&client.client_id))
            }
            _ => {
                return Err(OAuthClientRegistryError::Invalid(
                    "client authentication method and credential disagree".into(),
                ))
            }
        };
        Ok(Self {
            client_id: client.client_id.clone(),
            secret_reference: reference,
            redirect_uris: client.redirect_uris.clone(),
            client_name: client.client_name.clone(),
            token_endpoint_auth_method: client.token_endpoint_auth_method.clone(),
            client_id_issued_at: client.client_id_issued_at,
        })
    }

    fn resolve(
        self,
        custody: &OAuthClientSecretCustody,
    ) -> Result<RegisteredOAuthClient, OAuthClientRegistryError> {
        let secret = match (
            self.token_endpoint_auth_method.as_str(),
            self.secret_reference,
        ) {
            ("none", None) => String::new(),
            ("client_secret_basic" | "client_secret_post", Some(reference))
                if reference == custody.reference(&self.client_id) =>
            {
                let value = custody.store.get(&reference.name)?;
                std::str::from_utf8(value.expose())
                    .map_err(|_| {
                        OAuthClientRegistryError::Invalid(
                            "client credential is not valid UTF-8".into(),
                        )
                    })?
                    .to_owned()
            }
            _ => {
                return Err(OAuthClientRegistryError::Invalid(
                    "client credential reference does not match its binding".into(),
                ))
            }
        };
        Ok(RegisteredOAuthClient {
            client_id: self.client_id,
            client_secret: secret,
            redirect_uris: self.redirect_uris,
            client_name: self.client_name,
            token_endpoint_auth_method: self.token_endpoint_auth_method,
            client_id_issued_at: self.client_id_issued_at,
        })
    }
}

fn load_clients(
    path: &Path,
) -> Result<BTreeMap<String, RegisteredOAuthClient>, OAuthClientRegistryError> {
    load_clients_with_custody(path, None, false)
}

fn load_clients_with_custody(
    path: &Path,
    custody: Option<&OAuthClientSecretCustody>,
    allow_legacy: bool,
) -> Result<BTreeMap<String, RegisteredOAuthClient>, OAuthClientRegistryError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_REGISTRY_BYTES
    {
        return Err(OAuthClientRegistryError::Invalid(format!(
            "{} must be a regular file no larger than 1 MiB",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    set_no_follow(&mut options);
    let file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() > MAX_REGISTRY_BYTES {
        return Err(OAuthClientRegistryError::Invalid(format!(
            "{} must be a regular file no larger than 1 MiB",
            path.display()
        )));
    }
    require_owner_only(path, &opened)?;
    let mut bytes = Vec::new();
    file.take(MAX_REGISTRY_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(OAuthClientRegistryError::Invalid(
            "registry exceeds the 1 MiB limit".to_string(),
        ));
    }
    decode_clients(&bytes, custody, allow_legacy)
}

fn decode_clients(
    bytes: &[u8],
    custody: Option<&OAuthClientSecretCustody>,
    allow_legacy: bool,
) -> Result<BTreeMap<String, RegisteredOAuthClient>, OAuthClientRegistryError> {
    let clients = if bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        == Some(b'[')
    {
        if custody.is_some() && !allow_legacy {
            return Err(OAuthClientRegistryError::MigrationRequired);
        }
        serde_json::from_slice::<Vec<RegisteredOAuthClient>>(bytes)?
    } else {
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        match serde_json::from_slice::<Version>(bytes)?.version {
            1 => {
                if custody.is_some() && !allow_legacy {
                    return Err(OAuthClientRegistryError::MigrationRequired);
                }
                serde_json::from_slice::<RegistryFile>(bytes)?.clients
            }
            2 => {
                let custody = custody.ok_or_else(|| {
                    OAuthClientRegistryError::Invalid(
                        "secret-store registry requires its configured custody provider".into(),
                    )
                })?;
                let file: SecretRegistryFile = serde_json::from_slice(bytes)?;
                if file.namespace != custody.namespace {
                    return Err(OAuthClientRegistryError::Invalid(
                        "client registry belongs to another credential namespace".into(),
                    ));
                }
                file.clients
                    .into_iter()
                    .map(|client| client.resolve(custody))
                    .collect::<Result<Vec<_>, _>>()?
            }
            version => {
                return Err(OAuthClientRegistryError::Invalid(format!(
                    "unsupported version {version}"
                )))
            }
        }
    };
    let mut indexed = BTreeMap::new();
    for client in clients {
        if client.client_id.is_empty() || indexed.insert(client.client_id.clone(), client).is_some()
        {
            return Err(OAuthClientRegistryError::Invalid(
                "empty or duplicate client ID".to_string(),
            ));
        }
    }
    Ok(indexed)
}

fn save_clients_with_custody(
    path: &Path,
    clients: &BTreeMap<String, RegisteredOAuthClient>,
    custody: Option<&OAuthClientSecretCustody>,
) -> Result<(), OAuthClientRegistryError> {
    let serialized = if let Some(custody) = custody {
        let file = SecretRegistryFile {
            version: 2,
            namespace: custody.namespace.clone(),
            clients: clients
                .values()
                .map(|client| StoredOAuthClient::from_client(client, custody))
                .collect::<Result<Vec<_>, _>>()?,
        };
        let bytes = serde_json::to_vec_pretty(&file)?;
        validate_registry_size(&bytes)?;
        // Validate and bound all metadata before any secret creation. Secrets
        // are immutable; partial migration resumes only when their bytes match.
        for client in clients
            .values()
            .filter(|client| !client.client_secret.is_empty())
        {
            custody.ensure_secret(client)?;
        }
        bytes
    } else {
        serde_json::to_vec_pretty(&RegistryFile {
            version: REGISTRY_VERSION,
            clients: clients.values().cloned().collect(),
        })?
    };
    validate_registry_size(&serialized)?;
    publish_registry(path, &serialized)
}

fn validate_registry_size(serialized: &[u8]) -> Result<(), OAuthClientRegistryError> {
    if serialized.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(OAuthClientRegistryError::Invalid(
            "registry exceeds the 1 MiB limit".to_string(),
        ));
    }
    Ok(())
}

fn publish_registry(path: &Path, serialized: &[u8]) -> Result<(), OAuthClientRegistryError> {
    vulcan_core::durable::replace(path, serialized, vulcan_core::durable::Durability::Full)?;
    Ok(())
}

struct RegistryLock {
    _file: File,
}

impl RegistryLock {
    fn acquire(path: &Path) -> Result<Self, OAuthClientRegistryError> {
        let parent = path.parent().ok_or_else(|| {
            OAuthClientRegistryError::Invalid("registry path has no parent".to_string())
        })?;
        fs::create_dir_all(parent)?;
        let lock_path = path.with_extension("lock");
        if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(OAuthClientRegistryError::Invalid(format!(
                    "{} must be a regular lock file",
                    lock_path.display()
                )));
            }
            require_owner_only(&lock_path, &metadata)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        set_no_follow(&mut options);
        let file = options.open(&lock_path)?;
        let opened = file.metadata()?;
        if !opened.is_file() {
            return Err(OAuthClientRegistryError::Invalid(format!(
                "{} must be a regular lock file",
                lock_path.display()
            )));
        }
        require_owner_only(&lock_path, &opened)?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }
}

fn set_no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = options;
}

#[cfg(unix)]
fn require_owner_only(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), OAuthClientRegistryError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(OAuthClientRegistryError::Invalid(format!(
            "{} must be owner-only (mode 0600)",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn require_owner_only(
    path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), OAuthClientRegistryError> {
    vulcan_app::windows_acl::verify_private_path(path).map_err(|error| {
        OAuthClientRegistryError::Invalid(format!(
            "{} must be private to the current user: {error}",
            path.display()
        ))
    })
}

#[cfg(not(any(unix, windows)))]
#[allow(clippy::unnecessary_wraps)] // Signature matches the fallible Unix implementation.
fn require_owner_only(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), OAuthClientRegistryError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes a hand-written fixture file owner-only, as the registry writes it.
    #[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
    fn set_owner_only(file: &File) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        let _ = file;
        Ok(())
    }
    use std::sync::Arc;
    use std::thread;

    fn client(id: &str) -> RegisteredOAuthClient {
        RegisteredOAuthClient {
            client_id: id.to_string(),
            client_secret: format!("secret-{id}"),
            redirect_uris: vec!["https://client.example.test/callback".to_string()],
            client_name: Some(id.to_string()),
            token_endpoint_auth_method: "client_secret_post".to_string(),
            client_id_issued_at: 1,
        }
    }

    fn test_custody(root: &Path, namespace: &str) -> OAuthClientSecretCustody {
        OAuthClientSecretCustody::new(
            Arc::new(vulcan_secrets::ProtectedFileSecretStore::at(
                root.join("secrets"),
            )),
            SecretName::parse(namespace).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn secret_store_registrations_publish_only_bound_references_and_survive_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("clients.json");
        let custody = test_custody(temporary.path(), "mcp-instance");
        let registry =
            OAuthClientRegistry::with_secret_store(path.clone(), custody.clone()).unwrap();
        registry.register(client("confidential")).unwrap();
        let mut public = client("public");
        public.token_endpoint_auth_method = "none".into();
        public.client_secret.clear();
        registry.register(public.clone()).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(!body.contains("secret-confidential"));
        assert!(!body.contains("\"client_secret\""));
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(
            json["clients"][0]["secret_reference"]["provider"],
            "file_v1"
        );
        assert!(json["clients"][1].get("secret_reference").is_none());
        let restarted =
            OAuthClientRegistry::with_secret_store(path.clone(), custody.clone()).unwrap();
        assert_eq!(
            restarted.get("confidential").unwrap(),
            Some(client("confidential"))
        );
        assert_eq!(restarted.get("public").unwrap(), Some(public));
        assert!(OAuthClientRegistry::at(path.clone()).is_err());
        assert!(OAuthClientRegistry::with_secret_store(
            path.clone(),
            test_custody(temporary.path(), "other-instance")
        )
        .is_err());
        let reference = custody.reference("confidential");
        custody.store.delete(&reference.name).unwrap();
        assert!(restarted.get("confidential").is_err());
        assert!(!format!("{:?}", client("confidential")).contains("secret-confidential"));
    }

    #[test]
    fn explicit_migration_is_dry_run_safe_and_resumes_after_a_partial_copy() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use vulcan_secrets::{SecretStoreCapabilities, SecretStoreState};
        #[derive(Debug)]
        struct FailSecondCreate {
            inner: vulcan_secrets::ProtectedFileSecretStore,
            creates: AtomicUsize,
        }
        impl SecretStore for FailSecondCreate {
            fn capabilities(&self) -> SecretStoreCapabilities {
                self.inner.capabilities()
            }
            fn inspect(&self, name: &SecretName) -> SecretStoreState {
                self.inner.inspect(name)
            }
            fn get(&self, name: &SecretName) -> Result<SecretBytes, SecretStoreError> {
                self.inner.get(name)
            }
            fn delete(&self, name: &SecretName) -> Result<(), SecretStoreError> {
                self.inner.delete(name)
            }
            fn create(
                &self,
                name: &SecretName,
                value: &SecretBytes,
            ) -> Result<(), SecretStoreError> {
                if self.creates.fetch_add(1, Ordering::SeqCst) == 1 {
                    return Err(SecretStoreError::Unavailable);
                }
                self.inner.create(name, value)
            }
        }
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("clients.json");
        let original = serde_json::to_vec(&vec![client("first"), client("second")]).unwrap();
        fs::write(&path, &original).unwrap();
        set_owner_only(&File::open(&path).unwrap()).unwrap();
        let destination = temporary.path().join("secrets");
        let failing = OAuthClientSecretCustody::new(
            Arc::new(FailSecondCreate {
                inner: vulcan_secrets::ProtectedFileSecretStore::at(&destination),
                creates: AtomicUsize::new(0),
            }),
            SecretName::parse("mcp-instance").unwrap(),
        )
        .unwrap();
        assert!(matches!(
            OAuthClientRegistry::with_secret_store(path.clone(), failing.clone()),
            Err(OAuthClientRegistryError::MigrationRequired)
        ));
        fs::remove_file(path.with_extension("lock")).unwrap();
        let preview = OAuthClientRegistry::migrate_secrets(&path, &failing, true).unwrap();
        assert!(preview.dry_run);
        assert!(preview.migrated_clients.is_none());
        assert!(!destination.exists());
        assert!(!path.with_extension("lock").exists());
        assert!(OAuthClientRegistry::migrate_secrets(&path, &failing, false).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        let ordinary = test_custody(temporary.path(), "mcp-instance");
        let applied = OAuthClientRegistry::migrate_secrets(&path, &ordinary, false).unwrap();
        assert_eq!(applied.migrated_clients, Some(2));
        let registry =
            OAuthClientRegistry::with_secret_store(path.clone(), ordinary.clone()).unwrap();
        assert_eq!(
            registry.list().unwrap(),
            vec![client("first"), client("second")]
        );
        assert_eq!(
            OAuthClientRegistry::migrate_secrets(&path, &ordinary, false)
                .unwrap()
                .migrated_clients,
            Some(2)
        );
        assert!(!fs::read_to_string(path).unwrap().contains("secret-first"));
    }

    #[test]
    fn migration_refuses_different_secrets_and_untrusted_reference_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("clients.json");
        let custody = test_custody(temporary.path(), "mcp-instance");
        let original = serde_json::to_vec(&vec![client("first")]).unwrap();
        fs::write(&path, &original).unwrap();
        set_owner_only(&File::open(&path).unwrap()).unwrap();
        let reference = custody.reference("first");
        custody
            .store
            .create(
                &reference.name,
                &SecretBytes::new(b"different private credential".to_vec()).unwrap(),
            )
            .unwrap();
        let error = OAuthClientRegistry::migrate_secrets(&path, &custody, false).unwrap_err();
        assert!(!format!("{error:?}").contains("different private credential"));
        assert_eq!(fs::read(&path).unwrap(), original);
        custody.store.delete(&reference.name).unwrap();
        OAuthClientRegistry::migrate_secrets(&path, &custody, false).unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        json["clients"][0]["secret_reference"] =
            serde_json::to_value(custody.reference("another-client")).unwrap();
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(OAuthClientRegistry::with_secret_store(path, custody).is_err());
    }

    #[test]
    fn separate_registry_instances_see_each_others_clients_without_stale_overwrites() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let path = temporary.path().join("oauth-clients.json");
        let first = Arc::new(OAuthClientRegistry::at(path.clone()).expect("first store"));
        let second = Arc::new(OAuthClientRegistry::at(path.clone()).expect("second store"));
        thread::scope(|scope| {
            for index in 0..8 {
                let registry = if index % 2 == 0 { &first } else { &second };
                scope.spawn(move || {
                    registry
                        .register(client(&format!("client-{index}")))
                        .expect("concurrent registration");
                });
            }
        });
        assert_eq!(first.list().expect("all clients").len(), 8);
        assert_eq!(second.list().expect("all clients").len(), 8);
        assert_eq!(
            first.get("client-7").expect("fresh lookup"),
            Some(client("client-7"))
        );
        assert_eq!(
            second.get("client-0").expect("fresh lookup"),
            Some(client("client-0"))
        );
        assert!(fs::read_to_string(path)
            .expect("durable registry")
            .contains("\"version\": 1"));
    }

    #[test]
    fn decode_errors_do_not_render_private_source_values() {
        let error = decode_clients(
            br#"{"version":"private-credential-marker","clients":[]}"#,
            None,
            false,
        )
        .unwrap_err();
        assert!(matches!(error, OAuthClientRegistryError::Json(_)));
        for diagnostic in [error.to_string(), format!("{error:?}")] {
            assert!(diagnostic.contains("line 1 column"));
            assert!(!diagnostic.contains("private-credential-marker"));
        }
    }

    #[test]
    fn legacy_array_migrates_on_write_and_invalid_files_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let path = temporary.path().join("oauth-clients.json");
        fs::write(
            &path,
            serde_json::to_vec(&vec![client("legacy")]).expect("legacy JSON"),
        )
        .expect("legacy file");
        set_owner_only(&File::open(&path).expect("legacy file handle"))
            .expect("owner-only legacy file");
        let registry = OAuthClientRegistry::at(path.clone()).expect("legacy store");
        assert_eq!(
            registry.get("legacy").expect("legacy lookup"),
            Some(client("legacy"))
        );
        registry
            .register(client("new"))
            .expect("migrating registration");
        assert_eq!(registry.list().expect("migrated clients").len(), 2);
        fs::write(&path, br#"{"version":2,"clients":[]}"#).expect("future version");
        assert!(OAuthClientRegistry::at(path).is_err());
    }

    #[test]
    fn failed_publication_never_exposes_a_new_client() {
        let temporary = tempfile::tempdir().expect("temporary state");
        let path = temporary.path().join("oauth-clients.json");
        let registry = OAuthClientRegistry::at(path.clone()).expect("store");
        fs::create_dir(&path).expect("block publication with a directory");
        assert!(registry.register(client("unpublished")).is_err());
        assert!(registry.get("unpublished").is_err());
        fs::remove_dir(&path).expect("remove blocking directory");
        assert!(registry
            .get("unpublished")
            .expect("empty registry")
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_registry_and_lock_files_are_rejected_without_touching_targets() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temporary = tempfile::tempdir().expect("temporary state");
        let target = temporary.path().join("unrelated");
        fs::write(&target, "unrelated contents").expect("target");
        let original_mode = fs::metadata(&target)
            .expect("target metadata")
            .permissions()
            .mode();
        let path = temporary.path().join("oauth-clients.json");
        symlink(&target, &path).expect("registry symlink");
        assert!(OAuthClientRegistry::at(path.clone()).is_err());
        fs::remove_file(&path).expect("remove registry symlink");
        let lock_path = path.with_extension("lock");
        fs::remove_file(&lock_path).expect("remove created lock file");
        symlink(&target, &lock_path).expect("lock symlink");
        assert!(OAuthClientRegistry::at(path).is_err());
        assert_eq!(
            fs::read_to_string(&target).expect("target"),
            "unrelated contents"
        );
        assert_eq!(
            fs::metadata(&target)
                .expect("target metadata")
                .permissions()
                .mode(),
            original_mode
        );
    }
}
