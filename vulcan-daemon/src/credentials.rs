//! Device-local bearer credentials for companion transports.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fmt::{Display, Formatter, Write as _};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use tempfile::NamedTempFile;

pub const COMPANION_CREDENTIAL_VERSION: u32 = 1;
const CREDENTIAL_FILE: &str = "companion-credential.json";
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
const TOKEN_BYTES: usize = 32;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionCredential {
    pub version: u32,
    pub id: String,
    pub token: String,
    pub allowed_origins: Vec<String>,
}

impl std::fmt::Debug for CompanionCredential {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompanionCredential")
            .field("version", &self.version)
            .field("id", &self.id)
            .field("token", &"[REDACTED]")
            .field("allowed_origins", &self.allowed_origins)
            .finish()
    }
}

impl CompanionCredential {
    pub fn generate(allowed_origins: Vec<String>) -> Result<Self, CredentialError> {
        validate_origins(&allowed_origins)?;
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| CredentialError::Random(error.to_string()))?;
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(token.as_bytes());
        let mut id = String::with_capacity(24);
        for byte in &digest[..12] {
            write!(id, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Ok(Self {
            version: COMPANION_CREDENTIAL_VERSION,
            id,
            token,
            allowed_origins,
        })
    }

    #[must_use]
    pub fn authorizes(&self, candidate: &str) -> bool {
        self.token.as_bytes().ct_eq(candidate.as_bytes()).into()
    }

    #[must_use]
    pub fn allows_origin(&self, origin: Option<&str>) -> bool {
        origin.is_none_or(|origin| self.allowed_origins.iter().any(|allowed| allowed == origin))
    }
}

#[derive(Debug, Clone)]
pub struct CompanionCredentialStore {
    path: PathBuf,
}

impl CompanionCredentialStore {
    #[must_use]
    pub fn at(state_root: impl AsRef<Path>) -> Self {
        Self {
            path: state_root.as_ref().join("daemon").join(CREDENTIAL_FILE),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_or_create(
        &self,
        allowed_origins: Vec<String>,
    ) -> Result<CompanionCredential, CredentialError> {
        match self.load() {
            Ok(credential) => Ok(credential),
            Err(CredentialError::Missing) => {
                let credential = CompanionCredential::generate(allowed_origins)?;
                self.save_new(&credential)?;
                self.load()
            }
            Err(error) => Err(error),
        }
    }

    pub fn load(&self) -> Result<CompanionCredential, CredentialError> {
        let metadata = fs::symlink_metadata(&self.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CredentialError::Missing
            } else {
                CredentialError::Io(error)
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CredentialError::Invalid(format!(
                "companion credential at {} is not a regular file",
                self.path.display()
            )));
        }
        if metadata.len() > MAX_CREDENTIAL_BYTES {
            return Err(CredentialError::Invalid(format!(
                "companion credential exceeds the {MAX_CREDENTIAL_BYTES} byte limit"
            )));
        }
        validate_owner_only(&metadata, &self.path)?;
        let credential: CompanionCredential = serde_json::from_slice(&fs::read(&self.path)?)?;
        validate_credential(&credential)?;
        Ok(credential)
    }

    fn save_new(&self, credential: &CompanionCredential) -> Result<(), CredentialError> {
        let parent = self.path.parent().ok_or_else(|| {
            CredentialError::Invalid("companion credential path has no parent".to_string())
        })?;
        fs::create_dir_all(parent)?;
        let mut temporary = NamedTempFile::new_in(parent)?;
        temporary.write_all(&serde_json::to_vec_pretty(credential)?)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        set_owner_only(temporary.path())?;
        match temporary.persist_noclobber(&self.path) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(CredentialError::Io(error.error)),
        }
    }
}

fn validate_credential(credential: &CompanionCredential) -> Result<(), CredentialError> {
    if credential.version != COMPANION_CREDENTIAL_VERSION {
        return Err(CredentialError::Invalid(format!(
            "unsupported companion credential version {}",
            credential.version
        )));
    }
    if credential.id.len() != 24
        || !credential.id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || credential.token.len() < 40
        || credential.token.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CredentialError::Invalid(
            "companion credential contains an invalid ID or token".to_string(),
        ));
    }
    let digest = Sha256::digest(credential.token.as_bytes());
    let mut expected_id = String::with_capacity(24);
    for byte in &digest[..12] {
        write!(expected_id, "{byte:02x}").expect("writing to a String cannot fail");
    }
    if credential.id != expected_id {
        return Err(CredentialError::Invalid(
            "companion credential ID does not match its token".to_string(),
        ));
    }
    validate_origins(&credential.allowed_origins)
}

fn validate_origins(origins: &[String]) -> Result<(), CredentialError> {
    if origins.len() > 32
        || origins.iter().any(|origin| {
            origin.is_empty()
                || origin.len() > 512
                || origin.bytes().any(|byte| byte.is_ascii_control())
                || !(origin.starts_with("app://")
                    || origin.starts_with("capacitor://")
                    || origin.starts_with("http://localhost")
                    || origin.starts_with("http://127.0.0.1")
                    || origin.starts_with("http://[::1]"))
        })
    {
        return Err(CredentialError::Invalid(
            "allowed companion origins must be bounded app, capacitor, or loopback origins"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(unix)]
fn validate_owner_only(metadata: &fs::Metadata, path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(CredentialError::Invalid(format!(
            "companion credential at {} is accessible by group or other users",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(path: &Path) -> Result<(), CredentialError> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn validate_owner_only(_metadata: &fs::Metadata, _path: &Path) -> Result<(), CredentialError> {
    Ok(())
}

#[derive(Debug)]
pub enum CredentialError {
    Missing,
    Random(String),
    Invalid(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl Display for CredentialError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => formatter.write_str("companion credential does not exist"),
            Self::Random(detail) | Self::Invalid(detail) => formatter.write_str(detail),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for CredentialError {}

impl From<std::io::Error> for CredentialError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CredentialError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generated_credentials_are_strong_scoped_and_constant_time_checked() {
        let credential = CompanionCredential::generate(vec!["app://obsidian.md".to_string()])
            .expect("credential");
        assert_eq!(credential.token.len(), 43);
        assert_eq!(credential.id.len(), 24);
        assert!(credential.authorizes(&credential.token));
        assert!(!credential.authorizes("wrong"));
        assert!(credential.allows_origin(None));
        assert!(credential.allows_origin(Some("app://obsidian.md")));
        assert!(!credential.allows_origin(Some("https://example.com")));
        assert!(!format!("{credential:?}").contains(&credential.token));
    }

    #[test]
    fn store_creation_is_atomic_and_reuses_the_first_credential() {
        let temporary = tempdir().expect("temporary directory");
        let store = CompanionCredentialStore::at(temporary.path());
        let first = store
            .load_or_create(vec!["app://obsidian.md".to_string()])
            .expect("first credential");
        let second = store
            .load_or_create(vec!["http://localhost:3000".to_string()])
            .expect("existing credential");
        assert_eq!(first, second);
        assert_eq!(store.load().expect("load credential"), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.path())
                    .expect("credential metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn unsafe_origins_and_symlinked_stores_fail_closed() {
        assert!(CompanionCredential::generate(vec!["https://example.com".to_string()]).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let temporary = tempdir().expect("temporary directory");
            let store = CompanionCredentialStore::at(temporary.path());
            fs::create_dir_all(store.path().parent().expect("parent")).expect("state directory");
            let target = temporary.path().join("target");
            fs::write(&target, b"{}").expect("target");
            symlink(&target, store.path()).expect("symlink");
            assert!(matches!(store.load(), Err(CredentialError::Invalid(_))));
        }
    }

    #[test]
    #[cfg(unix)]
    fn store_rejects_credentials_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempdir().expect("temporary directory");
        let store = CompanionCredentialStore::at(temporary.path());
        store
            .load_or_create(vec!["app://obsidian.md".to_string()])
            .expect("credential");
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644))
            .expect("loosen permissions");
        assert!(matches!(store.load(), Err(CredentialError::Invalid(_))));
    }
}
