mod error;
mod migrations;
mod schema;

pub use error::CacheError;
pub use migrations::{Migration, MigrationRegistry};
pub(crate) use schema::{
    drop_fts_triggers, rebuild_fts_index, rebuild_search_index, restore_fts_triggers,
};

use crate::paths::{ensure_vulcan_dir, VaultPaths};
use crate::{EXTRACTION_VERSION, PARSER_VERSION};
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::time::Duration;
#[cfg(feature = "vectors")]
use vulcan_embed::register_sqlite_vec_extension;

pub const BUSY_TIMEOUT_MS: u64 = 5_000;
pub const META_EXTRACTION_VERSION: &str = "extraction_version";
pub const META_PARSER_VERSION: &str = "parser_version";
pub const META_SCHEMA_VERSION: &str = "schema_version";

#[derive(Debug)]
pub struct CacheDatabase {
    connection: Connection,
}

impl CacheDatabase {
    pub fn open(paths: &VaultPaths) -> Result<Self, CacheError> {
        ensure_vulcan_dir(paths)?;
        #[cfg(target_os = "android")]
        {
            let state_root = crate::vulcan_user_state_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "cannot determine the private Vulcan state directory for the Android cache",
                )
            })?;
            let expected = crate::paths::private_cache_db_path(paths.vault_root(), &state_root)?;
            if paths.cache_db() != expected {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Android cache path is stale; reopen the vault from its existing directory",
                )
                .into());
            }
            std::fs::create_dir_all(expected.parent().expect("cache path has a parent"))?;
        }
        #[cfg(feature = "vectors")]
        register_sqlite_vec_extension();

        let mut connection = Connection::open(paths.cache_db())?;
        configure_connection(&connection)?;
        MigrationRegistry::schema_v1().migrate(&mut connection)?;
        sync_runtime_metadata(&connection)?;

        Ok(Self { connection })
    }

    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn user_version(&self) -> Result<u32, CacheError> {
        Ok(self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn meta_value(&self, key: &str) -> Result<Option<String>, CacheError> {
        Ok(self
            .connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn clear_all(&mut self) -> Result<(), CacheError> {
        self.rebuild_with(|_| Ok(()))
    }

    pub fn with_transaction<F, T, E>(&mut self, work: F) -> Result<T, E>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, E>,
        E: From<CacheError>,
    {
        let transaction = self
            .connection
            .transaction()
            .map_err(CacheError::from)
            .map_err(E::from)?;
        let result = work(&transaction)?;
        transaction
            .commit()
            .map_err(CacheError::from)
            .map_err(E::from)?;
        Ok(result)
    }

    /// Clear the cache and run `rebuild` in the same transaction.
    ///
    /// The triggers that maintain derived state from the core tables (see
    /// [`REBUILT_TRIGGER_TABLES`]) are suspended for the whole rebuild and
    /// recreated before it commits: firing them for every row of a full
    /// rebuild tripled its cost. The callback starts from an empty,
    /// consistent cache. One that writes documents must call
    /// [`rebuild_note_store`] once they and their properties, tags, and
    /// aliases are written, and recompute graph confidence once links are
    /// resolved. Checkpoint change tracking needs nothing: clearing the
    /// cache already makes the next checkpoint a full one.
    pub fn rebuild_with<F, T, E>(&mut self, rebuild: F) -> Result<T, E>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, E>,
        E: From<CacheError>,
    {
        self.with_transaction(|transaction| {
            let triggers = suspend_rebuilt_triggers(transaction)
                .map_err(CacheError::from)
                .map_err(E::from)?;
            schema::clear_cache_tables(transaction)
                .map_err(CacheError::from)
                .map_err(E::from)?;
            rebuild_note_store(transaction)
                .and_then(|()| crate::graph_confidence::recompute_all(transaction))
                .map_err(CacheError::from)
                .map_err(E::from)?;
            sync_runtime_metadata_tx(transaction).map_err(E::from)?;
            let result = rebuild(transaction)?;
            triggers
                .restore(transaction)
                .map_err(CacheError::from)
                .map_err(E::from)?;
            Ok(result)
        })
    }

    /// Enable or disable foreign key checks on this connection.
    ///
    /// Foreign key checks must be changed outside of a transaction.  It is the caller's
    /// responsibility to ensure this is called at the right point and to restore the
    /// setting to `true` after bulk writes are complete.
    pub fn set_foreign_keys(&self, enabled: bool) -> Result<(), CacheError> {
        self.connection
            .pragma_update(None, "foreign_keys", if enabled { "ON" } else { "OFF" })
            .map_err(CacheError::from)
    }

    /// Set the `SQLite` locking mode for this connection.
    ///
    /// Use `"EXCLUSIVE"` during single-writer bulk operations to eliminate shared-lock
    /// overhead.  Restore to `"NORMAL"` when done.  Must be called outside a transaction.
    pub fn set_locking_mode(&self, mode: &str) -> Result<(), CacheError> {
        self.connection
            .pragma_update(None, "locking_mode", mode)
            .map_err(CacheError::from)
    }
}

fn configure_connection(connection: &Connection) -> Result<(), CacheError> {
    // page_size must be set before any writes on a new database; silently ignored on existing ones.
    // Benchmarked on 10K-note vault: 4096→6.83s, 8192→6.53s (+26% peak throughput), 16384→6.56s.
    connection.pragma_update(None, "page_size", 8192)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "cache_size", -16_000)?; // 16 MB
    connection.pragma_update(None, "mmap_size", 67_108_864)?; // 64 MB
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    Ok(())
}

fn sync_runtime_metadata(connection: &Connection) -> Result<(), CacheError> {
    // Skip the write transaction if all metadata values are already current.
    let pairs = [
        (META_SCHEMA_VERSION, crate::SCHEMA_VERSION.to_string()),
        (META_PARSER_VERSION, PARSER_VERSION.to_string()),
        (META_EXTRACTION_VERSION, EXTRACTION_VERSION.to_string()),
    ];
    let all_current = pairs.iter().all(|(key, expected)| {
        connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .ok()
            .as_ref()
            == Some(expected)
    });
    if all_current {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    sync_runtime_metadata_tx(&transaction)?;
    transaction.commit()?;
    Ok(())
}

fn sync_runtime_metadata_tx(transaction: &Transaction<'_>) -> Result<(), CacheError> {
    let pairs = [
        (META_SCHEMA_VERSION, crate::SCHEMA_VERSION.to_string()),
        (META_PARSER_VERSION, PARSER_VERSION.to_string()),
        (META_EXTRACTION_VERSION, EXTRACTION_VERSION.to_string()),
    ];

    for (key, value) in pairs {
        transaction.execute(
            "
            INSERT INTO meta (key, value)
            VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            ",
            (&key, &value),
        )?;
    }

    Ok(())
}

/// Give the `note_query` rows of `document_ids` new row versions (and
/// advance the clock) in the caller's transaction, for a change that alters
/// what those notes show without rewriting their rows, such as an inferred
/// link between them. Retained readers then treat both as changed.
pub(crate) fn touch_note_rows(
    connection: &Connection,
    document_ids: &[&str],
) -> rusqlite::Result<()> {
    let mut statement =
        connection.prepare_cached("UPDATE note_query SET path = path WHERE document_id = ?1")?;
    for id in document_ids {
        statement.execute([id])?;
    }
    Ok(())
}

/// Tables whose triggers only maintain state that a full rebuild recomputes:
/// `note_query` and its row versions, checkpoint change tracking, and graph
/// confidence dirty marking.
pub(crate) const REBUILT_TRIGGER_TABLES: [&str; 6] = [
    "documents",
    "properties",
    "tags",
    "aliases",
    "links",
    "note_query",
];

/// Triggers dropped for a rebuild, kept as their `CREATE` statements.
#[must_use = "suspended triggers must be restored before the rebuild commits"]
pub(crate) struct SuspendedTriggers(Vec<(String, String)>);

impl SuspendedTriggers {
    #[cfg(test)]
    pub(crate) fn names(&self) -> Vec<&str> {
        self.0.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// Recreate the dropped triggers in the caller's transaction.
    pub(crate) fn restore(self, transaction: &Transaction<'_>) -> rusqlite::Result<()> {
        for (_, sql) in self.0 {
            transaction.execute_batch(&sql)?;
        }
        Ok(())
    }
}

/// Drop every trigger on [`REBUILT_TRIGGER_TABLES`] in the caller's
/// transaction and return what recreates them.
pub(crate) fn suspend_rebuilt_triggers(
    transaction: &Transaction<'_>,
) -> rusqlite::Result<SuspendedTriggers> {
    let triggers = transaction
        .prepare(
            "SELECT name, sql FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name IN (SELECT value FROM json_each(?1))
             ORDER BY name",
        )?
        .query_map(
            [serde_json::to_string(&REBUILT_TRIGGER_TABLES).expect("table names serialize")],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, _) in &triggers {
        transaction.execute_batch(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")))?;
    }
    Ok(SuspendedTriggers(triggers))
}

/// Rebuild `note_query` from the normalized tables in one pass, as its
/// triggers would have maintained it row by row. Every row gets a version
/// above the current clock and the clock moves past all of them, so retained
/// readers see each row, and each removed row, as changed.
pub(crate) fn rebuild_note_store(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    transaction.execute_batch(
        "DELETE FROM note_query;
         INSERT INTO note_query (
             document_id, path, filename, extension, stat_fingerprint, revision,
             parser_version, file_size, file_mtime, file_ctime, properties, tags, aliases,
             row_version
         )
         SELECT documents.id, documents.path, documents.filename, documents.extension,
                documents.stat_fingerprint, documents.content_hash, documents.parser_version,
                documents.file_size, documents.file_mtime, documents.file_ctime,
                (SELECT jsonb(canonical_json) FROM properties
                 WHERE properties.document_id = documents.id),
                (SELECT json_group_array(tag_text) FROM
                    (SELECT tag_text FROM tags WHERE tags.document_id = documents.id
                     ORDER BY tags.rowid)),
                (SELECT json_group_array(alias_text) FROM
                    (SELECT alias_text FROM aliases WHERE aliases.document_id = documents.id
                     ORDER BY aliases.rowid)),
                (SELECT version FROM note_store_clock)
                    + row_number() OVER (ORDER BY documents.rowid)
         FROM documents;
         UPDATE note_store_clock
         SET version = version + (SELECT count(*) FROM note_query) + 1;",
    )
}

/// Advance the note store clock (schema v28) in the caller's transaction.
/// `note_query` triggers advance it for row changes; writers that change
/// what a hydrated note shows without touching its row (link resolution,
/// inferred links) call this in the same transaction, so a reader's clock
/// identifies every note-visible state.
pub(crate) fn advance_note_store_clock(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute("UPDATE note_store_clock SET version = version + 1", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::TempDir;
    use ulid::Ulid;

    #[test]
    fn open_requires_initialized_vulcan_dir() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        let error = CacheDatabase::open(&paths).expect_err("missing .vulcan should fail");
        assert!(
            error.to_string().contains("Run `vulcan init`"),
            "expected actionable init guidance: {error}"
        );
        assert!(!paths.cache_db().exists());
    }

    #[test]
    fn open_initializes_cache_schema_and_runtime_metadata() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        crate::initialize_vulcan_dir(&paths).expect(".vulcan dir should be created");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert!(paths.cache_db().exists());
        assert!(paths.gitignore_file().exists());
        assert_eq!(
            database
                .user_version()
                .expect("user_version should be readable"),
            crate::SCHEMA_VERSION
        );
        assert_eq!(query_journal_mode(database.connection()), "wal");
        assert_eq!(query_foreign_keys(database.connection()), 1);
        assert_eq!(
            query_busy_timeout(database.connection()),
            i64::try_from(BUSY_TIMEOUT_MS).expect("busy timeout should fit into i64")
        );

        for table_name in [
            "documents",
            "headings",
            "block_refs",
            "links",
            "aliases",
            "tags",
            "properties",
            "property_values",
            "property_list_items",
            "property_catalog",
            "mdbase_record_cache",
            "kanban_boards",
            "events",
            "vector_index_state",
            "vector_clusters",
            "chunks",
            "search_chunk_content",
            "search_chunks_fts",
            "diagnostics",
            "meta",
        ] {
            assert!(table_exists(database.connection(), table_name));
        }

        assert_eq!(
            database
                .meta_value(META_SCHEMA_VERSION)
                .expect("schema version should be stored"),
            Some(crate::SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            database
                .meta_value(META_PARSER_VERSION)
                .expect("parser version should be stored"),
            Some(PARSER_VERSION.to_string())
        );
        assert_eq!(
            database
                .meta_value(META_EXTRACTION_VERSION)
                .expect("extraction version should be stored"),
            Some(EXTRACTION_VERSION.to_string())
        );
    }

    #[cfg(target_os = "android")]
    #[test]
    fn android_cache_open_creates_private_database_and_wal() {
        let temporary = TempDir::new().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        crate::initialize_vulcan_dir(&paths).expect("initialize vault");
        let database = CacheDatabase::open(&paths).expect("open private cache");
        assert!(paths.cache_db().exists());
        assert!(!paths.vulcan_dir().join("cache.db").exists());
        assert_eq!(query_journal_mode(database.connection()), "wal");
    }

    #[test]
    fn clear_all_removes_cached_rows_but_preserves_runtime_metadata() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        crate::initialize_vulcan_dir(&paths).expect(".vulcan dir should be created");
        let mut database = CacheDatabase::open(&paths).expect("database should open");

        insert_document(database.connection(), "doc-1", "one.md");
        database
            .connection()
            .execute(
                "
                INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
                VALUES (?1, NULL, 'parse_error', 'bad yaml', '{}', '2026-03-20T00:00:00Z')
                ",
                params![Ulid::new().to_string()],
            )
            .expect("diagnostic row should insert");

        database.clear_all().expect("clear should succeed");

        assert_eq!(count_rows(database.connection(), "documents"), 0);
        assert_eq!(count_rows(database.connection(), "diagnostics"), 0);
        assert_eq!(
            database
                .meta_value(META_SCHEMA_VERSION)
                .expect("schema version should remain readable"),
            Some(crate::SCHEMA_VERSION.to_string())
        );
    }

    #[test]
    fn rebuild_with_resets_existing_rows_before_running_callback() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        crate::initialize_vulcan_dir(&paths).expect(".vulcan dir should be created");
        let mut database = CacheDatabase::open(&paths).expect("database should open");

        insert_document(database.connection(), "doc-1", "one.md");

        let inserted_id = database
            .rebuild_with(|transaction| {
                transaction.execute(
                    "
                    INSERT INTO documents (
                        id,
                        path,
                        filename,
                        extension,
                        content_hash,
                        raw_frontmatter,
                        file_size,
                        file_mtime,
                        parser_version,
                        indexed_at
                    )
                    VALUES (?1, 'two.md', 'two', 'md', ?2, NULL, 42, 123456, ?3, '2026-03-20T00:00:00Z')
                    ",
                    params![
                        "doc-2",
                        vec![2_u8; 32],
                        crate::PARSER_VERSION,
                    ],
                )?;

                rebuild_note_store(transaction).map_err(CacheError::from)?;
                Ok::<_, CacheError>("doc-2".to_string())
            })
            .expect("rebuild should succeed");

        assert_eq!(inserted_id, "doc-2");
        assert_eq!(count_rows(database.connection(), "documents"), 1);
        assert_eq!(
            document_paths(database.connection()),
            vec!["two.md".to_string()]
        );
        let note_paths: Vec<String> = database
            .connection()
            .prepare("SELECT path FROM note_query")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(note_paths, vec!["two.md".to_string()]);
    }

    /// A trigger added to a core table is suspended for every full rebuild,
    /// so whatever it maintains must be rebuilt in bulk there. Review that
    /// before extending this list.
    #[test]
    fn a_rebuild_suspends_only_the_reviewed_derived_state_triggers() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        crate::initialize_vulcan_dir(&paths).expect(".vulcan dir should be created");
        let mut database = CacheDatabase::open(&paths).expect("database should open");
        let trigger_count = |connection: &Connection| {
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'trigger'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        };
        let before = trigger_count(database.connection());

        database
            .with_transaction(|transaction| {
                let suspended = suspend_rebuilt_triggers(transaction)?;
                assert_eq!(
                    suspended.names(),
                    [
                        "checkpoint_link_delete",
                        "checkpoint_link_insert",
                        "checkpoint_link_target_delete",
                        "checkpoint_link_target_rename",
                        "checkpoint_link_update",
                        "checkpoint_orphan_link_delete",
                        "checkpoint_orphan_link_insert",
                        "checkpoint_orphan_link_update",
                        "checkpoint_path_delete",
                        "checkpoint_path_insert",
                        "checkpoint_path_update",
                        "checkpoint_property_delete",
                        "checkpoint_property_insert",
                        "checkpoint_property_update",
                        "graph_confidence_extension_update",
                        "graph_confidence_link_delete",
                        "graph_confidence_link_insert",
                        "graph_confidence_link_update",
                        "note_query_aliases_delete",
                        "note_query_aliases_insert",
                        "note_query_document_delete",
                        "note_query_document_insert",
                        "note_query_document_update",
                        "note_query_properties_delete",
                        "note_query_properties_insert",
                        "note_query_properties_update",
                        "note_query_tags_delete",
                        "note_query_tags_insert",
                        "note_query_version_delete",
                        "note_query_version_insert",
                        "note_query_version_update",
                    ]
                );
                assert_eq!(
                    trigger_count(transaction),
                    before - 31,
                    "suspended triggers are dropped"
                );
                suspended.restore(transaction)?;
                Ok::<_, CacheError>(())
            })
            .unwrap();
        assert_eq!(trigger_count(database.connection()), before);

        database.clear_all().unwrap();
        assert_eq!(
            trigger_count(database.connection()),
            before,
            "a rebuild restores every trigger it suspended"
        );
    }

    fn insert_document(connection: &Connection, id: &str, path: &str) {
        connection
            .execute(
                "
                INSERT INTO documents (
                    id,
                    path,
                    filename,
                    extension,
                    content_hash,
                    raw_frontmatter,
                    file_size,
                    file_mtime,
                    parser_version,
                    indexed_at
                )
                VALUES (?1, ?2, 'one', 'md', ?3, NULL, 1, 1, ?4, '2026-03-20T00:00:00Z')
                ",
                params![id, path, vec![1_u8; 32], crate::PARSER_VERSION],
            )
            .expect("document row should insert");
    }

    fn count_rows(connection: &Connection, table_name: &str) -> i64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table_name}"), [], |row| {
                row.get(0)
            })
            .expect("row count should be readable")
    }

    fn document_paths(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT path FROM documents ORDER BY path")
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| row.get(0))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn query_journal_mode(connection: &Connection) -> String {
        connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("journal_mode should be readable")
    }

    fn query_foreign_keys(connection: &Connection) -> i64 {
        connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("foreign_keys should be readable")
    }

    fn query_busy_timeout(connection: &Connection) -> i64 {
        connection
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("busy_timeout should be readable")
    }

    fn table_exists(connection: &Connection, table_name: &str) -> bool {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table_name],
                |row| row.get::<_, i64>(0),
            )
            .expect("sqlite_master query should succeed")
            > 0
    }
}
