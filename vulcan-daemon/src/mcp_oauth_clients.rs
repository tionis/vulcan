//! Durable dynamic OAuth client registrations shared by MCP hosting modes.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::NamedTempFile;

const REGISTRY_VERSION: u32 = 1;
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RegisteredOAuthClient {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
    pub token_endpoint_auth_method: String,
    pub client_id_issued_at: u64,
}

#[derive(Debug)]
pub enum OAuthClientRegistryError {
    Io(io::Error),
    Json(serde_json::Error),
    Invalid(String),
}

impl std::fmt::Display for OAuthClientRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "OAuth client registry I/O error: {error}"),
            Self::Json(error) => write!(formatter, "OAuth client registry JSON error: {error}"),
            Self::Invalid(error) => write!(formatter, "invalid OAuth client registry: {error}"),
        }
    }
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
    ephemeral: Mutex<BTreeMap<String, RegisteredOAuthClient>>,
}

impl OAuthClientRegistry {
    /// Use a durable, cross-process registry. Existing bare-array files are read without mutation.
    pub fn at(path: PathBuf) -> Result<Self, OAuthClientRegistryError> {
        let _lock = RegistryLock::acquire(&path)?;
        let _ = load_clients(&path)?;
        Ok(Self {
            path: Some(path),
            ephemeral: Mutex::new(BTreeMap::new()),
        })
    }

    /// Keep direct test and temporary issuer state in memory without creating device files.
    #[must_use]
    pub fn ephemeral() -> Self {
        Self {
            path: None,
            ephemeral: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn get(
        &self,
        client_id: &str,
    ) -> Result<Option<RegisteredOAuthClient>, OAuthClientRegistryError> {
        if let Some(path) = &self.path {
            let _lock = RegistryLock::acquire(path)?;
            return Ok(load_clients(path)?.remove(client_id));
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
            return Ok(load_clients(path)?.into_values().collect());
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
            let mut clients = load_clients(path)?;
            if clients.contains_key(&client.client_id) {
                return Err(OAuthClientRegistryError::Invalid(format!(
                    "duplicate client ID `{}`",
                    client.client_id
                )));
            }
            clients.insert(client.client_id.clone(), client);
            return save_clients(path, &clients);
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

fn load_clients(
    path: &Path,
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
    let clients = if bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        == Some(b'[')
    {
        serde_json::from_slice::<Vec<RegisteredOAuthClient>>(&bytes)?
    } else {
        let file = serde_json::from_slice::<RegistryFile>(&bytes)?;
        if file.version != REGISTRY_VERSION {
            return Err(OAuthClientRegistryError::Invalid(format!(
                "unsupported version {}",
                file.version
            )));
        }
        file.clients
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

fn save_clients(
    path: &Path,
    clients: &BTreeMap<String, RegisteredOAuthClient>,
) -> Result<(), OAuthClientRegistryError> {
    let parent = path.parent().ok_or_else(|| {
        OAuthClientRegistryError::Invalid("registry path has no parent".to_string())
    })?;
    let file = RegistryFile {
        version: REGISTRY_VERSION,
        clients: clients.values().cloned().collect(),
    };
    let serialized = serde_json::to_vec_pretty(&file)?;
    if serialized.len() as u64 > MAX_REGISTRY_BYTES {
        return Err(OAuthClientRegistryError::Invalid(
            "registry exceeds the 1 MiB limit".to_string(),
        ));
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&serialized)?;
    temporary.as_file().sync_all()?;
    set_owner_only(temporary.as_file())?;
    temporary
        .persist(path)
        .map_err(|error| OAuthClientRegistryError::Io(error.error))?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
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

#[cfg(not(unix))]
fn require_owner_only(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), OAuthClientRegistryError> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(file: &File) -> Result<(), OAuthClientRegistryError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_file: &File) -> Result<(), OAuthClientRegistryError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
