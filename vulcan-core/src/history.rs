use crate::{CacheDatabase, CacheError, VaultPaths};
use blake3::Hasher;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;

const STALE_AGE_SECS: i64 = 180 * 24 * 60 * 60;
const MAX_AUTOMATIC_SCAN_CHECKPOINTS: usize = 24;

#[derive(Debug)]
pub enum CheckpointError {
    Cache(CacheError),
    CacheMissing,
    InvalidName(String),
    NotFound { name: String },
    Sqlite(rusqlite::Error),
    Time(std::time::SystemTimeError),
}

impl Display for CheckpointError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before using checkpoints")
            }
            Self::InvalidName(name) => write!(
                formatter,
                "checkpoint names must be ASCII letters, digits, '-', or '_': {name}"
            ),
            Self::NotFound { name } => write!(formatter, "checkpoint not found: {name}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::Time(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for CheckpointError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Time(error) => Some(error),
            Self::CacheMissing | Self::InvalidName(_) | Self::NotFound { .. } => None,
        }
    }
}

impl From<CacheError> for CheckpointError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<rusqlite::Error> for CheckpointError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<std::time::SystemTimeError> for CheckpointError {
    fn from(error: std::time::SystemTimeError) -> Self {
        Self::Time(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeAnchor {
    LastScan,
    Checkpoint(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Added,
    Updated,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Note,
    Link,
    Property,
    Embedding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeItem {
    pub path: String,
    pub status: ChangeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeReport {
    pub anchor: String,
    pub notes: Vec<ChangeItem>,
    pub links: Vec<ChangeItem>,
    pub properties: Vec<ChangeItem>,
    pub embeddings: Vec<ChangeItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointRecord {
    pub id: String,
    pub name: Option<String>,
    pub source: String,
    pub created_at: i64,
    pub note_count: usize,
    pub orphan_notes: usize,
    pub stale_notes: usize,
    pub resolved_links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphTrendsReport {
    pub points: Vec<GraphTrendPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphTrendPoint {
    pub label: String,
    pub source: String,
    pub created_at: i64,
    pub note_count: usize,
    pub orphan_notes: usize,
    pub stale_notes: usize,
    pub resolved_links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DocumentState {
    path: String,
    document_kind: String,
    content_hash: String,
    link_hash: String,
    property_hash: String,
    embedding_hash: String,
    orphan: bool,
    stale: bool,
}

#[derive(Debug, Clone)]
struct SnapshotState {
    records: Vec<CheckpointRecord>,
    documents: Vec<DocumentState>,
}

pub fn create_checkpoint(
    paths: &VaultPaths,
    name: &str,
) -> Result<CheckpointRecord, CheckpointError> {
    validate_checkpoint_name(name)?;
    let mut database = open_existing_cache(paths)?;
    database.with_transaction(|transaction| {
        transaction.execute("DELETE FROM checkpoints WHERE name = ?1", [name])?;
        insert_checkpoint_snapshot(transaction, Some(name), "manual")
    })
}

pub fn list_checkpoints(paths: &VaultPaths) -> Result<Vec<CheckpointRecord>, CheckpointError> {
    let database = open_existing_cache(paths)?;
    load_checkpoint_records(database.connection())
}

pub fn query_graph_trends(
    paths: &VaultPaths,
    limit: usize,
) -> Result<GraphTrendsReport, CheckpointError> {
    let database = open_existing_cache(paths)?;
    let mut records = load_checkpoint_records(database.connection())?;
    if limit > 0 && records.len() > limit {
        records.truncate(limit);
    }
    records.reverse();

    Ok(GraphTrendsReport {
        points: records
            .into_iter()
            .map(|record| GraphTrendPoint {
                label: record
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("{}:{}", record.source, record.created_at)),
                source: record.source,
                created_at: record.created_at,
                note_count: record.note_count,
                orphan_notes: record.orphan_notes,
                stale_notes: record.stale_notes,
                resolved_links: record.resolved_links,
            })
            .collect(),
    })
}

pub fn query_change_report(
    paths: &VaultPaths,
    anchor: &ChangeAnchor,
) -> Result<ChangeReport, CheckpointError> {
    let database = open_existing_cache(paths)?;
    let current = load_document_states(database.connection(), current_unix_timestamp()?)?;
    let baseline = load_anchor_snapshot(database.connection(), anchor)?;
    let current_map = current
        .into_iter()
        .map(|state| (state.path.clone(), state))
        .collect::<HashMap<_, _>>();
    let baseline_map = baseline
        .documents
        .into_iter()
        .map(|state| (state.path.clone(), state))
        .collect::<HashMap<_, _>>();
    let all_paths = current_map
        .keys()
        .chain(baseline_map.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut notes = Vec::new();
    let mut links = Vec::new();
    let mut properties = Vec::new();
    let mut embeddings = Vec::new();

    for path in all_paths {
        let old = baseline_map.get(&path);
        let new = current_map.get(&path);

        if let Some(status) = diff_category(
            old.map(|state| state.content_hash.as_str()),
            new.map(|state| state.content_hash.as_str()),
            true,
        ) {
            notes.push(ChangeItem {
                path: path.clone(),
                status,
            });
        }
        if let Some(status) = diff_category(
            old.map(|state| state.link_hash.as_str()),
            new.map(|state| state.link_hash.as_str()),
            false,
        ) {
            links.push(ChangeItem {
                path: path.clone(),
                status,
            });
        }
        if let Some(status) = diff_category(
            old.map(|state| state.property_hash.as_str()),
            new.map(|state| state.property_hash.as_str()),
            false,
        ) {
            properties.push(ChangeItem {
                path: path.clone(),
                status,
            });
        }
        if let Some(status) = diff_category(
            old.map(|state| state.embedding_hash.as_str()),
            new.map(|state| state.embedding_hash.as_str()),
            false,
        ) {
            embeddings.push(ChangeItem { path, status });
        }
    }

    Ok(ChangeReport {
        anchor: match anchor {
            ChangeAnchor::LastScan => baseline
                .records
                .first()
                .and_then(|record| record.name.clone())
                .unwrap_or_else(|| "last_scan".to_string()),
            ChangeAnchor::Checkpoint(name) => name.clone(),
        },
        notes,
        links,
        properties,
        embeddings,
    })
}

pub(crate) fn record_scan_checkpoint(connection: &Connection) -> Result<(), CheckpointError> {
    let transaction = connection.unchecked_transaction()?;
    insert_full_scan_checkpoint(&transaction)?;
    prune_automatic_scan_checkpoints(&transaction)?;
    transaction.commit()?;
    Ok(())
}

/// Record a scan checkpoint touching only documents whose state can have
/// changed since the previous one: tracked path, link, orphan, and property
/// changes, the given IDs, vector-input changes, and documents whose age
/// crossed the staleness threshold since then. Falls back to a full snapshot
/// when tracking is not continuous from the newest generation.
pub(crate) fn record_scan_checkpoint_incremental(
    connection: &Connection,
    changed_document_ids: &[String],
) -> Result<(), CheckpointError> {
    let transaction = connection.unchecked_transaction()?;
    match previous_scan_generation(&transaction)? {
        Some(previous) => insert_incremental_scan(&transaction, &previous, changed_document_ids)?,
        None => insert_full_scan_checkpoint(&transaction)?,
    }
    prune_automatic_scan_checkpoints(&transaction)?;
    transaction.commit()?;
    Ok(())
}

fn insert_full_scan_checkpoint(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), CheckpointError> {
    let now = current_unix_timestamp()?;
    let snapshot = build_snapshot_state_at(transaction, now)?;
    insert_snapshot(transaction, None, "scan", snapshot)?;
    finish_scan_checkpoint(transaction, now)
}

/// Close out change tracking for the checkpoint just written at `now`.
fn finish_scan_checkpoint(
    transaction: &rusqlite::Transaction<'_>,
    now: i64,
) -> Result<(), CheckpointError> {
    reconcile_vector_inputs(transaction)?;
    transaction.execute_batch(
        "DELETE FROM checkpoint_dirty_documents;
         DELETE FROM checkpoint_link_dirty_documents;
         DELETE FROM checkpoint_path_dirty;
         DELETE FROM checkpoint_orphan_dirty_documents;
         DELETE FROM meta WHERE key = 'checkpoint_reset';",
    )?;
    transaction.execute(
        "INSERT INTO meta(key, value) VALUES ('checkpoint_clock', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [now.to_string()],
    )?;
    Ok(())
}

/// The newest scan generation and the clock its staleness was evaluated at.
struct PreviousScan {
    generation: i64,
    clock: i64,
    note_count: i64,
    orphan_notes: i64,
    stale_notes: i64,
}

/// The newest automatic generation, when change tracking has been
/// continuous since it was written.
fn previous_scan_generation(
    connection: &Connection,
) -> Result<Option<PreviousScan>, CheckpointError> {
    let tracked: bool = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM meta WHERE key = 'checkpoint_reset')",
        [],
        |row| row.get(0),
    )?;
    let clock: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'checkpoint_clock'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(clock) = clock.and_then(|clock| clock.parse::<i64>().ok()) else {
        return Ok(None);
    };
    if !tracked {
        return Ok(None);
    }
    // The newest scan header must be the newest generation.
    Ok(connection
        .query_row(
            "SELECT generation, note_count, orphan_notes, stale_notes FROM checkpoints
             WHERE source = 'scan' ORDER BY created_at DESC, id DESC LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .and_then(|(generation, note_count, orphan_notes, stale_notes)| {
            Some(PreviousScan {
                generation: generation?,
                clock,
                note_count,
                orphan_notes,
                stale_notes,
            })
        })
        .filter(|previous| {
            connection
                .query_row("SELECT max(generation) FROM checkpoints", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })
                .ok()
                .flatten()
                == Some(previous.generation)
        }))
}

fn insert_checkpoint_snapshot(
    transaction: &rusqlite::Transaction<'_>,
    name: Option<&str>,
    source: &str,
) -> Result<CheckpointRecord, CheckpointError> {
    let snapshot = build_snapshot_state(transaction)?;
    insert_snapshot(transaction, name, source, snapshot)
}

fn insert_snapshot(
    transaction: &rusqlite::Transaction<'_>,
    name: Option<&str>,
    source: &str,
    snapshot: SnapshotState,
) -> Result<CheckpointRecord, CheckpointError> {
    let record = snapshot
        .records
        .into_iter()
        .next()
        .expect("snapshot state should include one record");
    let record = insert_checkpoint_header(transaction, name, source, &record)?;
    if source == "scan" {
        insert_scan_versions(transaction, &record.id, &snapshot.documents)?;
    } else {
        let mut statement = transaction.prepare(
            "
        INSERT INTO checkpoint_documents (
            checkpoint_id,
            path,
            document_kind,
            content_hash,
            link_hash,
            property_hash,
            embedding_hash,
            orphan,
            stale
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ",
        )?;
        for state in snapshot.documents {
            statement.execute(params![
                &record.id,
                &state.path,
                &state.document_kind,
                &state.content_hash,
                &state.link_hash,
                &state.property_hash,
                &state.embedding_hash,
                i64::from(state.orphan),
                i64::from(state.stale),
            ])?;
        }
    }
    Ok(record)
}

fn insert_checkpoint_header(
    transaction: &rusqlite::Transaction<'_>,
    name: Option<&str>,
    source: &str,
    record: &CheckpointRecord,
) -> Result<CheckpointRecord, CheckpointError> {
    let (created_at, checkpoint_id) = checkpoint_identity(transaction)?;
    transaction.execute(
        "
        INSERT INTO checkpoints (
            id,
            name,
            source,
            created_at,
            note_count,
            orphan_notes,
            stale_notes,
            resolved_links
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ",
        params![
            &checkpoint_id,
            name,
            source,
            created_at,
            i64::try_from(record.note_count).unwrap_or(i64::MAX),
            i64::try_from(record.orphan_notes).unwrap_or(i64::MAX),
            i64::try_from(record.stale_notes).unwrap_or(i64::MAX),
            i64::try_from(record.resolved_links).unwrap_or(i64::MAX),
        ],
    )?;

    Ok(CheckpointRecord {
        id: checkpoint_id,
        name: name.map(ToOwned::to_owned),
        source: source.to_string(),
        created_at,
        ..record.clone()
    })
}

fn checkpoint_identity(connection: &Connection) -> Result<(i64, String), CheckpointError> {
    let mut created_at = current_unix_timestamp()?;
    let mut checkpoint_id = Ulid::new();
    // Keep public ordering consistent with insertion order, including captures
    // in the same millisecond and a wall clock that moves backwards. Retention
    // must never discard the newest generation ahead of its ancestors.
    let previous: Option<(i64, String)> = connection
        .query_row(
            "SELECT created_at, id FROM checkpoints ORDER BY created_at DESC, id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((previous_time, previous_id)) = previous {
        created_at = created_at.max(previous_time);
        if let Ok(previous_id) = previous_id.parse::<Ulid>() {
            if checkpoint_id <= previous_id {
                checkpoint_id = Ulid::from(
                    u128::from(previous_id)
                        .checked_add(1)
                        .ok_or(rusqlite::Error::InvalidQuery)?,
                );
            }
        }
    }
    Ok((created_at, checkpoint_id.to_string()))
}

/// Intervals are independent of checkpoint headers, so dropping an old header
/// cannot invalidate a retained snapshot. Closing an interval is the only update
/// to an existing version; unchanged rows are never copied or rebased.
fn insert_scan_versions(
    transaction: &rusqlite::Transaction<'_>,
    checkpoint_id: &str,
    documents: &[DocumentState],
) -> Result<(), CheckpointError> {
    let previous: Option<(String, i64)> = transaction
        .query_row(
            "SELECT id, generation FROM checkpoints WHERE generation IS NOT NULL
         ORDER BY generation DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let generation = previous
        .as_ref()
        .map_or(1, |(_, generation)| generation + 1);
    let mut previous_documents: HashMap<String, DocumentState> = match previous {
        Some((id, _)) => load_checkpoint_documents(transaction, &id)?,
        None => Vec::new(),
    }
    .into_iter()
    .map(|state| (state.path.clone(), state))
    .collect();
    transaction.execute(
        "UPDATE checkpoints SET generation = ?2 WHERE id = ?1",
        params![checkpoint_id, generation],
    )?;
    let mut close = transaction.prepare(
        "UPDATE checkpoint_document_versions SET valid_to = ?2
         WHERE path = ?1 AND valid_to IS NULL",
    )?;
    let mut insert = transaction.prepare(
        "INSERT INTO checkpoint_document_versions
         (path, valid_from, document_kind, content_hash, link_hash,
          property_hash, embedding_hash, orphan, stale)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for state in documents {
        if let Some(old) = previous_documents.remove(&state.path) {
            if old == *state {
                continue;
            }
            close.execute(params![state.path, generation])?;
        }
        insert.execute(params![
            state.path,
            generation,
            state.document_kind,
            state.content_hash,
            state.link_hash,
            state.property_hash,
            state.embedding_hash,
            i64::from(state.orphan),
            i64::from(state.stale),
        ])?;
    }
    for path in previous_documents.keys() {
        close.execute(params![path, generation])?;
    }
    Ok(())
}

fn prune_automatic_scan_checkpoints(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<(), CheckpointError> {
    let mut statement = transaction.prepare(
        "
        SELECT id
        FROM checkpoints
        WHERE source = 'scan'
        ORDER BY created_at DESC, id DESC
        ",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let ids = rows.collect::<Result<Vec<_>, _>>()?;
    for id in ids.into_iter().skip(MAX_AUTOMATIC_SCAN_CHECKPOINTS) {
        transaction.execute("DELETE FROM checkpoints WHERE id = ?1", [id])?;
    }
    // Keep versions covering even the oldest retained generation. Open versions
    // may start before it and remain necessary indefinitely.
    transaction.execute(
        "DELETE FROM checkpoint_document_versions
         WHERE valid_to <= (SELECT MIN(generation) FROM checkpoints)",
        [],
    )?;
    Ok(())
}

fn build_snapshot_state(connection: &Connection) -> Result<SnapshotState, CheckpointError> {
    build_snapshot_state_at(connection, current_unix_timestamp()?)
}

fn build_snapshot_state_at(
    connection: &Connection,
    now: i64,
) -> Result<SnapshotState, CheckpointError> {
    let documents = load_document_states(connection, now)?;
    snapshot_from_documents(connection, documents)
}

// Only metadata that participates in the existing embedding hash is compared.
// No embedding payload is read. This also detects vector-only changes on an
// unedited document without depending on triggers on a virtual table.
fn vector_input_query(connection: &Connection) -> Result<&'static str, CheckpointError> {
    let has_model: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM vector_index_state WHERE id = 1)",
        [],
        |row| row.get(0),
    )?;
    Ok(if has_model {
        "SELECT chunks.id AS chunk_id, chunks.document_id, chunks.content_hash,
                chunks.sequence_index,
                (SELECT provider_name || ':' || model_name || ':' || dimensions
                 FROM vector_index_state WHERE id = 1) AS model
         FROM chunks JOIN vectors ON vectors.chunk_id = chunks.id"
    } else {
        "SELECT chunk_id, document_id, content_hash, sequence_index, model
         FROM checkpoint_vector_inputs WHERE 0"
    })
}

fn changed_vector_documents(connection: &Connection) -> Result<Vec<String>, CheckpointError> {
    let query = vector_input_query(connection)?;
    let mut statement = connection.prepare(&format!(
        "WITH current AS ({query}),
         removed AS (SELECT * FROM checkpoint_vector_inputs EXCEPT SELECT * FROM current),
         added AS (SELECT * FROM current EXCEPT SELECT * FROM checkpoint_vector_inputs)
         SELECT document_id FROM removed UNION SELECT document_id FROM added"
    ))?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn reconcile_vector_inputs(connection: &Connection) -> Result<(), CheckpointError> {
    let query = vector_input_query(connection)?;
    connection.execute(
        &format!(
            "DELETE FROM checkpoint_vector_inputs WHERE chunk_id IN (
            SELECT chunk_id FROM (SELECT * FROM checkpoint_vector_inputs EXCEPT {query}))"
        ),
        [],
    )?;
    connection.execute(
        &format!(
        "INSERT INTO checkpoint_vector_inputs {query} EXCEPT SELECT * FROM checkpoint_vector_inputs"
    ),
        [],
    )?;
    Ok(())
}

/// A scan checkpoint written as versions for candidate documents only; see
/// [`record_scan_checkpoint_incremental`]. Each candidate's state is
/// recomputed exactly as a full snapshot would, reusing its previous link,
/// property, and embedding hashes when their inputs are unchanged; every
/// other document's open version is already current. Header counts are the
/// previous ones adjusted by the candidates' changes.
#[allow(clippy::too_many_lines)]
fn insert_incremental_scan(
    transaction: &rusqlite::Transaction<'_>,
    previous: &PreviousScan,
    changed_ids: &[String],
) -> Result<(), CheckpointError> {
    use std::collections::HashSet;
    let now = current_unix_timestamp()?;
    let ids = |sql: &str| -> Result<HashSet<String>, CheckpointError> {
        let mut statement = transaction.prepare(sql)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let mut hash_dirty: HashSet<String> = changed_ids.iter().cloned().collect();
    hash_dirty.extend(ids("SELECT document_id FROM checkpoint_dirty_documents")?);
    hash_dirty.extend(changed_vector_documents(transaction)?);
    let link_dirty = link_dirty_documents(transaction)?;
    let orphan_dirty = ids("SELECT document_id FROM checkpoint_orphan_dirty_documents")?;

    let mut candidates = ids("SELECT path FROM checkpoint_path_dirty")?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let by_id = hash_dirty
        .iter()
        .chain(&link_dirty)
        .chain(&orphan_dirty)
        .cloned()
        .collect::<Vec<_>>();
    for chunk in by_id.chunks(256) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let mut statement = transaction.prepare(&format!(
            "SELECT path FROM note_query WHERE document_id IN ({placeholders})"
        ))?;
        let rows = statement.query_map(rusqlite::params_from_iter(chunk), |row| row.get(0))?;
        for path in rows {
            candidates.insert(path?);
        }
    }
    // Staleness flips exactly for mtimes between the two thresholds.
    let low = previous.clock.min(now) - STALE_AGE_SECS;
    let high = previous.clock.max(now) - STALE_AGE_SECS;
    {
        let mut statement = transaction.prepare(
            "SELECT path FROM note_query WHERE file_mtime > 0 AND file_mtime BETWEEN ?1 AND ?2",
        )?;
        let rows = statement.query_map([low, high], |row| row.get(0))?;
        for path in rows {
            candidates.insert(path?);
        }
    }

    let mut current_row = transaction.prepare_cached(
        "SELECT document_id, extension, lower(hex(revision)), file_mtime
         FROM note_query WHERE path = ?1",
    )?;
    let mut previous_row = transaction.prepare_cached(
        "SELECT document_kind, content_hash, link_hash, property_hash, embedding_hash,
                orphan, stale
         FROM checkpoint_document_versions WHERE path = ?1 AND valid_to IS NULL",
    )?;
    let mut unlinked = transaction.prepare_cached(
        "SELECT NOT EXISTS (
             SELECT 1 FROM links WHERE source_document_id = ?1
               AND resolved_target_id IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM links WHERE resolved_target_id = ?1)",
    )?;
    // (path, previous open version, new state and document ID if present)
    let mut changes = Vec::new();
    let mut link_ids = Vec::new();
    let mut hash_ids = Vec::new();
    for path in candidates {
        let old = previous_row
            .query_row([&path], |row| {
                Ok(DocumentState {
                    path: path.clone(),
                    document_kind: row.get(0)?,
                    content_hash: row.get(1)?,
                    link_hash: row.get(2)?,
                    property_hash: row.get(3)?,
                    embedding_hash: row.get(4)?,
                    orphan: row.get::<_, i64>(5)? != 0,
                    stale: row.get::<_, i64>(6)? != 0,
                })
            })
            .optional()?;
        let current = current_row
            .query_row([&path], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .optional()?;
        let state = match current {
            None => None,
            Some((id, extension, content_hash, mtime)) => {
                let kind = match extension.as_str() {
                    "md" => "note",
                    "base" => "base",
                    _ => "attachment",
                };
                let slot = changes.len();
                if old.is_none() || link_dirty.contains(&id) {
                    link_ids.push((id.clone(), slot));
                }
                if old.is_none()
                    || hash_dirty.contains(&id)
                    || old
                        .as_ref()
                        .is_some_and(|state| state.content_hash != content_hash)
                {
                    hash_ids.push((id.clone(), slot));
                }
                let orphan =
                    kind == "note" && unlinked.query_row([&id], |row| row.get::<_, bool>(0))?;
                Some(DocumentState {
                    path: path.clone(),
                    document_kind: kind.into(),
                    content_hash,
                    link_hash: old
                        .as_ref()
                        .map(|state| state.link_hash.clone())
                        .unwrap_or_default(),
                    property_hash: old
                        .as_ref()
                        .map(|state| state.property_hash.clone())
                        .unwrap_or_default(),
                    embedding_hash: old
                        .as_ref()
                        .map(|state| state.embedding_hash.clone())
                        .unwrap_or_default(),
                    orphan,
                    stale: kind == "note"
                        && mtime > 0
                        && now.saturating_sub(mtime) >= STALE_AGE_SECS,
                })
            }
        };
        changes.push((path, old, state));
    }
    // Bound bind parameters even for a large update or model switch.
    for chunk in link_ids.chunks(256) {
        let ids = chunk.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        let placeholders = vec!["?"; ids.len()].join(",");
        let links = document_link_hashes_for_ids(transaction, &placeholders, &ids)?;
        for (id, slot) in chunk {
            if let Some(state) = changes[*slot].2.as_mut() {
                state.link_hash = links.get(id).cloned().unwrap_or_default();
            }
        }
    }
    for chunk in hash_ids.chunks(256) {
        let ids = chunk.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        let placeholders = vec!["?"; ids.len()].join(",");
        let properties = document_property_hashes_for_ids(transaction, &placeholders, &ids)?;
        let embeddings = document_embedding_hashes_for_ids(transaction, &placeholders, &ids)?;
        for (id, slot) in chunk {
            if let Some(state) = changes[*slot].2.as_mut() {
                state.property_hash = properties.get(id).cloned().unwrap_or_default();
                state.embedding_hash = embeddings.get(id).cloned().unwrap_or_default();
            }
        }
    }

    let (mut notes, mut orphans, mut stale) = (
        previous.note_count,
        previous.orphan_notes,
        previous.stale_notes,
    );
    let mut count = |state: &DocumentState, sign: i64| {
        if state.document_kind == "note" {
            notes += sign;
            orphans += sign * i64::from(state.orphan);
            stale += sign * i64::from(state.stale);
        }
    };
    for (_, old, state) in &changes {
        if let Some(old) = old {
            count(old, -1);
        }
        if let Some(state) = state {
            count(state, 1);
        }
    }
    let size = |value: i64| usize::try_from(value).unwrap_or(0);
    let resolved_links = usize::try_from(transaction.query_row(
        "SELECT COUNT(*) FROM links WHERE resolved_target_id IS NOT NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .unwrap_or(usize::MAX);
    let record = insert_checkpoint_header(
        transaction,
        None,
        "scan",
        &CheckpointRecord {
            id: String::new(),
            name: None,
            source: String::new(),
            created_at: 0,
            note_count: size(notes),
            orphan_notes: size(orphans),
            stale_notes: size(stale),
            resolved_links,
        },
    )?;
    let generation = previous.generation + 1;
    transaction.execute(
        "UPDATE checkpoints SET generation = ?2 WHERE id = ?1",
        params![record.id, generation],
    )?;
    let mut close = transaction.prepare_cached(
        "UPDATE checkpoint_document_versions SET valid_to = ?2
         WHERE path = ?1 AND valid_to IS NULL",
    )?;
    let mut insert = transaction.prepare_cached(
        "INSERT INTO checkpoint_document_versions
         (path, valid_from, document_kind, content_hash, link_hash,
          property_hash, embedding_hash, orphan, stale)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for (path, old, state) in changes {
        if old == state {
            continue;
        }
        if old.is_some() {
            close.execute(params![path, generation])?;
        }
        if let Some(state) = state {
            insert.execute(params![
                state.path,
                generation,
                state.document_kind,
                state.content_hash,
                state.link_hash,
                state.property_hash,
                state.embedding_hash,
                i64::from(state.orphan),
                i64::from(state.stale),
            ])?;
        }
    }
    finish_scan_checkpoint(transaction, now)
}

fn snapshot_from_documents(
    connection: &Connection,
    documents: Vec<DocumentState>,
) -> Result<SnapshotState, CheckpointError> {
    let note_count = documents
        .iter()
        .filter(|state| state.document_kind == "note")
        .count();
    let orphan_notes = documents
        .iter()
        .filter(|state| state.document_kind == "note" && state.orphan)
        .count();
    let stale_notes = documents
        .iter()
        .filter(|state| state.document_kind == "note" && state.stale)
        .count();
    let resolved_links = usize::try_from(connection.query_row(
        "SELECT COUNT(*) FROM links WHERE resolved_target_id IS NOT NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .unwrap_or(usize::MAX);

    Ok(SnapshotState {
        records: vec![CheckpointRecord {
            id: String::new(),
            name: None,
            source: String::new(),
            created_at: 0,
            note_count,
            orphan_notes,
            stale_notes,
            resolved_links,
        }],
        documents,
    })
}

fn load_document_states(
    connection: &Connection,
    now: i64,
) -> Result<Vec<DocumentState>, CheckpointError> {
    // The narrow table mirrors these columns (`revision` is the content hash).
    let mut statement = connection.prepare_cached(
        "
        SELECT document_id, path, extension, lower(hex(revision)), file_mtime
        FROM note_query
        ORDER BY path
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let documents = rows.collect::<Result<Vec<_>, _>>()?;
    let unlinked = unlinked_documents(connection)?;
    let link_hashes = document_link_hashes(connection)?;
    let property_hashes = document_property_hashes(connection)?;
    let embedding_hashes = document_embedding_hashes(connection)?;

    Ok(documents
        .into_iter()
        .map(|(id, path, extension, content_hash, file_mtime)| {
            let document_kind = match extension.as_str() {
                "md" => "note",
                "base" => "base",
                _ => "attachment",
            }
            .to_string();
            let orphan = document_kind == "note" && unlinked.contains(&id);
            let stale = document_kind == "note"
                && file_mtime > 0
                && now.saturating_sub(file_mtime) >= STALE_AGE_SECS;

            DocumentState {
                path,
                document_kind,
                content_hash,
                link_hash: link_hashes.get(&id).cloned().unwrap_or_default(),
                property_hash: property_hashes.get(&id).cloned().unwrap_or_default(),
                embedding_hash: embedding_hashes.get(&id).cloned().unwrap_or_default(),
                orphan,
                stale,
            }
        })
        .collect())
}

/// Documents with no resolved outbound or inbound link: two index probes
/// per document rather than grouping every link.
fn unlinked_documents(
    connection: &Connection,
) -> Result<std::collections::HashSet<String>, CheckpointError> {
    let mut statement = connection.prepare_cached(
        "SELECT document_id FROM note_query
         WHERE NOT EXISTS (
             SELECT 1 FROM links
             WHERE links.source_document_id = note_query.document_id
               AND links.resolved_target_id IS NOT NULL)
           AND NOT EXISTS (
             SELECT 1 FROM links WHERE links.resolved_target_id = note_query.document_id)",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Documents whose link rows or resolved target paths changed since the last
/// scan checkpoint.
fn link_dirty_documents(
    connection: &Connection,
) -> Result<std::collections::HashSet<String>, CheckpointError> {
    let mut statement =
        connection.prepare("SELECT document_id FROM checkpoint_link_dirty_documents")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(ids)
}

fn document_link_hashes(
    connection: &Connection,
) -> Result<HashMap<String, String>, CheckpointError> {
    link_hashes(connection, "", &[])
}

/// [`document_link_hashes`] for the given source documents only.
fn document_link_hashes_for_ids(
    connection: &Connection,
    placeholders: &str,
    ids: &[String],
) -> Result<HashMap<String, String>, CheckpointError> {
    link_hashes(
        connection,
        &format!("WHERE links.source_document_id IN ({placeholders})"),
        ids,
    )
}

fn link_hashes(
    connection: &Connection,
    filter: &str,
    ids: &[String],
) -> Result<HashMap<String, String>, CheckpointError> {
    let mut statement = connection.prepare(&format!(
        "
        SELECT
            source_document_id,
            raw_text,
            link_kind,
            COALESCE(display_text, ''),
            COALESCE(target_path_candidate, ''),
            COALESCE(target_heading, ''),
            COALESCE(target_block, ''),
            COALESCE(target.path, '')
        FROM links
        LEFT JOIN documents AS target ON target.id = links.resolved_target_id
        {filter}
        ORDER BY source_document_id, byte_offset
        "
    ))?;
    let rows = statement.query_map(rusqlite::params_from_iter(ids), |row| {
        Ok((
            row.get::<_, String>(0)?,
            [
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ]
            .join("|"),
        ))
    })?;

    let mut parts = HashMap::<String, Vec<String>>::new();
    for row in rows {
        let (document_id, value) = row?;
        parts.entry(document_id).or_default().push(value);
    }
    Ok(parts
        .into_iter()
        .map(|(document_id, values)| (document_id, hash_joined(&values)))
        .collect())
}

fn document_property_hashes(
    connection: &Connection,
) -> Result<HashMap<String, String>, CheckpointError> {
    let mut statement = connection.prepare(
        "
        SELECT document_id, canonical_json
        FROM properties
        ORDER BY document_id
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(document_id, canonical_json)| (document_id, hash_value(&canonical_json)))
        .collect())
}

fn document_embedding_hashes(
    connection: &Connection,
) -> Result<HashMap<String, String>, CheckpointError> {
    let model = connection
        .query_row(
            "
            SELECT provider_name, model_name, dimensions
            FROM vector_index_state
            WHERE id = 1
            ",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((provider_name, model_name, dimensions)) = model else {
        return Ok(HashMap::new());
    };

    let mut statement = connection.prepare(
        "
        SELECT chunks.document_id, chunks.id, lower(hex(chunks.content_hash))
        FROM chunks
        JOIN vectors ON vectors.chunk_id = chunks.id
        ORDER BY chunks.document_id, chunks.sequence_index
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            format!("{}:{}", row.get::<_, String>(1)?, row.get::<_, String>(2)?),
        ))
    })?;
    let mut parts = HashMap::<String, Vec<String>>::new();
    for row in rows {
        let (document_id, value) = row?;
        parts.entry(document_id).or_default().push(value);
    }
    let prefix = format!("{provider_name}:{model_name}:{dimensions}");
    Ok(parts
        .into_iter()
        .map(|(document_id, values)| {
            let mut all = Vec::with_capacity(values.len() + 1);
            all.push(prefix.clone());
            all.extend(values);
            (document_id, hash_joined(&all))
        })
        .collect())
}

fn document_property_hashes_for_ids(
    connection: &Connection,
    placeholders: &str,
    document_ids: &[String],
) -> Result<HashMap<String, String>, CheckpointError> {
    let sql = format!(
        "SELECT document_id, canonical_json
         FROM properties
         WHERE document_id IN ({placeholders})
         ORDER BY document_id"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(document_ids.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(document_id, canonical_json)| (document_id, hash_value(&canonical_json)))
        .collect())
}

fn document_embedding_hashes_for_ids(
    connection: &Connection,
    placeholders: &str,
    document_ids: &[String],
) -> Result<HashMap<String, String>, CheckpointError> {
    let model = connection
        .query_row(
            "SELECT provider_name, model_name, dimensions
             FROM vector_index_state WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((provider_name, model_name, dimensions)) = model else {
        return Ok(HashMap::new());
    };

    let sql = format!(
        "SELECT chunks.document_id, chunks.id, lower(hex(chunks.content_hash))
         FROM chunks
         JOIN vectors ON vectors.chunk_id = chunks.id
         WHERE chunks.document_id IN ({placeholders})
         ORDER BY chunks.document_id, chunks.sequence_index"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(document_ids.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            format!("{}:{}", row.get::<_, String>(1)?, row.get::<_, String>(2)?),
        ))
    })?;
    let mut parts = HashMap::<String, Vec<String>>::new();
    for row in rows {
        let (document_id, value) = row?;
        parts.entry(document_id).or_default().push(value);
    }
    let prefix = format!("{provider_name}:{model_name}:{dimensions}");
    Ok(parts
        .into_iter()
        .map(|(document_id, values)| {
            let mut all = Vec::with_capacity(values.len() + 1);
            all.push(prefix.clone());
            all.extend(values);
            (document_id, hash_joined(&all))
        })
        .collect())
}

fn load_anchor_snapshot(
    connection: &Connection,
    anchor: &ChangeAnchor,
) -> Result<SnapshotState, CheckpointError> {
    let record = match anchor {
        ChangeAnchor::LastScan => {
            let mut statement = connection.prepare(
                "
                SELECT id, name, source, created_at, note_count, orphan_notes, stale_notes, resolved_links
                FROM checkpoints
                WHERE source = 'scan'
                ORDER BY created_at DESC, id DESC
                LIMIT 1 OFFSET 1
                ",
            )?;
            statement
                .query_row([], checkpoint_record_row)
                .optional()?
                .ok_or_else(|| CheckpointError::NotFound {
                    name: "last_scan".to_string(),
                })?
        }
        ChangeAnchor::Checkpoint(name) => {
            let mut statement = connection.prepare(
                "
                SELECT id, name, source, created_at, note_count, orphan_notes, stale_notes, resolved_links
                FROM checkpoints
                WHERE name = ?1
                ORDER BY created_at DESC, id DESC
                LIMIT 1
                ",
            )?;
            statement
                .query_row([name], checkpoint_record_row)
                .optional()?
                .ok_or_else(|| CheckpointError::NotFound { name: name.clone() })?
        }
    };
    let documents = load_checkpoint_documents(connection, &record.id)?;
    Ok(SnapshotState {
        records: vec![record],
        documents,
    })
}

fn load_checkpoint_records(
    connection: &Connection,
) -> Result<Vec<CheckpointRecord>, CheckpointError> {
    let mut statement = connection.prepare(
        "
        SELECT id, name, source, created_at, note_count, orphan_notes, stale_notes, resolved_links
        FROM checkpoints
        ORDER BY created_at DESC, id DESC
        ",
    )?;
    let rows = statement.query_map([], checkpoint_record_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn checkpoint_record_row(row: &rusqlite::Row<'_>) -> Result<CheckpointRecord, rusqlite::Error> {
    Ok(CheckpointRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        source: row.get(2)?,
        created_at: row.get(3)?,
        note_count: usize::try_from(row.get::<_, i64>(4)?).unwrap_or(usize::MAX),
        orphan_notes: usize::try_from(row.get::<_, i64>(5)?).unwrap_or(usize::MAX),
        stale_notes: usize::try_from(row.get::<_, i64>(6)?).unwrap_or(usize::MAX),
        resolved_links: usize::try_from(row.get::<_, i64>(7)?).unwrap_or(usize::MAX),
    })
}

fn load_checkpoint_documents(
    connection: &Connection,
    checkpoint_id: &str,
) -> Result<Vec<DocumentState>, CheckpointError> {
    let mut statement = connection.prepare(
        "
        SELECT path, document_kind, content_hash, link_hash, property_hash, embedding_hash, orphan, stale
        FROM checkpoint_documents
        WHERE checkpoint_id = ?1
        UNION ALL
        SELECT v.path, v.document_kind, v.content_hash, v.link_hash,
               v.property_hash, v.embedding_hash, v.orphan, v.stale
        FROM checkpoint_document_versions v
        JOIN checkpoints c ON c.id = ?1
        WHERE v.valid_from <= c.generation
          AND (v.valid_to IS NULL OR v.valid_to > c.generation)
        ORDER BY path
        ",
    )?;
    let rows = statement.query_map([checkpoint_id], |row| {
        Ok(DocumentState {
            path: row.get(0)?,
            document_kind: row.get(1)?,
            content_hash: row.get(2)?,
            link_hash: row.get(3)?,
            property_hash: row.get(4)?,
            embedding_hash: row.get(5)?,
            orphan: row.get::<_, i64>(6)? != 0,
            stale: row.get::<_, i64>(7)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn hash_joined(values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        hash_value(&values.join("\n"))
    }
}

fn hash_value(value: &str) -> String {
    #[cfg(test)]
    CHECKPOINT_HASH_CALLS.with(|calls| calls.set(calls.get() + 1));
    let mut hasher = Hasher::new();
    hasher.update(value.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
thread_local! {
    static CHECKPOINT_HASH_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn diff_category(
    old: Option<&str>,
    new: Option<&str>,
    always_track_presence: bool,
) -> Option<ChangeStatus> {
    match (old, new) {
        (None, Some(value)) if always_track_presence || !value.is_empty() => {
            Some(ChangeStatus::Added)
        }
        (Some(value), None) if always_track_presence || !value.is_empty() => {
            Some(ChangeStatus::Deleted)
        }
        (Some(left), Some(right)) if left != right => Some(ChangeStatus::Updated),
        _ => None,
    }
}

fn validate_checkpoint_name(name: &str) -> Result<(), CheckpointError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(CheckpointError::InvalidName(name.to_string()));
    }
    Ok(())
}

fn current_unix_timestamp() -> Result<i64, CheckpointError> {
    Ok(i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()).unwrap_or(i64::MAX))
}

fn open_existing_cache(paths: &VaultPaths) -> Result<CacheDatabase, CheckpointError> {
    if !paths.cache_db().exists() {
        return Err(CheckpointError::CacheMissing);
    }
    CacheDatabase::open(paths).map_err(CheckpointError::from)
}

#[cfg(test)]
#[path = "history_checkpoint_tests.rs"]
mod checkpoint_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{initialize_vulcan_dir, scan_vault, ScanMode};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn checkpoints_and_change_reports_track_scans() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        create_checkpoint(&paths, "baseline").expect("checkpoint should create");
        fs::write(
            vault_root.join("Home.md"),
            "# Home\n\nUpdated dashboard links.\n",
        )
        .expect("updated note should write");
        scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");

        let report = query_change_report(&paths, &ChangeAnchor::Checkpoint("baseline".to_string()))
            .expect("change report should succeed");

        assert_eq!(
            report.notes,
            vec![ChangeItem {
                path: "Home.md".to_string(),
                status: ChangeStatus::Updated,
            }]
        );
    }

    #[test]
    fn graph_trends_returns_chronological_scan_points() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        fs::write(vault_root.join("Extra.md"), "# Extra\n").expect("extra note should write");
        scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");

        let report = query_graph_trends(&paths, 10).expect("trend query should succeed");

        assert!(report.points.len() >= 2);
        assert!(report.points[0].created_at <= report.points[1].created_at);
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);
        copy_dir_recursive(&source, destination);
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).expect("fixture file should copy");
            }
        }
    }
}
