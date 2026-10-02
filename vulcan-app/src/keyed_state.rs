//! Durable keyed state that persists only what changed.
//!
//! Integration state such as Outline mappings holds one entry per synced
//! document and is saved after every remote mutation. Rewriting one file per
//! save made a run quadratic in the number of documents. A [`TrackedMap`]
//! records which keys were mutated, and a [`KeyedStateStore`] writes only those
//! rows in one `SQLite` transaction, so a save costs what it changes.

use crate::AppError;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{btree_map, BTreeMap, BTreeSet};
use std::fs;
use std::ops::Deref;
use std::path::Path;
use std::time::Duration;

const SCHEMA_VERSION: i64 = 1;
/// The meta key whose presence marks a fully initialized store.
const INITIALIZED_KEY: &str = "initialized";

/// A string-keyed map that remembers which keys were inserted, changed, or
/// removed since it was last persisted. Reads go through [`Deref`]; every
/// mutation goes through a method that marks the affected keys.
///
/// A map that was not loaded from a store (default, collected, or
/// deserialized) is a replacement: saving it first clears the stored namespace,
/// so assigning a fresh map never leaves stale rows behind.
#[derive(Clone)]
pub(crate) struct TrackedMap<V> {
    entries: BTreeMap<String, V>,
    dirty: BTreeSet<String>,
    replaced: bool,
}

impl<V> Default for TrackedMap<V> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            dirty: BTreeSet::new(),
            replaced: true,
        }
    }
}

impl<V> TrackedMap<V> {
    /// Wraps entries that already match their persisted rows.
    pub(crate) fn persisted(entries: BTreeMap<String, V>) -> Self {
        Self {
            entries,
            dirty: BTreeSet::new(),
            replaced: false,
        }
    }

    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        let value = self.entries.get_mut(key)?;
        self.dirty.insert(key.to_string());
        Some(value)
    }

    pub(crate) fn insert(&mut self, key: String, value: V) -> Option<V> {
        self.dirty.insert(key.clone());
        self.entries.insert(key, value)
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<V> {
        let removed = self.entries.remove(key)?;
        self.dirty.insert(key.to_string());
        Some(removed)
    }

    pub(crate) fn entry_or_insert_with(
        &mut self,
        key: String,
        value: impl FnOnce() -> V,
    ) -> &mut V {
        self.dirty.insert(key.clone());
        self.entries.entry(key).or_insert_with(value)
    }

    /// Marks every entry dirty. Prefer [`Self::get_mut`] for targeted changes.
    pub(crate) fn values_mut(&mut self) -> btree_map::ValuesMut<'_, String, V> {
        self.dirty.extend(self.entries.keys().cloned());
        self.entries.values_mut()
    }

    pub(crate) fn dirty_keys(&self) -> &BTreeSet<String> {
        &self.dirty
    }

    /// Whether saving must replace the whole stored namespace.
    pub(crate) fn is_replaced(&self) -> bool {
        self.replaced
    }

    /// Records that every change has been persisted.
    pub(crate) fn mark_persisted(&mut self) {
        self.dirty.clear();
        self.replaced = false;
    }
}

impl<V> Deref for TrackedMap<V> {
    type Target = BTreeMap<String, V>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl<V: PartialEq> PartialEq for TrackedMap<V> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<V: Eq> Eq for TrackedMap<V> {}

impl<V: std::fmt::Debug> std::fmt::Debug for TrackedMap<V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.entries.fmt(formatter)
    }
}

impl<V> FromIterator<(String, V)> for TrackedMap<V> {
    /// Collected entries have never been persisted; see [`TrackedMap::is_replaced`].
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self {
            entries: iter.into_iter().collect(),
            dirty: BTreeSet::new(),
            replaced: true,
        }
    }
}

impl<V> IntoIterator for TrackedMap<V> {
    type Item = (String, V);
    type IntoIter = btree_map::IntoIter<String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, V> IntoIterator for &'a TrackedMap<V> {
    type Item = (&'a String, &'a V);
    type IntoIter = btree_map::Iter<'a, String, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl<V: Serialize> Serialize for TrackedMap<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for TrackedMap<V> {
    /// Deserialized entries (for example from a legacy JSON file) have not
    /// been written to a store yet, so the map is a replacement.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(BTreeMap::deserialize(deserializer)?.into_iter().collect())
    }
}

/// A string set with the same change tracking as [`TrackedMap`]. It
/// serializes as a sequence.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct TrackedSet {
    map: TrackedMap<()>,
}

impl TrackedSet {
    pub(crate) fn persisted(values: BTreeSet<String>) -> Self {
        Self {
            map: TrackedMap::persisted(values.into_iter().map(|value| (value, ())).collect()),
        }
    }

    pub(crate) fn remove(&mut self, value: &str) -> bool {
        self.map.remove(value).is_some()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }

    pub(crate) fn tracked(&self) -> &TrackedMap<()> {
        &self.map
    }

    pub(crate) fn mark_persisted(&mut self) {
        self.map.mark_persisted();
    }
}

impl std::fmt::Debug for TrackedSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_set().entries(self.map.keys()).finish()
    }
}

impl FromIterator<String> for TrackedSet {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self {
            map: iter.into_iter().map(|value| (value, ())).collect(),
        }
    }
}

impl Serialize for TrackedSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.map.keys())
    }
}

impl<'de> Deserialize<'de> for TrackedSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(BTreeSet::<String>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

/// One `SQLite` file of durable keyed state: small JSON `meta` values plus
/// namespaced JSON entries. Entries may also hold `claims`, strings that must be
/// unique within a namespace, which the store enforces with a primary key so a
/// save never has to revalidate untouched entries.
pub(crate) struct KeyedStateStore {
    connection: Connection,
}

impl KeyedStateStore {
    /// Opens or creates the store for writing. The caller holds the workflow
    /// lock that serializes writers.
    pub(crate) fn open(path: &Path) -> Result<Self, AppError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        let connection = Connection::open(path).map_err(AppError::operation)?;
        Self::configure(&connection)?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;
                 CREATE TABLE IF NOT EXISTS meta (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS entries (
                     namespace TEXT NOT NULL,
                     key TEXT NOT NULL,
                     value TEXT NOT NULL,
                     PRIMARY KEY (namespace, key)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS claims (
                     namespace TEXT NOT NULL,
                     claim TEXT NOT NULL,
                     key TEXT NOT NULL,
                     PRIMARY KEY (namespace, claim)
                 ) WITHOUT ROWID;
                 CREATE INDEX IF NOT EXISTS claims_by_key ON claims (namespace, key);",
            )
            .map_err(AppError::operation)?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(AppError::operation)?;
        match version {
            0 => connection
                .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
                .map_err(AppError::operation)?,
            SCHEMA_VERSION => {}
            other => {
                return Err(AppError::operation(format!(
                    "unsupported keyed state schema version {other} in {}",
                    path.display()
                )))
            }
        }
        Ok(Self { connection })
    }

    /// Opens an existing store without creating or modifying anything.
    pub(crate) fn open_read_only(path: &Path) -> Result<Option<Self>, AppError> {
        if !path.is_file() {
            return Ok(None);
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(AppError::operation)?;
        Self::configure(&connection)?;
        let store = Self { connection };
        Ok(store.is_initialized()?.then_some(store))
    }

    fn configure(connection: &Connection) -> Result<(), AppError> {
        connection
            .busy_timeout(Duration::from_secs(10))
            .map_err(AppError::operation)
    }

    /// Whether a complete initial write has been committed.
    pub(crate) fn is_initialized(&self) -> Result<bool, AppError> {
        Ok(self.raw_meta(INITIALIZED_KEY)?.is_some())
    }

    fn raw_meta(&self, key: &str) -> Result<Option<String>, AppError> {
        match self
            .connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()
        {
            Ok(value) => Ok(value),
            // A read-only handle on a store created by a newer layout.
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("no such table") =>
            {
                Ok(None)
            }
            Err(error) => Err(AppError::operation(error)),
        }
    }

    pub(crate) fn meta<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, AppError> {
        self.raw_meta(key)?
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| {
                    AppError::operation(format!("malformed keyed state value `{key}`: {error}"))
                })
            })
            .transpose()
    }

    pub(crate) fn load_map<V: DeserializeOwned>(
        &self,
        namespace: &str,
    ) -> Result<TrackedMap<V>, AppError> {
        self.load_entries(namespace).map(TrackedMap::persisted)
    }

    /// The stored entries of `namespace`, for callers that enrich them in
    /// memory (for example with content kept outside the store) before
    /// tracking changes with [`TrackedMap::persisted`].
    pub(crate) fn load_entries<V: DeserializeOwned>(
        &self,
        namespace: &str,
    ) -> Result<BTreeMap<String, V>, AppError> {
        let mut statement = self
            .connection
            .prepare("SELECT key, value FROM entries WHERE namespace = ?1 ORDER BY key")
            .map_err(AppError::operation)?;
        let rows = statement
            .query_map([namespace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(AppError::operation)?;
        let mut entries = BTreeMap::new();
        for row in rows {
            let (key, value) = row.map_err(AppError::operation)?;
            let value = serde_json::from_str(&value).map_err(|error| {
                AppError::operation(format!(
                    "malformed keyed state entry `{namespace}/{key}`: {error}"
                ))
            })?;
            entries.insert(key, value);
        }
        Ok(entries)
    }

    pub(crate) fn load_set(&self, namespace: &str) -> Result<TrackedSet, AppError> {
        Ok(TrackedSet::persisted(
            self.load_map::<()>(namespace)?
                .into_iter()
                .map(|(key, ())| key)
                .collect(),
        ))
    }

    /// Runs `write` in one transaction, committing only when it succeeds.
    pub(crate) fn write(
        &mut self,
        write: impl FnOnce(&KeyedStateWriter<'_>) -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        let transaction = self.connection.transaction().map_err(AppError::operation)?;
        let writer = KeyedStateWriter {
            transaction: &transaction,
        };
        write(&writer)?;
        transaction.commit().map_err(AppError::operation)
    }
}

pub(crate) struct KeyedStateWriter<'a> {
    transaction: &'a rusqlite::Transaction<'a>,
}

impl KeyedStateWriter<'_> {
    pub(crate) fn set_meta<T: Serialize>(&self, key: &str, value: &T) -> Result<(), AppError> {
        let value = serde_json::to_string(value).map_err(AppError::operation)?;
        self.transaction
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(AppError::operation)?;
        Ok(())
    }

    pub(crate) fn delete_meta(&self, key: &str) -> Result<(), AppError> {
        self.transaction
            .execute("DELETE FROM meta WHERE key = ?1", [key])
            .map_err(AppError::operation)?;
        Ok(())
    }

    /// Marks the store as completely written; see [`KeyedStateStore::is_initialized`].
    pub(crate) fn mark_initialized(&self) -> Result<(), AppError> {
        self.set_meta(INITIALIZED_KEY, &true)
    }

    pub(crate) fn clear_namespace(&self, namespace: &str) -> Result<(), AppError> {
        for table in ["entries", "claims"] {
            self.transaction
                .execute(
                    &format!("DELETE FROM {table} WHERE namespace = ?1"),
                    [namespace],
                )
                .map_err(AppError::operation)?;
        }
        Ok(())
    }

    /// Writes the changes of `map`: dirty keys that are present are upserted
    /// with their claims and absent ones are deleted; a replaced map rewrites
    /// the namespace. A claim already held by another key fails the write with
    /// `conflict`.
    pub(crate) fn write_dirty<V: Serialize>(
        &self,
        namespace: &str,
        map: &TrackedMap<V>,
        claims: impl Fn(&V) -> Vec<String>,
        conflict: &str,
    ) -> Result<(), AppError> {
        self.write_dirty_as(namespace, map, serde_json::to_string, claims, conflict)
    }

    /// As [`Self::write_dirty`], storing `encode(value)` instead of the value's
    /// own JSON.
    pub(crate) fn write_dirty_as<V>(
        &self,
        namespace: &str,
        map: &TrackedMap<V>,
        encode: impl Fn(&V) -> serde_json::Result<String>,
        claims: impl Fn(&V) -> Vec<String>,
        conflict: &str,
    ) -> Result<(), AppError> {
        let keys: Box<dyn Iterator<Item = &String>> = if map.is_replaced() {
            self.clear_namespace(namespace)?;
            Box::new(map.keys())
        } else {
            // Release every dirty key's claims first so entries can swap claims.
            for key in map.dirty_keys() {
                self.transaction
                    .execute(
                        "DELETE FROM claims WHERE namespace = ?1 AND key = ?2",
                        params![namespace, key],
                    )
                    .map_err(AppError::operation)?;
            }
            Box::new(map.dirty_keys().iter())
        };
        for key in keys {
            let Some(value) = map.get(key) else {
                self.transaction
                    .execute(
                        "DELETE FROM entries WHERE namespace = ?1 AND key = ?2",
                        params![namespace, key],
                    )
                    .map_err(AppError::operation)?;
                continue;
            };
            let encoded = encode(value).map_err(AppError::operation)?;
            self.transaction
                .execute(
                    "INSERT INTO entries (namespace, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT (namespace, key) DO UPDATE SET value = excluded.value",
                    params![namespace, key, encoded],
                )
                .map_err(AppError::operation)?;
            for claim in claims(value) {
                match self.transaction.execute(
                    "INSERT INTO claims (namespace, claim, key) VALUES (?1, ?2, ?3)",
                    params![namespace, claim, key],
                ) {
                    Ok(_) => {}
                    Err(rusqlite::Error::SqliteFailure(error, _))
                        if error.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        return Err(AppError::operation(conflict));
                    }
                    Err(error) => return Err(AppError::operation(error)),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn claims(value: &String) -> Vec<String> {
        vec![value.to_string()]
    }

    #[test]
    fn tracked_map_marks_only_mutated_keys() {
        let mut map = TrackedMap::persisted(BTreeMap::from([
            ("a".to_string(), 1),
            ("b".to_string(), 2),
            ("c".to_string(), 3),
        ]));
        assert!(map.dirty_keys().is_empty());
        *map.get_mut("a").expect("a") += 10;
        map.remove("b");
        map.insert("d".to_string(), 4);
        assert!(map.get_mut("missing").is_none());
        map.remove("c");
        assert_eq!(
            map.dirty_keys()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        let collected = [("x".to_string(), 1)]
            .into_iter()
            .collect::<TrackedMap<_>>();
        assert!(collected.is_replaced());
        assert!(!map.is_replaced());
    }

    #[test]
    fn store_persists_only_dirty_rows_and_enforces_claims() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("state.sqlite");
        let mut store = KeyedStateStore::open(&path).expect("store");
        assert!(!store.is_initialized().expect("initialized"));
        let mut map = [
            ("one".to_string(), "remote-1".to_string()),
            ("two".to_string(), "remote-2".to_string()),
        ]
        .into_iter()
        .collect::<TrackedMap<_>>();
        store
            .write(|writer| {
                writer.set_meta("profile", &"wiki")?;
                writer.write_dirty("docs", &map, claims, "duplicate remote")?;
                writer.mark_initialized()
            })
            .expect("initial write");
        map.mark_persisted();

        // Swapping claims between two entries is not a conflict.
        *map.get_mut("one").expect("one") = "remote-2".to_string();
        *map.get_mut("two").expect("two") = "remote-1".to_string();
        store
            .write(|writer| writer.write_dirty("docs", &map, claims, "duplicate remote"))
            .expect("swap claims");
        map.mark_persisted();

        map.insert("three".to_string(), "remote-1".to_string());
        let error = store
            .write(|writer| writer.write_dirty("docs", &map, claims, "duplicate remote"))
            .expect_err("duplicate claim");
        assert!(error.to_string().contains("duplicate remote"));
        map.remove("three");
        map.remove("two");
        store
            .write(|writer| writer.write_dirty("docs", &map, claims, "duplicate remote"))
            .expect("remove rows");
        drop(store);

        let reader = KeyedStateStore::open_read_only(&path)
            .expect("read only")
            .expect("initialized store");
        assert_eq!(
            reader.meta::<String>("profile").expect("meta").as_deref(),
            Some("wiki")
        );
        let loaded = reader.load_map::<String>("docs").expect("load");
        assert_eq!(
            *loaded,
            BTreeMap::from([("one".to_string(), "remote-2".to_string())])
        );
        assert!(loaded.dirty_keys().is_empty());
    }

    #[test]
    fn read_only_open_ignores_missing_and_unfinished_stores() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("state.sqlite");
        assert!(KeyedStateStore::open_read_only(&path)
            .expect("missing")
            .is_none());
        let mut store = KeyedStateStore::open(&path).expect("store");
        store
            .write(|writer| writer.set_meta("profile", &"wiki"))
            .expect("partial write");
        assert!(KeyedStateStore::open_read_only(&path)
            .expect("unfinished")
            .is_none());
    }
}
