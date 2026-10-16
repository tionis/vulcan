//! Idempotency keys for REST writes (`docs/specs/daemon-rest-api.md`).
//!
//! Each key is one small record under the state directory, written as
//! pending before the write runs and completed with its response after. A
//! replay with the same request returns the stored response; a key reused
//! for a different request, or one whose earlier write never recorded an
//! outcome (the daemon stopped mid-write), is refused rather than run again.
//! Records older than a day are pruned.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const RECORD_VERSION: u32 = 1;
const MAX_KEY_BYTES: usize = 128;
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const RETENTION_MS: u64 = 24 * 60 * 60 * 1_000;
const PRUNE_INTERVAL_MS: u64 = 10 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IdempotencyRecord {
    version: u32,
    fingerprint: String,
    created_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<serde_json::Value>,
}

/// What a write may do with its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyBegin {
    /// The key is new: run the write, then [`IdempotencyStore::finish`] it.
    Run(IdempotencyTicket),
    /// The same request already completed: return its response.
    Replay {
        status: u16,
        body: serde_json::Value,
    },
    /// The key cannot be used for this request.
    Refused(String),
}

/// The right to record the outcome of one key's write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyTicket {
    path: PathBuf,
    fingerprint: String,
    created_unix_ms: u64,
}

#[derive(Debug)]
pub struct IdempotencyStore {
    root: PathBuf,
    last_prune: AtomicU64,
}

impl IdempotencyStore {
    #[must_use]
    pub fn at(state_root: impl AsRef<Path>) -> Self {
        Self {
            root: state_root.as_ref().join("daemon/rest-idempotency"),
            last_prune: AtomicU64::new(0),
        }
    }

    /// Claims `key` in `scope` for a request with `fingerprint`.
    pub fn begin(
        &self,
        scope: &str,
        key: &str,
        fingerprint: &str,
    ) -> Result<IdempotencyBegin, String> {
        validate_key(key)?;
        self.prune_if_due();
        let path = self
            .root
            .join(short_hash(scope, 16))
            .join(format!("{}.json", short_hash(key, 32)));
        let now = unix_time_ms();
        let pending = IdempotencyRecord {
            version: RECORD_VERSION,
            fingerprint: fingerprint.to_string(),
            created_unix_ms: now,
            status: None,
            body: None,
        };
        let bytes = serde_json::to_vec(&pending).map_err(|error| error.to_string())?;
        if vulcan_core::durable::create_new(&path, &bytes, vulcan_core::durable::Durability::Full)
            .map_err(|error| error.to_string())?
        {
            return Ok(IdempotencyBegin::Run(IdempotencyTicket {
                path,
                fingerprint: fingerprint.to_string(),
                created_unix_ms: now,
            }));
        }
        let existing = read_record(&path)?;
        if existing.fingerprint != fingerprint {
            return Ok(IdempotencyBegin::Refused(
                "this Idempotency-Key was already used for a different request".to_string(),
            ));
        }
        match (existing.status, existing.body) {
            (Some(status), Some(body)) => Ok(IdempotencyBegin::Replay { status, body }),
            _ => Ok(IdempotencyBegin::Refused(
                "an earlier request with this Idempotency-Key has not finished, or the daemon stopped before recording its outcome; check the vault before retrying with a new key"
                    .to_string(),
            )),
        }
    }

    /// Records the response a ticket's write produced.
    pub fn finish(
        &self,
        ticket: &IdempotencyTicket,
        status: u16,
        body: &serde_json::Value,
    ) -> Result<(), String> {
        let record = IdempotencyRecord {
            version: RECORD_VERSION,
            fingerprint: ticket.fingerprint.clone(),
            created_unix_ms: ticket.created_unix_ms,
            status: Some(status),
            body: Some(body.clone()),
        };
        let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
        vulcan_core::durable::replace(&ticket.path, &bytes, vulcan_core::durable::Durability::Full)
            .map_err(|error| error.to_string())
    }

    fn prune_if_due(&self) {
        let now = unix_time_ms();
        let last = self.last_prune.load(Ordering::Acquire);
        if now.saturating_sub(last) < PRUNE_INTERVAL_MS
            || self
                .last_prune
                .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let Ok(scopes) = fs::read_dir(&self.root) else {
            return;
        };
        for scope in scopes.flatten() {
            let Ok(records) = fs::read_dir(scope.path()) else {
                continue;
            };
            for record in records.flatten() {
                let expired = read_record(&record.path())
                    .is_ok_and(|record| now.saturating_sub(record.created_unix_ms) > RETENTION_MS);
                if expired {
                    let _ = fs::remove_file(record.path());
                }
            }
        }
    }
}

fn read_record(path: &Path) -> Result<IdempotencyRecord, String> {
    let bytes = vulcan_core::durable::read_bounded(path, MAX_RECORD_BYTES)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the idempotency record disappeared".to_string())?;
    let record: IdempotencyRecord =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if record.version != RECORD_VERSION {
        return Err(format!(
            "unsupported idempotency record version {}",
            record.version
        ));
    }
    Ok(record)
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.len() > MAX_KEY_BYTES
        || !key.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(format!(
            "Idempotency-Key must be 1 to {MAX_KEY_BYTES} visible ASCII characters"
        ));
    }
    Ok(())
}

fn short_hash(value: &str, length: usize) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..length].to_string()
}

/// The fingerprint of a write: its method, path, query, and body.
#[must_use]
pub fn request_fingerprint(method: &str, path_and_query: &str, body: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [method.as_bytes(), path_and_query.as_bytes(), body] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().to_hex().to_string()
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_runs_once_then_replays_its_response() {
        let temporary = tempfile::tempdir().unwrap();
        let store = IdempotencyStore::at(temporary.path());
        let fingerprint = request_fingerprint("POST", "/notes/notes", b"{}");
        let IdempotencyBegin::Run(ticket) = store.begin("scope", "key-1", &fingerprint).unwrap()
        else {
            panic!("a new key runs");
        };
        // Until the outcome is recorded, a retry is refused, not rerun.
        assert!(matches!(
            store.begin("scope", "key-1", &fingerprint).unwrap(),
            IdempotencyBegin::Refused(_)
        ));
        store
            .finish(&ticket, 200, &serde_json::json!({"path": "A.md"}))
            .unwrap();
        assert_eq!(
            store.begin("scope", "key-1", &fingerprint).unwrap(),
            IdempotencyBegin::Replay {
                status: 200,
                body: serde_json::json!({"path": "A.md"})
            }
        );
        let other = request_fingerprint("POST", "/notes/notes", b"{\"path\":\"B\"}");
        assert!(matches!(
            store.begin("scope", "key-1", &other).unwrap(),
            IdempotencyBegin::Refused(reason) if reason.contains("different request")
        ));
        // Keys are scoped to their principal.
        assert!(matches!(
            store.begin("other-scope", "key-1", &other).unwrap(),
            IdempotencyBegin::Run(_)
        ));
    }

    #[test]
    fn keys_are_bounded_visible_text() {
        let temporary = tempfile::tempdir().unwrap();
        let store = IdempotencyStore::at(temporary.path());
        for key in ["", "has space", &"k".repeat(129)] {
            assert!(store.begin("scope", key, "f").is_err(), "{key:?}");
        }
    }

    #[test]
    fn fingerprints_separate_their_parts() {
        assert_ne!(
            request_fingerprint("POST", "/a", b"bc"),
            request_fingerprint("POST", "/ab", b"c")
        );
    }
}
