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

#[derive(Debug, Clone)]
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
    let current = load_document_states(database.connection())?;
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
    insert_checkpoint_snapshot(&transaction, None, "scan")?;
    prune_automatic_scan_checkpoints(&transaction)?;
    transaction.commit()?;
    Ok(())
}

/// Record a scan checkpoint incrementally: copy unchanged document rows from the
/// most recent scan checkpoint and only recompute hashes for the changed documents.
/// This avoids the O(N) hash computation over all documents when only a few changed.
#[allow(clippy::too_many_lines)]
pub(crate) fn record_scan_checkpoint_incremental(
    connection: &Connection,
    changed_document_ids: &[String],
) -> Result<(), CheckpointError> {
    let transaction = connection.unchecked_transaction()?;

    // Find the most recent scan checkpoint to copy unchanged rows from.
    let previous_checkpoint_id: Option<String> = transaction
        .query_row(
            "SELECT id FROM checkpoints WHERE source = 'scan' ORDER BY created_at DESC, id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    // If there's no previous checkpoint, fall back to a full snapshot.
    let Some(previous_id) = previous_checkpoint_id else {
        insert_checkpoint_snapshot(&transaction, None, "scan")?;
        prune_automatic_scan_checkpoints(&transaction)?;
        transaction.commit()?;
        return Ok(());
    };

    let created_at = current_unix_timestamp()?;
    let checkpoint_id = Ulid::new().to_string();

    // Build a set of changed doc IDs for quick lookup, and also collect
    // the current document IDs (path→id) so we know which previous rows are still valid.
    let changed_set: std::collections::HashSet<&str> = changed_document_ids
        .iter()
        .map(String::as_str)
        .collect();

    // Insert the checkpoint header row first (FK parent for checkpoint_documents).
    // Counts will be updated after all document rows are inserted.
    transaction.execute(
        "INSERT INTO checkpoints (id, name, source, created_at, note_count, orphan_notes, stale_notes, resolved_links)
         VALUES (?1, NULL, 'scan', ?2, 0, 0, 0, 0)",
        params![&checkpoint_id, created_at],
    )?;

    // Load current document id→path mapping.
    let current_docs: HashMap<String, String> = {
        let mut stmt = transaction.prepare("SELECT id, path FROM documents")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<Result<HashMap<_, _>, _>>()?
    };
    let current_paths: std::collections::HashSet<&str> =
        current_docs.values().map(String::as_str).collect();

    // Copy unchanged rows from the previous checkpoint. A row is unchanged if:
    // 1. Its path still exists in the current documents table
    // 2. The document at that path was not in the changed set
    // We need to map paths back to IDs to check against changed_document_ids.
    let path_to_id: HashMap<&str, &str> = current_docs
        .iter()
        .map(|(id, path)| (path.as_str(), id.as_str()))
        .collect();

    let previous_docs = load_checkpoint_documents(&transaction, &previous_id)?;

    let mut insert_stmt = transaction.prepare(
        "INSERT INTO checkpoint_documents (
            checkpoint_id, path, document_kind, content_hash,
            link_hash, property_hash, embedding_hash, orphan, stale
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;

    // Copy unchanged document rows from previous checkpoint.
    for doc in &previous_docs {
        if !current_paths.contains(doc.path.as_str()) {
            // Document was deleted — skip it.
            continue;
        }
        if let Some(&doc_id) = path_to_id.get(doc.path.as_str()) {
            if changed_set.contains(doc_id) {
                // Document was changed — will be recomputed below.
                continue;
            }
        }
        insert_stmt.execute(params![
            &checkpoint_id,
            &doc.path,
            &doc.document_kind,
            &doc.content_hash,
            &doc.link_hash,
            &doc.property_hash,
            &doc.embedding_hash,
            i64::from(doc.orphan),
            i64::from(doc.stale),
        ])?;
    }

    // Compute fresh state only for changed documents.
    if !changed_document_ids.is_empty() {
        let now = current_unix_timestamp()?;
        let changed_states =
            load_document_states_for_ids(&transaction, changed_document_ids, now)?;
        for state in &changed_states {
            insert_stmt.execute(params![
                &checkpoint_id,
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

    // Drop the prepared statement so `transaction` is no longer borrowed.
    drop(insert_stmt);

    // Compute summary counts from the current cache state (cheap queries).
    let note_count = usize::try_from(transaction.query_row(
        "SELECT COUNT(*) FROM documents WHERE extension = 'md'",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .unwrap_or(usize::MAX);

    let resolved_links = usize::try_from(transaction.query_row(
        "SELECT COUNT(*) FROM links WHERE resolved_target_id IS NOT NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?)
    .unwrap_or(usize::MAX);

    // Count orphan/stale from the checkpoint_documents we just inserted.
    let orphan_notes = usize::try_from(transaction.query_row(
        "SELECT COUNT(*) FROM checkpoint_documents WHERE checkpoint_id = ?1 AND document_kind = 'note' AND orphan = 1",
        [&checkpoint_id],
        |row| row.get::<_, i64>(0),
    )?)
    .unwrap_or(usize::MAX);

    let stale_notes = usize::try_from(transaction.query_row(
        "SELECT COUNT(*) FROM checkpoint_documents WHERE checkpoint_id = ?1 AND document_kind = 'note' AND stale = 1",
        [&checkpoint_id],
        |row| row.get::<_, i64>(0),
    )?)
    .unwrap_or(usize::MAX);

    transaction.execute(
        "UPDATE checkpoints SET note_count = ?2, orphan_notes = ?3, stale_notes = ?4, resolved_links = ?5 WHERE id = ?1",
        params![
            &checkpoint_id,
            i64::try_from(note_count).unwrap_or(i64::MAX),
            i64::try_from(orphan_notes).unwrap_or(i64::MAX),
            i64::try_from(stale_notes).unwrap_or(i64::MAX),
            i64::try_from(resolved_links).unwrap_or(i64::MAX),
        ],
    )?;

    prune_automatic_scan_checkpoints(&transaction)?;
    transaction.commit()?;
    Ok(())
}

fn insert_checkpoint_snapshot(
    transaction: &rusqlite::Transaction<'_>,
    name: Option<&str>,
    source: &str,
) -> Result<CheckpointRecord, CheckpointError> {
    let created_at = current_unix_timestamp()?;
    let checkpoint_id = Ulid::new().to_string();
    let snapshot = build_snapshot_state(transaction)?;
    let record = snapshot
        .records
        .into_iter()
        .next()
        .expect("snapshot state should include one record");

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
            &checkpoint_id,
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

    Ok(CheckpointRecord {
        id: checkpoint_id,
        name: name.map(ToOwned::to_owned),
        source: source.to_string(),
        created_at,
        ..record
    })
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
    Ok(())
}

fn build_snapshot_state(connection: &Connection) -> Result<SnapshotState, CheckpointError> {
    let documents = load_document_states(connection)?;
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

fn load_document_states(connection: &Connection) -> Result<Vec<DocumentState>, CheckpointError> {
    let now = current_unix_timestamp()?;
    let mut statement = connection.prepare(
        "
        SELECT id, path, extension, lower(hex(content_hash)), file_mtime
        FROM documents
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
    let outbound = count_map(
        connection,
        "SELECT source_document_id, COUNT(*) FROM links WHERE resolved_target_id IS NOT NULL GROUP BY source_document_id",
    )?;
    let inbound = count_map(
        connection,
        "SELECT resolved_target_id, COUNT(*) FROM links WHERE resolved_target_id IS NOT NULL GROUP BY resolved_target_id",
    )?;
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
            let orphan = document_kind == "note"
                && outbound.get(&id).copied().unwrap_or(0) == 0
                && inbound.get(&id).copied().unwrap_or(0) == 0;
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

/// Load document states for only the specified document IDs. Used by incremental checkpoints
/// to avoid recomputing hashes for the entire vault.
fn load_document_states_for_ids(
    connection: &Connection,
    document_ids: &[String],
    now: i64,
) -> Result<Vec<DocumentState>, CheckpointError> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = document_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");

    // Load document basics.
    let sql = format!(
        "SELECT id, path, extension, lower(hex(content_hash)), file_mtime
         FROM documents WHERE id IN ({placeholders}) ORDER BY path"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(document_ids.iter()),
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    )?;
    let documents = rows.collect::<Result<Vec<_>, _>>()?;

    // Compute link/property/embedding hashes only for these documents.
    let link_hashes = document_link_hashes_for_ids(connection, &placeholders, document_ids)?;
    let property_hashes =
        document_property_hashes_for_ids(connection, &placeholders, document_ids)?;
    let embedding_hashes =
        document_embedding_hashes_for_ids(connection, &placeholders, document_ids)?;

    // Compute orphan status for these documents.
    let outbound_sql = format!(
        "SELECT source_document_id, COUNT(*) FROM links
         WHERE resolved_target_id IS NOT NULL AND source_document_id IN ({placeholders})
         GROUP BY source_document_id"
    );
    let inbound_sql = format!(
        "SELECT resolved_target_id, COUNT(*) FROM links
         WHERE resolved_target_id IS NOT NULL AND resolved_target_id IN ({placeholders})
         GROUP BY resolved_target_id"
    );
    let outbound = count_map_parameterized(connection, &outbound_sql, document_ids)?;
    let inbound = count_map_parameterized(connection, &inbound_sql, document_ids)?;

    Ok(documents
        .into_iter()
        .map(|(id, path, extension, content_hash, file_mtime)| {
            let document_kind = match extension.as_str() {
                "md" => "note",
                "base" => "base",
                _ => "attachment",
            }
            .to_string();
            let orphan = document_kind == "note"
                && outbound.get(&id).copied().unwrap_or(0) == 0
                && inbound.get(&id).copied().unwrap_or(0) == 0;
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

fn count_map(
    connection: &Connection,
    sql: &str,
) -> Result<HashMap<String, usize>, CheckpointError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            usize::try_from(row.get::<_, i64>(1)?).unwrap_or(usize::MAX),
        ))
    })?;
    Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
}

fn document_link_hashes(
    connection: &Connection,
) -> Result<HashMap<String, String>, CheckpointError> {
    let mut statement = connection.prepare(
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
        ORDER BY source_document_id, byte_offset
        ",
    )?;
    let rows = statement.query_map([], |row| {
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

fn count_map_parameterized(
    connection: &Connection,
    sql: &str,
    params: &[String],
) -> Result<HashMap<String, usize>, CheckpointError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            usize::try_from(row.get::<_, i64>(1)?).unwrap_or(usize::MAX),
        ))
    })?;
    Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
}

fn document_link_hashes_for_ids(
    connection: &Connection,
    placeholders: &str,
    document_ids: &[String],
) -> Result<HashMap<String, String>, CheckpointError> {
    let sql = format!(
        "SELECT
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
        WHERE source_document_id IN ({placeholders})
        ORDER BY source_document_id, byte_offset"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(document_ids.iter()), |row| {
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
    let mut hasher = Hasher::new();
    hasher.update(value.as_bytes());
    hasher.finalize().to_hex().to_string()
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
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn checkpoints_and_change_reports_track_scans() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

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
