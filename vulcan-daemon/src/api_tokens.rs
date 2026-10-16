//! Per-vault API tokens for the daemon REST API (Roadmap 10.2).
//!
//! A token is `vat_<token-id>_<secret>`. The store, outside vault content in
//! the device-local state directory, keeps only the BLAKE3 hash of each
//! secret: the secrets are 256 random bits, so a slow password hash would add
//! cost without adding resistance (see `docs/specs/daemon-rest-api.md`).
//! Tokens bind to a registration ID, so a re-added wiki with the same ID does
//! not revive them.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use ulid::Ulid;

pub const API_TOKEN_STORE_VERSION: u32 = 1;
pub const API_TOKEN_PREFIX: &str = "vat_";
const STORE_FILE: &str = "api-tokens.json";
const LOCK_FILE: &str = "api-tokens.lock";
const MAX_STORE_BYTES: u64 = 1024 * 1024;
const MAX_TOKENS: usize = 1024;
const MAX_LABEL_CHARS: usize = 128;
const SECRET_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    tokens: Vec<ApiTokenRecord>,
}

/// One stored token. The hash never leaves the store: listings use
/// [`ApiTokenSummary`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiTokenRecord {
    pub id: Ulid,
    pub registration_id: Ulid,
    /// The wiki ID when the token was created, for listings of orphaned tokens.
    pub wiki_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_unix_ms: u64,
    secret_blake3: String,
}

impl std::fmt::Debug for ApiTokenRecord {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiTokenRecord")
            .field("id", &self.id)
            .field("registration_id", &self.registration_id)
            .field("wiki_id", &self.wiki_id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiTokenSummary {
    pub id: Ulid,
    pub wiki_id: String,
    pub registration_id: Ulid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_unix_ms: u64,
}

impl From<&ApiTokenRecord> for ApiTokenSummary {
    fn from(record: &ApiTokenRecord) -> Self {
        Self {
            id: record.id,
            wiki_id: record.wiki_id.clone(),
            registration_id: record.registration_id,
            label: record.label.clone(),
            created_unix_ms: record.created_unix_ms,
        }
    }
}

/// A newly created token. `token` is the only copy of the secret; a dry run
/// leaves it out.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct CreatedApiToken {
    #[serde(flatten)]
    pub summary: ApiTokenSummary,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl std::fmt::Debug for CreatedApiToken {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedApiToken")
            .field("summary", &self.summary)
            .field("dry_run", &self.dry_run)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ApiTokenStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl ApiTokenStore {
    #[must_use]
    pub fn at(state_root: impl AsRef<Path>) -> Self {
        let directory = state_root.as_ref().join("daemon");
        Self {
            path: directory.join(STORE_FILE),
            lock_path: directory.join(LOCK_FILE),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every stored token, oldest first.
    pub fn list(&self) -> Result<Vec<ApiTokenRecord>, ApiTokenError> {
        Ok(self.read()?.tokens)
    }

    /// Creates a token for one registration and returns its secret once.
    pub fn create(
        &self,
        registration_id: Ulid,
        wiki_id: &str,
        label: Option<&str>,
        dry_run: bool,
    ) -> Result<CreatedApiToken, ApiTokenError> {
        let label = label.map(validate_label).transpose()?;
        let mut secret = [0_u8; SECRET_BYTES];
        getrandom::fill(&mut secret).map_err(|error| ApiTokenError::Random(error.to_string()))?;
        let secret = URL_SAFE_NO_PAD.encode(secret);
        let record = ApiTokenRecord {
            id: Ulid::new(),
            registration_id,
            wiki_id: wiki_id.to_string(),
            label,
            created_unix_ms: unix_time_ms(),
            secret_blake3: blake3::hash(secret.as_bytes()).to_hex().to_string(),
        };
        let summary = ApiTokenSummary::from(&record);
        let token = format!("{API_TOKEN_PREFIX}{}_{secret}", record.id);
        if dry_run {
            return Ok(CreatedApiToken {
                summary,
                dry_run,
                token: None,
            });
        }
        self.mutate(|tokens| {
            if tokens.len() >= MAX_TOKENS {
                return Err(ApiTokenError::Invalid(format!(
                    "at most {MAX_TOKENS} API tokens may exist"
                )));
            }
            tokens.push(record);
            Ok(())
        })?;
        Ok(CreatedApiToken {
            summary,
            dry_run,
            token: Some(token),
        })
    }

    /// Deletes one token. Its holder is refused from the next request on.
    pub fn revoke(&self, id: Ulid, dry_run: bool) -> Result<ApiTokenSummary, ApiTokenError> {
        if dry_run {
            return self
                .read()?
                .tokens
                .iter()
                .find(|token| token.id == id)
                .map(ApiTokenSummary::from)
                .ok_or(ApiTokenError::UnknownToken(id));
        }
        let mut revoked = None;
        self.mutate(|tokens| {
            let position = tokens
                .iter()
                .position(|token| token.id == id)
                .ok_or(ApiTokenError::UnknownToken(id))?;
            revoked = Some(ApiTokenSummary::from(&tokens.remove(position)));
            Ok(())
        })?;
        Ok(revoked.expect("a revoked token was recorded"))
    }

    /// The record a presented bearer token proves, if any. The secret is
    /// compared by hash in constant time.
    pub fn authenticate(&self, presented: &str) -> Result<Option<ApiTokenRecord>, ApiTokenError> {
        let Some((id, secret)) = parse_token(presented) else {
            return Ok(None);
        };
        let expected = blake3::hash(secret.as_bytes()).to_hex().to_string();
        Ok(self.read()?.tokens.into_iter().find(|token| {
            token.id == id && bool::from(token.secret_blake3.as_bytes().ct_eq(expected.as_bytes()))
        }))
    }

    fn read(&self) -> Result<StoreFile, ApiTokenError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StoreFile {
                    version: API_TOKEN_STORE_VERSION,
                    tokens: Vec::new(),
                });
            }
            Err(error) => return Err(ApiTokenError::Io(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ApiTokenError::Invalid(format!(
                "API token store at {} is not a regular file",
                self.path.display()
            )));
        }
        if metadata.len() > MAX_STORE_BYTES {
            return Err(ApiTokenError::Invalid(format!(
                "API token store exceeds the {MAX_STORE_BYTES} byte limit"
            )));
        }
        validate_owner_only(&metadata, &self.path)?;
        let store: StoreFile = serde_json::from_slice(&fs::read(&self.path)?)
            .map_err(|error| ApiTokenError::Invalid(format!("API token store: {error}")))?;
        if store.version != API_TOKEN_STORE_VERSION {
            return Err(ApiTokenError::Invalid(format!(
                "unsupported API token store version {}",
                store.version
            )));
        }
        if store.tokens.len() > MAX_TOKENS
            || store
                .tokens
                .iter()
                .any(|token| token.secret_blake3.len() != 64)
        {
            return Err(ApiTokenError::Invalid(
                "API token store holds an invalid token record".to_string(),
            ));
        }
        Ok(store)
    }

    fn mutate(
        &self,
        change: impl FnOnce(&mut Vec<ApiTokenRecord>) -> Result<(), ApiTokenError>,
    ) -> Result<(), ApiTokenError> {
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock_path)?;
        lock.lock_exclusive()?;
        let mut store = self.read()?;
        change(&mut store.tokens)?;
        let mut bytes = serde_json::to_vec_pretty(&store)
            .map_err(|error| ApiTokenError::Invalid(error.to_string()))?;
        bytes.push(b'\n');
        // Unix files are created owner-only; on Windows the file inherits the
        // per-user state directory's ACL, which `read` verifies.
        vulcan_core::durable::replace(&self.path, &bytes, vulcan_core::durable::Durability::Full)?;
        Ok(())
    }
}

/// Splits `vat_<ulid>_<secret>` into its ID and secret.
fn parse_token(presented: &str) -> Option<(Ulid, &str)> {
    let rest = presented.strip_prefix(API_TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    let id = Ulid::from_string(id).ok()?;
    (secret.len() >= 40
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then_some((id, secret))
}

/// Whether a bearer value has the shape of an API token, so the caller can
/// tell it from a companion credential.
#[must_use]
pub fn looks_like_api_token(presented: &str) -> bool {
    presented.starts_with(API_TOKEN_PREFIX)
}

fn validate_label(label: &str) -> Result<String, ApiTokenError> {
    let label = label.trim();
    if label.is_empty()
        || label.chars().count() > MAX_LABEL_CHARS
        || label.chars().any(char::is_control)
    {
        return Err(ApiTokenError::Invalid(format!(
            "API token labels are 1 to {MAX_LABEL_CHARS} characters without control characters"
        )));
    }
    Ok(label.to_string())
}

#[cfg(unix)]
fn validate_owner_only(metadata: &fs::Metadata, path: &Path) -> Result<(), ApiTokenError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(ApiTokenError::Invalid(format!(
            "API token store at {} is accessible by group or other users",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_owner_only(_metadata: &fs::Metadata, path: &Path) -> Result<(), ApiTokenError> {
    vulcan_app::windows_acl::verify_private_path(path).map_err(|error| {
        ApiTokenError::Invalid(format!(
            "API token store at {} is not private to the current user: {error}",
            path.display()
        ))
    })
}

#[cfg(not(any(unix, windows)))]
#[allow(clippy::unnecessary_wraps)]
fn validate_owner_only(_metadata: &fs::Metadata, _path: &Path) -> Result<(), ApiTokenError> {
    Ok(())
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[derive(Debug)]
pub enum ApiTokenError {
    UnknownToken(Ulid),
    Random(String),
    Invalid(String),
    Io(std::io::Error),
}

impl Display for ApiTokenError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownToken(id) => write!(formatter, "unknown API token `{id}`"),
            Self::Random(detail) | Self::Invalid(detail) => formatter.write_str(detail),
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ApiTokenError {}

impl From<std::io::Error> for ApiTokenError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_created_token_authenticates_until_it_is_revoked() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ApiTokenStore::at(temporary.path());
        let registration = Ulid::new();
        let created = store
            .create(registration, "notes", Some("phone"), false)
            .unwrap();
        let token = created.token.clone().unwrap();
        assert!(token.starts_with("vat_"));
        assert!(!format!("{created:?}").contains(&token));

        let record = store.authenticate(&token).unwrap().unwrap();
        assert_eq!(record.registration_id, registration);
        assert_eq!(record.label.as_deref(), Some("phone"));
        let stored = fs::read_to_string(store.path()).unwrap();
        let secret = token.rsplit_once('_').unwrap().1;
        assert!(!stored.contains(secret), "the secret is never stored");

        let mut forged = token.clone();
        forged.pop();
        forged.push(if token.ends_with('A') { 'B' } else { 'A' });
        assert!(store.authenticate(&forged).unwrap().is_none());
        assert!(store.authenticate("not-a-token").unwrap().is_none());

        store.revoke(created.summary.id, false).unwrap();
        assert!(store.authenticate(&token).unwrap().is_none());
        assert!(matches!(
            store.revoke(created.summary.id, false),
            Err(ApiTokenError::UnknownToken(_))
        ));
    }

    #[test]
    fn dry_runs_neither_store_nor_reveal_a_token() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ApiTokenStore::at(temporary.path());
        let preview = store.create(Ulid::new(), "notes", None, true).unwrap();
        assert!(preview.token.is_none());
        assert!(store.list().unwrap().is_empty());

        let created = store.create(Ulid::new(), "notes", None, false).unwrap();
        store.revoke(created.summary.id, true).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn labels_are_bounded_single_line_text() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ApiTokenStore::at(temporary.path());
        for label in ["", "line\nbreak", &"x".repeat(129)] {
            assert!(store
                .create(Ulid::new(), "notes", Some(label), false)
                .is_err());
        }
        assert!(store.list().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_store_readable_by_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let store = ApiTokenStore::at(temporary.path());
        let created = store.create(Ulid::new(), "notes", None, false).unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store
            .authenticate(created.token.as_deref().unwrap())
            .is_err());
    }
}
