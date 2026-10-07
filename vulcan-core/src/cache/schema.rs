use rusqlite::Transaction;

/// A covering index for note identity facts (QRY.6): loading a universe's
/// paths, file names, aliases, and row versions reads the index alone, not
/// the `note_query` rows that carry properties.
pub fn apply_schema_v29(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE INDEX idx_note_query_identity
             ON note_query(path, filename, aliases, row_version);",
    )
}

/// Widen the identity index (QRY.6) so read scopes on path and extension
/// apply per row, and document ids for incremental refreshes, come from the
/// index alone.
/// Change tracking that lets a scan checkpoint touch only documents whose
/// state can have changed. A document's checkpoint row depends on its
/// `note_query` path, kind, revision, and mtime (tracked here by path, so
/// renames and deletions record the old path), its own links (already
/// link-dirty), and its inbound resolved links: link writes mark their old
/// and new resolved targets as orphan candidates. Earlier changes were not
/// tracked, so the next checkpoint is full.
///
/// The record cache's completeness count tests two wide-row columns; the
/// expression index answers it without reading overflow pages.
pub fn apply_schema_v31(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE TABLE checkpoint_path_dirty (path TEXT PRIMARY KEY);
         CREATE TRIGGER checkpoint_path_insert AFTER INSERT ON note_query BEGIN
             INSERT INTO checkpoint_path_dirty SELECT new.path
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_path_dirty WHERE path = new.path);
         END;
         CREATE TRIGGER checkpoint_path_delete AFTER DELETE ON note_query BEGIN
             INSERT INTO checkpoint_path_dirty SELECT old.path
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_path_dirty WHERE path = old.path);
         END;
         CREATE TRIGGER checkpoint_path_update AFTER UPDATE ON note_query
         WHEN old.path IS NOT new.path OR old.extension IS NOT new.extension
           OR old.revision IS NOT new.revision OR old.file_mtime IS NOT new.file_mtime BEGIN
             INSERT INTO checkpoint_path_dirty SELECT old.path
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_path_dirty WHERE path = old.path);
             INSERT INTO checkpoint_path_dirty SELECT new.path
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_path_dirty WHERE path = new.path);
         END;
         CREATE TABLE checkpoint_orphan_dirty_documents (document_id TEXT PRIMARY KEY);
         CREATE TRIGGER checkpoint_orphan_link_insert AFTER INSERT ON links
         WHEN new.resolved_target_id IS NOT NULL BEGIN
             INSERT INTO checkpoint_orphan_dirty_documents SELECT new.resolved_target_id
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_orphan_dirty_documents
                               WHERE document_id = new.resolved_target_id);
         END;
         CREATE TRIGGER checkpoint_orphan_link_delete AFTER DELETE ON links
         WHEN old.resolved_target_id IS NOT NULL BEGIN
             INSERT INTO checkpoint_orphan_dirty_documents SELECT old.resolved_target_id
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_orphan_dirty_documents
                               WHERE document_id = old.resolved_target_id);
         END;
         CREATE TRIGGER checkpoint_orphan_link_update AFTER UPDATE ON links
         WHEN old.resolved_target_id IS NOT new.resolved_target_id
           OR old.source_document_id IS NOT new.source_document_id BEGIN
             INSERT INTO checkpoint_orphan_dirty_documents SELECT old.resolved_target_id
             WHERE old.resolved_target_id IS NOT NULL AND NOT EXISTS (
                 SELECT 1 FROM checkpoint_orphan_dirty_documents
                 WHERE document_id = old.resolved_target_id);
             INSERT INTO checkpoint_orphan_dirty_documents SELECT new.resolved_target_id
             WHERE new.resolved_target_id IS NOT NULL AND NOT EXISTS (
                 SELECT 1 FROM checkpoint_orphan_dirty_documents
                 WHERE document_id = new.resolved_target_id);
         END;
         CREATE INDEX idx_mdbase_record_cache_complete ON mdbase_record_cache(
             collection_root, dependency_digest, record_model_version,
             metadata_json IS NOT NULL, local_record_json IS NOT NULL);
         INSERT OR IGNORE INTO meta(key, value) VALUES ('checkpoint_reset', '1');",
    )
}

pub fn apply_schema_v30(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "DROP INDEX idx_note_query_identity;
         CREATE INDEX idx_note_query_identity
             ON note_query(path, filename, extension, row_version, document_id, aliases);",
    )
}

/// Row versions for the note store (QRY.6). Every insert or update of a
/// `note_query` row (documents, properties, tags, and aliases all write
/// through it) advances the store clock and stamps the row with the new
/// value, and every delete advances the clock, so a retained reader can tell
/// exactly which rows changed. The clock is never cleared with the cache
/// tables; `store_id` is random per cache file, so versions from a deleted
/// and recreated cache never match retained ones.
pub fn apply_schema_v28(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE TABLE note_store_clock (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             store_id TEXT NOT NULL,
             version INTEGER NOT NULL
         );
         INSERT INTO note_store_clock VALUES (1, lower(hex(randomblob(16))), 0);
         ALTER TABLE note_query ADD COLUMN row_version INTEGER NOT NULL DEFAULT 0;
         CREATE INDEX idx_note_query_row_version ON note_query(row_version);
         CREATE TRIGGER note_query_version_insert AFTER INSERT ON note_query BEGIN
             UPDATE note_store_clock SET version = version + 1;
             UPDATE note_query SET row_version = (SELECT version FROM note_store_clock)
             WHERE document_id = new.document_id;
         END;
         CREATE TRIGGER note_query_version_update AFTER UPDATE ON note_query
         WHEN new.row_version = old.row_version BEGIN
             UPDATE note_store_clock SET version = version + 1;
             UPDATE note_query SET row_version = (SELECT version FROM note_store_clock)
             WHERE document_id = new.document_id;
         END;
         CREATE TRIGGER note_query_version_delete AFTER DELETE ON note_query BEGIN
             UPDATE note_store_clock SET version = version + 1;
         END;
         UPDATE note_query SET row_version = rowid;
         UPDATE note_store_clock SET version = (SELECT coalesce(max(row_version), 0) FROM note_query);",
    )
}

/// The narrow note query table (QRY.4): one row per document with its
/// identity facts (path, file name, aliases), freshness evidence (stat
/// fingerprint, revision, parser version), file metadata, JSONB properties,
/// and tag membership, maintained by triggers from the normalized tables so
/// it is rebuilt with them. Scans record stat fingerprints on `documents`.
pub fn apply_schema_v27(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "ALTER TABLE documents ADD COLUMN stat_fingerprint BLOB;
         CREATE TABLE note_query (
             document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
             path TEXT NOT NULL UNIQUE,
             filename TEXT NOT NULL,
             extension TEXT NOT NULL,
             stat_fingerprint BLOB,
             revision BLOB NOT NULL,
             parser_version INTEGER NOT NULL,
             file_size INTEGER NOT NULL,
             file_mtime INTEGER NOT NULL,
             file_ctime INTEGER,
             properties BLOB,
             tags TEXT NOT NULL DEFAULT '[]',
             aliases TEXT NOT NULL DEFAULT '[]'
         );
         CREATE INDEX idx_note_query_filename ON note_query(filename);
         INSERT INTO note_query (
             document_id, path, filename, extension, stat_fingerprint, revision,
             parser_version, file_size, file_mtime, file_ctime, properties, tags, aliases
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
                     ORDER BY aliases.rowid))
         FROM documents;
         CREATE TRIGGER note_query_document_insert AFTER INSERT ON documents BEGIN
             INSERT INTO note_query (
                 document_id, path, filename, extension, stat_fingerprint, revision,
                 parser_version, file_size, file_mtime, file_ctime
             ) VALUES (
                 new.id, new.path, new.filename, new.extension, new.stat_fingerprint,
                 new.content_hash, new.parser_version, new.file_size, new.file_mtime,
                 new.file_ctime
             );
         END;
         CREATE TRIGGER note_query_document_update AFTER UPDATE ON documents BEGIN
             UPDATE note_query SET
                 path = new.path, filename = new.filename, extension = new.extension,
                 stat_fingerprint = new.stat_fingerprint, revision = new.content_hash,
                 parser_version = new.parser_version, file_size = new.file_size,
                 file_mtime = new.file_mtime, file_ctime = new.file_ctime
             WHERE document_id = new.id;
         END;
         CREATE TRIGGER note_query_document_delete AFTER DELETE ON documents BEGIN
             DELETE FROM note_query WHERE document_id = old.id;
         END;
         CREATE TRIGGER note_query_properties_insert AFTER INSERT ON properties BEGIN
             UPDATE note_query SET properties = jsonb(new.canonical_json)
             WHERE document_id = new.document_id;
         END;
         CREATE TRIGGER note_query_properties_update AFTER UPDATE ON properties BEGIN
             UPDATE note_query SET properties = jsonb(new.canonical_json)
             WHERE document_id = new.document_id;
         END;
         CREATE TRIGGER note_query_properties_delete AFTER DELETE ON properties BEGIN
             UPDATE note_query SET properties = NULL WHERE document_id = old.document_id;
         END;
         CREATE TRIGGER note_query_tags_insert AFTER INSERT ON tags BEGIN
             UPDATE note_query SET tags = (
                 SELECT json_group_array(tag_text) FROM (SELECT tag_text FROM tags
                     WHERE tags.document_id = new.document_id ORDER BY tags.rowid))
             WHERE document_id = new.document_id;
         END;
         CREATE TRIGGER note_query_tags_delete AFTER DELETE ON tags BEGIN
             UPDATE note_query SET tags = (
                 SELECT json_group_array(tag_text) FROM (SELECT tag_text FROM tags
                     WHERE tags.document_id = old.document_id ORDER BY tags.rowid))
             WHERE document_id = old.document_id;
         END;
         CREATE TRIGGER note_query_aliases_insert AFTER INSERT ON aliases BEGIN
             UPDATE note_query SET aliases = (
                 SELECT json_group_array(alias_text) FROM (SELECT alias_text FROM aliases
                     WHERE aliases.document_id = new.document_id ORDER BY aliases.rowid))
             WHERE document_id = new.document_id;
         END;
         CREATE TRIGGER note_query_aliases_delete AFTER DELETE ON aliases BEGIN
             UPDATE note_query SET aliases = (
                 SELECT json_group_array(alias_text) FROM (SELECT alias_text FROM aliases
                     WHERE aliases.document_id = old.document_id ORDER BY aliases.rowid))
             WHERE document_id = old.document_id;
         END;",
    )
}

/// Record `file.ctime` at scan time so note queries need no per-note
/// `stat`. Existing rows stay NULL until the next scan records them;
/// readers fall back to the filesystem meanwhile.
pub fn apply_schema_v26(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch("ALTER TABLE documents ADD COLUMN file_ctime INTEGER;")
}

/// Link-hash invalidation for incremental scan checkpoints. A document's
/// checkpoint link hash covers its link rows and the paths of their resolved
/// targets, so link writes and target renames or deletions mark the source.
/// The next checkpoint is full because earlier changes were not tracked.
/// Guards instead of `OR IGNORE`: foreign-key actions that fire these
/// triggers do not honor the trigger's own conflict policy.
pub fn apply_schema_v25(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE TABLE checkpoint_link_dirty_documents (document_id TEXT PRIMARY KEY);
         CREATE TRIGGER checkpoint_link_insert AFTER INSERT ON links BEGIN
             INSERT INTO checkpoint_link_dirty_documents SELECT new.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_link_dirty_documents
                               WHERE document_id = new.source_document_id);
         END;
         CREATE TRIGGER checkpoint_link_delete AFTER DELETE ON links BEGIN
             INSERT INTO checkpoint_link_dirty_documents SELECT old.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_link_dirty_documents
                               WHERE document_id = old.source_document_id);
         END;
         CREATE TRIGGER checkpoint_link_update AFTER UPDATE ON links BEGIN
             INSERT INTO checkpoint_link_dirty_documents SELECT old.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_link_dirty_documents
                               WHERE document_id = old.source_document_id);
             INSERT INTO checkpoint_link_dirty_documents SELECT new.source_document_id
             WHERE NOT EXISTS (SELECT 1 FROM checkpoint_link_dirty_documents
                               WHERE document_id = new.source_document_id);
         END;
         CREATE TRIGGER checkpoint_link_target_rename AFTER UPDATE OF path ON documents
         WHEN old.path IS NOT new.path BEGIN
             INSERT INTO checkpoint_link_dirty_documents
             SELECT DISTINCT source_document_id FROM links
             WHERE resolved_target_id = new.id AND source_document_id NOT IN
                 (SELECT document_id FROM checkpoint_link_dirty_documents);
         END;
         CREATE TRIGGER checkpoint_link_target_delete BEFORE DELETE ON documents BEGIN
             INSERT INTO checkpoint_link_dirty_documents
             SELECT DISTINCT source_document_id FROM links
             WHERE resolved_target_id = old.id AND source_document_id NOT IN
                 (SELECT document_id FROM checkpoint_link_dirty_documents);
         END;
         INSERT OR IGNORE INTO meta(key, value) VALUES ('checkpoint_reset', '1');",
    )
}

/// Per-record identity facts (types, basename, authored ID, uniqueness
/// values) let a refresh prove that changed records leave every other
/// record's overlay unchanged. Rows without them are rewritten by refresh.
pub fn apply_schema_v24(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch("ALTER TABLE mdbase_record_query ADD COLUMN identity_json TEXT;")
}

/// Narrow per-record query rows for indexed reads. Wide cache rows carry large
/// body-bearing payloads that spill to overflow pages, so columns read for every
/// record (stat fingerprint, query-input evidence, effective frontmatter, file
/// metadata) live here inline. Triggers delete a row whenever its source cache
/// row is rewritten or removed; refresh republishes it in the same transaction.
pub fn apply_schema_v23(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE TABLE mdbase_record_query (
            collection_root TEXT NOT NULL,
            path TEXT NOT NULL,
            revision TEXT NOT NULL,
            dependency_digest TEXT NOT NULL,
            record_model_version INTEGER NOT NULL,
            stat_fingerprint BLOB,
            input_converted INTEGER NOT NULL,
            input_bytes INTEGER NOT NULL,
            input_nodes INTEGER NOT NULL,
            input_width INTEGER NOT NULL,
            input_links INTEGER NOT NULL,
            effective_frontmatter_jsonb BLOB NOT NULL,
            file_json TEXT NOT NULL,
            PRIMARY KEY (collection_root, path)
         ) WITHOUT ROWID;
         CREATE INDEX idx_mdbase_record_query_freshness ON mdbase_record_query(
            collection_root, dependency_digest, record_model_version, path, stat_fingerprint,
            revision
         );
         CREATE TRIGGER mdbase_record_query_update AFTER UPDATE ON mdbase_record_cache BEGIN
            DELETE FROM mdbase_record_query
            WHERE collection_root = old.collection_root AND path = old.path
              AND (new.revision IS NOT old.revision
                OR new.dependency_digest IS NOT old.dependency_digest
                OR new.record_model_version IS NOT old.record_model_version
                OR new.types_json IS NOT old.types_json
                OR new.effective_frontmatter_json IS NOT old.effective_frontmatter_json
                OR new.metadata_json IS NOT old.metadata_json
                OR new.collection_root IS NOT old.collection_root
                OR new.path IS NOT old.path);
         END;
         CREATE TRIGGER mdbase_record_query_delete AFTER DELETE ON mdbase_record_cache BEGIN
            DELETE FROM mdbase_record_query
            WHERE collection_root = old.collection_root AND path = old.path;
         END;",
    )
}

/// Record-local derivation precedes visibility-sensitive collection overlays.
/// Legacy rows must derive this payload from source, not from final diagnostics.
pub fn apply_schema_v22(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch("ALTER TABLE mdbase_record_cache ADD COLUMN local_record_json TEXT;")
}

/// Legacy projections lack persisted values and cannot become complete metadata
/// snapshots through migration alone. Null payloads are repaired from the vault.
pub fn apply_schema_v21(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch("ALTER TABLE mdbase_record_cache ADD COLUMN metadata_json TEXT;")
}

/// Derived type membership for indexed MDB candidate selection. Triggers keep
/// membership and the source projection in the same `SQLite` transaction.
pub fn apply_schema_v20(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "CREATE TABLE mdbase_record_types (
            collection_root TEXT NOT NULL,
            type_name TEXT NOT NULL,
            path TEXT NOT NULL,
            PRIMARY KEY (collection_root, type_name, path),
            FOREIGN KEY (collection_root, path)
                REFERENCES mdbase_record_cache(collection_root, path) ON DELETE CASCADE
         ) WITHOUT ROWID;
         CREATE INDEX idx_mdbase_record_types_path ON mdbase_record_types(collection_root, path);
         INSERT OR IGNORE INTO mdbase_record_types
            SELECT record.collection_root, lower(member.value), record.path
            FROM mdbase_record_cache AS record, json_each(record.types_json) AS member;
         CREATE TRIGGER mdbase_record_types_insert AFTER INSERT ON mdbase_record_cache BEGIN
            INSERT OR IGNORE INTO mdbase_record_types
                SELECT new.collection_root, lower(value), new.path FROM json_each(new.types_json);
         END;
         CREATE TRIGGER mdbase_record_types_update
         AFTER UPDATE OF collection_root, path, types_json ON mdbase_record_cache BEGIN
            DELETE FROM mdbase_record_types WHERE collection_root = old.collection_root AND path = old.path;
            INSERT OR IGNORE INTO mdbase_record_types
                SELECT new.collection_root, lower(value), new.path FROM json_each(new.types_json);
         END;
         CREATE TRIGGER mdbase_record_types_delete AFTER DELETE ON mdbase_record_cache BEGIN
            DELETE FROM mdbase_record_types WHERE collection_root = old.collection_root AND path = old.path;
         END;",
    )
}

/// Old snapshots remain self-contained and readable. New automatic snapshots use
/// half-open version intervals; they never depend on the lifetime of a header.
pub fn apply_schema_v19(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "ALTER TABLE checkpoints ADD COLUMN generation INTEGER;
         CREATE UNIQUE INDEX idx_checkpoint_generation ON checkpoints(generation);
         CREATE TABLE checkpoint_document_versions (
             path TEXT NOT NULL,
             valid_from INTEGER NOT NULL,
             valid_to INTEGER CHECK(valid_to > valid_from),
             document_kind TEXT NOT NULL,
             content_hash TEXT NOT NULL,
             link_hash TEXT NOT NULL,
             property_hash TEXT NOT NULL,
             embedding_hash TEXT NOT NULL,
             orphan INTEGER NOT NULL,
             stale INTEGER NOT NULL,
             PRIMARY KEY(path, valid_from)
         );
         CREATE UNIQUE INDEX idx_checkpoint_version_current
             ON checkpoint_document_versions(path) WHERE valid_to IS NULL;
         CREATE INDEX idx_checkpoint_version_end
             ON checkpoint_document_versions(valid_to) WHERE valid_to IS NOT NULL;
         CREATE TABLE checkpoint_dirty_documents (document_id TEXT PRIMARY KEY);
         CREATE TRIGGER checkpoint_property_insert AFTER INSERT ON properties BEGIN
             INSERT OR IGNORE INTO checkpoint_dirty_documents VALUES (new.document_id);
         END;
         CREATE TRIGGER checkpoint_property_delete AFTER DELETE ON properties BEGIN
             INSERT OR IGNORE INTO checkpoint_dirty_documents VALUES (old.document_id);
         END;
         CREATE TRIGGER checkpoint_property_update AFTER UPDATE ON properties
         WHEN old.canonical_json IS NOT new.canonical_json OR old.document_id IS NOT new.document_id BEGIN
             INSERT OR IGNORE INTO checkpoint_dirty_documents VALUES (old.document_id);
             INSERT OR IGNORE INTO checkpoint_dirty_documents VALUES (new.document_id);
         END;
         CREATE TABLE checkpoint_vector_inputs (
             chunk_id TEXT PRIMARY KEY,
             document_id TEXT NOT NULL,
             content_hash BLOB NOT NULL,
             sequence_index INTEGER NOT NULL,
             model TEXT NOT NULL
         );
         CREATE INDEX idx_checkpoint_vector_document ON checkpoint_vector_inputs(document_id);",
    )
}

pub const TABLES_TO_CLEAR: &[&str] = &[
    "mdbase_record_cache",
    "link_suggestions",
    "graph_clusters",
    "kanban_boards",
    "events",
    "tasknotes_tasks",
    "task_properties",
    "tasks",
    "list_items",
    "tasks_blocks",
    "inline_expressions",
    "dataview_blocks",
    "headings",
    "block_refs",
    "links",
    "aliases",
    "tags",
    "property_list_items",
    "property_values",
    "properties",
    "property_catalog",
    "vector_clusters",
    "vector_index_state",
    "vector_model_registry",
    "search_chunk_content",
    "chunks",
    "diagnostics",
    "documents",
];

pub fn apply_schema_v1(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS documents (
            id TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            filename TEXT NOT NULL,
            extension TEXT NOT NULL,
            content_hash BLOB NOT NULL,
            raw_frontmatter TEXT,
            file_size INTEGER NOT NULL,
            file_mtime INTEGER NOT NULL,
            parser_version INTEGER NOT NULL,
            indexed_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS headings (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            level INTEGER NOT NULL,
            text TEXT NOT NULL,
            byte_offset INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS block_refs (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            block_id_text TEXT NOT NULL,
            block_id_byte_offset INTEGER NOT NULL,
            target_block_byte_start INTEGER NOT NULL,
            target_block_byte_end INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS links (
            id TEXT PRIMARY KEY,
            source_document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            raw_text TEXT NOT NULL,
            link_kind TEXT NOT NULL,
            display_text TEXT,
            target_path_candidate TEXT,
            target_heading TEXT,
            target_block TEXT,
            resolved_target_id TEXT REFERENCES documents(id) ON DELETE SET NULL,
            origin_context TEXT NOT NULL,
            byte_offset INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS aliases (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            alias_text TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tags (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            tag_text TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS chunks (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            sequence_index INTEGER NOT NULL,
            heading_path TEXT NOT NULL,
            byte_offset_start INTEGER NOT NULL,
            byte_offset_end INTEGER NOT NULL,
            content_hash BLOB NOT NULL,
            chunk_strategy TEXT NOT NULL,
            chunk_version INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS diagnostics (
            id TEXT PRIMARY KEY,
            document_id TEXT REFERENCES documents(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            message TEXT NOT NULL,
            detail TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_documents_path ON documents(path);
        CREATE INDEX IF NOT EXISTS idx_documents_content_hash ON documents(content_hash);
        CREATE INDEX IF NOT EXISTS idx_links_source_document_id ON links(source_document_id);
        CREATE INDEX IF NOT EXISTS idx_links_resolved_target_id ON links(resolved_target_id);
        CREATE INDEX IF NOT EXISTS idx_aliases_document_id ON aliases(document_id);
        CREATE INDEX IF NOT EXISTS idx_aliases_alias_text ON aliases(alias_text);
        CREATE INDEX IF NOT EXISTS idx_tags_tag_text ON tags(tag_text);
        CREATE INDEX IF NOT EXISTS idx_chunks_document_id ON chunks(document_id);
        ",
    )?;

    Ok(())
}

pub fn apply_schema_v2(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "ALTER TABLE chunks ADD COLUMN content TEXT NOT NULL DEFAULT ''",
        [],
    )?;
    Ok(())
}

pub fn apply_schema_v3(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    create_search_schema(transaction)
}

pub fn apply_schema_v4(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        DROP TRIGGER IF EXISTS chunk_search_content_ai;
        DROP TRIGGER IF EXISTS chunk_search_content_ad;
        DROP TRIGGER IF EXISTS chunk_search_content_au;
        DROP TABLE IF EXISTS chunk_search;
        DROP TABLE IF EXISTS chunk_search_content;

        DROP TRIGGER IF EXISTS search_chunk_content_ai;
        DROP TRIGGER IF EXISTS search_chunk_content_ad;
        DROP TRIGGER IF EXISTS search_chunk_content_au;
        DROP TABLE IF EXISTS search_chunks_fts;
        DROP TABLE IF EXISTS search_chunk_content;
        ",
    )?;

    create_search_schema(transaction)
}

pub fn apply_schema_v5(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS properties (
            document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            raw_yaml TEXT NOT NULL,
            canonical_json TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS property_values (
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            key TEXT NOT NULL,
            value_text TEXT,
            value_number REAL,
            value_bool INTEGER,
            value_date TEXT,
            value_type TEXT NOT NULL,
            PRIMARY KEY (document_id, key)
        );

        CREATE TABLE IF NOT EXISTS property_list_items (
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            key TEXT NOT NULL,
            item_index INTEGER NOT NULL,
            value_text TEXT NOT NULL,
            PRIMARY KEY (document_id, key, item_index)
        );

        CREATE TABLE IF NOT EXISTS property_catalog (
            key TEXT NOT NULL,
            observed_type TEXT NOT NULL,
            usage_count INTEGER NOT NULL,
            namespace TEXT NOT NULL,
            PRIMARY KEY (key, observed_type, namespace)
        );

        CREATE INDEX IF NOT EXISTS idx_property_values_key ON property_values(key);
        CREATE INDEX IF NOT EXISTS idx_property_values_key_text
            ON property_values(key, value_text);
        CREATE INDEX IF NOT EXISTS idx_property_values_key_number
            ON property_values(key, value_number);
        CREATE INDEX IF NOT EXISTS idx_property_values_key_bool
            ON property_values(key, value_bool);
        CREATE INDEX IF NOT EXISTS idx_property_values_key_date
            ON property_values(key, value_date);
        CREATE INDEX IF NOT EXISTS idx_property_list_items_key_value
            ON property_list_items(key, value_text);
        CREATE INDEX IF NOT EXISTS idx_property_catalog_key
            ON property_catalog(key);
        ",
    )?;

    Ok(())
}

pub fn apply_schema_v6(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS vector_index_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            provider_name TEXT NOT NULL,
            model_name TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            normalized INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS vector_clusters (
            provider_name TEXT NOT NULL,
            model_name TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            cluster_id INTEGER NOT NULL,
            cluster_label TEXT NOT NULL,
            chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
            PRIMARY KEY (provider_name, model_name, dimensions, chunk_id)
        );

        CREATE INDEX IF NOT EXISTS idx_vector_clusters_model_cluster
            ON vector_clusters(provider_name, model_name, dimensions, cluster_id);
        ",
    )?;

    Ok(())
}

pub fn apply_schema_v7(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS checkpoints (
            id TEXT PRIMARY KEY,
            name TEXT,
            source TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            note_count INTEGER NOT NULL,
            orphan_notes INTEGER NOT NULL,
            stale_notes INTEGER NOT NULL,
            resolved_links INTEGER NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_checkpoints_name
            ON checkpoints(name);
        CREATE INDEX IF NOT EXISTS idx_checkpoints_source_created_at
            ON checkpoints(source, created_at DESC);

        CREATE TABLE IF NOT EXISTS checkpoint_documents (
            checkpoint_id TEXT NOT NULL REFERENCES checkpoints(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            document_kind TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            link_hash TEXT NOT NULL,
            property_hash TEXT NOT NULL,
            embedding_hash TEXT NOT NULL,
            orphan INTEGER NOT NULL,
            stale INTEGER NOT NULL,
            PRIMARY KEY (checkpoint_id, path)
        );

        CREATE INDEX IF NOT EXISTS idx_checkpoint_documents_checkpoint
            ON checkpoint_documents(checkpoint_id);
        ",
    )?;

    Ok(())
}

/// Drop the FTS sync triggers to avoid per-row tokenization during bulk writes.
/// Call `restore_fts_triggers` + `rebuild_search_index` after the bulk write completes.
pub(crate) fn drop_fts_triggers(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        DROP TRIGGER IF EXISTS search_chunk_content_ai;
        DROP TRIGGER IF EXISTS search_chunk_content_ad;
        DROP TRIGGER IF EXISTS search_chunk_content_au;
        ",
    )?;
    Ok(())
}

/// Recreate the FTS sync triggers after a bulk write. Call `rebuild_search_index` first.
pub(crate) fn restore_fts_triggers(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS search_chunk_content_ai AFTER INSERT ON search_chunk_content BEGIN
            INSERT INTO search_chunks_fts(rowid, content, document_title, aliases, headings)
            VALUES (new.id, new.content, new.document_title, new.aliases, new.headings);
        END;

        CREATE TRIGGER IF NOT EXISTS search_chunk_content_ad AFTER DELETE ON search_chunk_content BEGIN
            INSERT INTO search_chunks_fts(search_chunks_fts, rowid, content, document_title, aliases, headings)
            VALUES ('delete', old.id, old.content, old.document_title, old.aliases, old.headings);
        END;

        CREATE TRIGGER IF NOT EXISTS search_chunk_content_au AFTER UPDATE ON search_chunk_content BEGIN
            INSERT INTO search_chunks_fts(search_chunks_fts, rowid, content, document_title, aliases, headings)
            VALUES ('delete', old.id, old.content, old.document_title, old.aliases, old.headings);
            INSERT INTO search_chunks_fts(rowid, content, document_title, aliases, headings)
            VALUES (new.id, new.content, new.document_title, new.aliases, new.headings);
        END;
        ",
    )?;
    Ok(())
}

/// Rebuild only the FTS5 index from the already-correct `search_chunk_content` table.
/// Use this after bulk writes with triggers disabled — the content table is already up to date,
/// so we only need to re-sync the FTS virtual table.
pub(crate) fn rebuild_fts_index(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction
        .execute_batch("INSERT INTO search_chunks_fts(search_chunks_fts) VALUES ('rebuild');")?;
    Ok(())
}

pub(crate) fn rebuild_search_index(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        DELETE FROM search_chunk_content;

        INSERT INTO search_chunk_content (
            chunk_id,
            document_id,
            content,
            document_title,
            aliases,
            headings
        )
        SELECT
            chunks.id,
            chunks.document_id,
            chunks.content,
            documents.filename,
            COALESCE((
                SELECT group_concat(alias_text, ' ')
                FROM aliases
                WHERE aliases.document_id = chunks.document_id
            ), ''),
            COALESCE((
                SELECT group_concat(value, ' ')
                FROM json_each(chunks.heading_path)
            ), '')
        FROM chunks
        JOIN documents ON documents.id = chunks.document_id;

        INSERT INTO search_chunks_fts(search_chunks_fts) VALUES ('rebuild');
        ",
    )?;
    Ok(())
}

fn create_search_schema(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE search_chunk_content (
            id INTEGER PRIMARY KEY,
            chunk_id TEXT NOT NULL UNIQUE REFERENCES chunks(id) ON DELETE CASCADE,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            content TEXT NOT NULL,
            document_title TEXT NOT NULL,
            aliases TEXT NOT NULL,
            headings TEXT NOT NULL
        );

        CREATE INDEX idx_search_chunk_content_document_id
            ON search_chunk_content(document_id);

        CREATE VIRTUAL TABLE search_chunks_fts USING fts5(
            content,
            document_title,
            aliases,
            headings,
            content = 'search_chunk_content',
            content_rowid = 'id',
            tokenize = 'unicode61'
        );

        CREATE TRIGGER search_chunk_content_ai AFTER INSERT ON search_chunk_content BEGIN
            INSERT INTO search_chunks_fts(rowid, content, document_title, aliases, headings)
            VALUES (new.id, new.content, new.document_title, new.aliases, new.headings);
        END;

        CREATE TRIGGER search_chunk_content_ad AFTER DELETE ON search_chunk_content BEGIN
            INSERT INTO search_chunks_fts(search_chunks_fts, rowid, content, document_title, aliases, headings)
            VALUES ('delete', old.id, old.content, old.document_title, old.aliases, old.headings);
        END;

        CREATE TRIGGER search_chunk_content_au AFTER UPDATE ON search_chunk_content BEGIN
            INSERT INTO search_chunks_fts(search_chunks_fts, rowid, content, document_title, aliases, headings)
            VALUES ('delete', old.id, old.content, old.document_title, old.aliases, old.headings);
            INSERT INTO search_chunks_fts(rowid, content, document_title, aliases, headings)
            VALUES (new.id, new.content, new.document_title, new.aliases, new.headings);
        END;

        ",
    )?;
    rebuild_search_index(transaction)?;
    Ok(())
}

pub fn apply_schema_v8(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS vector_model_registry (
            cache_key TEXT PRIMARY KEY,
            table_name TEXT NOT NULL UNIQUE,
            provider_name TEXT NOT NULL,
            model_name TEXT NOT NULL,
            dimensions INTEGER NOT NULL,
            normalized INTEGER NOT NULL,
            is_active INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )?;

    Ok(())
}

pub fn apply_schema_v9(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_documents_extension ON documents(extension);
        CREATE INDEX IF NOT EXISTS idx_tags_document_id ON tags(document_id);
        CREATE INDEX IF NOT EXISTS idx_headings_document_id ON headings(document_id);
        CREATE INDEX IF NOT EXISTS idx_block_refs_document_id ON block_refs(document_id);
        CREATE INDEX IF NOT EXISTS idx_links_source_resolved ON links(source_document_id, resolved_target_id);
        ",
    )?;
    Ok(())
}

pub fn apply_schema_v10(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        DROP TABLE IF EXISTS task_properties;
        DROP TABLE IF EXISTS tasks;
        DROP TABLE IF EXISTS inline_expressions;
        DROP TABLE IF EXISTS dataview_blocks;
        DROP TABLE IF EXISTS property_values;

        CREATE TABLE property_values (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            key TEXT NOT NULL,
            value_text TEXT,
            value_number REAL,
            value_bool INTEGER,
            value_date TEXT,
            value_type TEXT NOT NULL,
            origin TEXT NOT NULL DEFAULT 'frontmatter'
        );

        CREATE INDEX idx_property_values_document_key
            ON property_values(document_id, key);
        CREATE INDEX idx_property_values_key ON property_values(key);
        CREATE INDEX idx_property_values_key_origin ON property_values(key, origin);
        CREATE INDEX idx_property_values_key_text
            ON property_values(key, value_text);
        CREATE INDEX idx_property_values_key_number
            ON property_values(key, value_number);
        CREATE INDEX idx_property_values_key_bool
            ON property_values(key, value_bool);
        CREATE INDEX idx_property_values_key_date
            ON property_values(key, value_date);

        CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            status_char TEXT NOT NULL,
            text TEXT NOT NULL,
            byte_offset INTEGER NOT NULL,
            parent_task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
            section_heading TEXT,
            line_number INTEGER NOT NULL
        );

        CREATE INDEX idx_tasks_document_id ON tasks(document_id);
        CREATE INDEX idx_tasks_status_char ON tasks(status_char);

        CREATE TABLE task_properties (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            key TEXT NOT NULL,
            value_text TEXT,
            value_number REAL,
            value_bool INTEGER,
            value_date TEXT,
            value_type TEXT NOT NULL
        );

        CREATE INDEX idx_task_properties_task_id ON task_properties(task_id);
        CREATE INDEX idx_task_properties_key ON task_properties(key);

        CREATE TABLE dataview_blocks (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            language TEXT NOT NULL,
            block_index INTEGER NOT NULL,
            byte_offset_start INTEGER NOT NULL,
            byte_offset_end INTEGER NOT NULL,
            line_number INTEGER NOT NULL,
            raw_text TEXT NOT NULL
        );

        CREATE INDEX idx_dataview_blocks_document_id
            ON dataview_blocks(document_id);

        CREATE TABLE inline_expressions (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            expression TEXT NOT NULL,
            byte_offset_start INTEGER NOT NULL,
            byte_offset_end INTEGER NOT NULL,
            line_number INTEGER NOT NULL
        );

        CREATE INDEX idx_inline_expressions_document_id
            ON inline_expressions(document_id);
        ",
    )?;
    Ok(())
}

pub fn apply_schema_v11(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        DROP TABLE IF EXISTS task_properties;
        DROP TABLE IF EXISTS tasks;
        DROP TABLE IF EXISTS list_items;

        CREATE TABLE list_items (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            text TEXT NOT NULL,
            line_number INTEGER NOT NULL,
            line_count INTEGER NOT NULL,
            byte_offset INTEGER NOT NULL,
            section_heading TEXT,
            parent_item_id TEXT REFERENCES list_items(id) ON DELETE CASCADE,
            is_task INTEGER NOT NULL,
            block_id TEXT,
            annotated INTEGER NOT NULL,
            symbol TEXT NOT NULL
        );

        CREATE INDEX idx_list_items_document_id ON list_items(document_id);
        CREATE INDEX idx_list_items_is_task ON list_items(is_task);
        CREATE INDEX idx_list_items_parent_item_id ON list_items(parent_item_id);

        CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            list_item_id TEXT NOT NULL REFERENCES list_items(id) ON DELETE CASCADE,
            status_char TEXT NOT NULL,
            text TEXT NOT NULL,
            byte_offset INTEGER NOT NULL,
            parent_task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
            section_heading TEXT,
            line_number INTEGER NOT NULL
        );

        CREATE INDEX idx_tasks_document_id ON tasks(document_id);
        CREATE INDEX idx_tasks_status_char ON tasks(status_char);

        CREATE TABLE task_properties (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            key TEXT NOT NULL,
            value_text TEXT,
            value_number REAL,
            value_bool INTEGER,
            value_date TEXT,
            value_type TEXT NOT NULL
        );

        CREATE INDEX idx_task_properties_task_id ON task_properties(task_id);
        CREATE INDEX idx_task_properties_key ON task_properties(key);
        ",
    )?;
    Ok(())
}

pub fn apply_schema_v12(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        ALTER TABLE list_items ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE list_items ADD COLUMN outlinks_json TEXT NOT NULL DEFAULT '[]';
        ",
    )?;
    Ok(())
}

pub fn apply_schema_v13(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tasks_blocks (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            block_index INTEGER NOT NULL,
            byte_offset_start INTEGER NOT NULL,
            byte_offset_end INTEGER NOT NULL,
            line_number INTEGER NOT NULL,
            raw_text TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_tasks_blocks_document_id
            ON tasks_blocks(document_id);
        ",
    )?;
    Ok(())
}

pub fn apply_schema_v14(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS kanban_boards (
            document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            format TEXT NOT NULL,
            settings_json TEXT NOT NULL,
            date_trigger TEXT NOT NULL,
            time_trigger TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_kanban_boards_format
            ON kanban_boards(format);
        ",
    )?;
    Ok(())
}

pub fn apply_schema_v15(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        ALTER TABLE documents ADD COLUMN periodic_type TEXT;
        ALTER TABLE documents ADD COLUMN periodic_date TEXT;

        CREATE INDEX IF NOT EXISTS idx_documents_periodic_type
            ON documents(periodic_type);
        CREATE INDEX IF NOT EXISTS idx_documents_periodic_type_date
            ON documents(periodic_type, periodic_date);

        CREATE TABLE IF NOT EXISTS events (
            id TEXT PRIMARY KEY,
            document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            start_time TEXT NOT NULL,
            end_time TEXT,
            title TEXT NOT NULL,
            metadata_json TEXT NOT NULL,
            tags_json TEXT NOT NULL,
            byte_offset INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_events_document_id
            ON events(document_id);
        CREATE INDEX IF NOT EXISTS idx_events_start_time
            ON events(start_time);
        ",
    )?;
    Ok(())
}

pub fn apply_schema_v16(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tasknotes_tasks (
            document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            status TEXT NOT NULL,
            priority TEXT NOT NULL,
            due TEXT,
            scheduled TEXT,
            completed_date TEXT,
            date_created TEXT,
            date_modified TEXT,
            archived INTEGER NOT NULL DEFAULT 0,
            tags_json TEXT NOT NULL,
            contexts_json TEXT NOT NULL,
            projects_json TEXT NOT NULL,
            time_estimate REAL,
            recurrence TEXT,
            recurrence_anchor TEXT,
            complete_instances_json TEXT NOT NULL,
            skipped_instances_json TEXT NOT NULL,
            blocked_by_json TEXT NOT NULL,
            reminders_json TEXT NOT NULL,
            time_entries_json TEXT NOT NULL,
            custom_fields_json TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_tasknotes_tasks_status
            ON tasknotes_tasks(status);
        CREATE INDEX IF NOT EXISTS idx_tasknotes_tasks_priority
            ON tasknotes_tasks(priority);
        CREATE INDEX IF NOT EXISTS idx_tasknotes_tasks_due
            ON tasknotes_tasks(due);
        CREATE INDEX IF NOT EXISTS idx_tasknotes_tasks_scheduled
            ON tasknotes_tasks(scheduled);
        CREATE INDEX IF NOT EXISTS idx_tasknotes_tasks_archived
            ON tasknotes_tasks(archived);
        ",
    )?;

    Ok(())
}

pub fn apply_schema_v17(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        ALTER TABLE links ADD COLUMN confidence TEXT NOT NULL DEFAULT 'EXTRACTED'
            CHECK (confidence IN ('EXTRACTED', 'INFERRED', 'AMBIGUOUS'));
        ALTER TABLE links ADD COLUMN confidence_score REAL NOT NULL DEFAULT 1.0
            CHECK (confidence_score >= 0.0 AND confidence_score <= 1.0);

        CREATE TABLE IF NOT EXISTS graph_clusters (
            document_id TEXT PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            community_id INTEGER NOT NULL,
            label TEXT NOT NULL,
            cohesion REAL NOT NULL,
            computed_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_graph_clusters_community_id
            ON graph_clusters(community_id);

        CREATE TABLE IF NOT EXISTS link_suggestions (
            id TEXT PRIMARY KEY,
            source_document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            target_document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            score REAL NOT NULL,
            signals TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'accepted', 'rejected')),
            created_at TEXT NOT NULL,
            accepted_at TEXT,
            rejected_at TEXT,
            UNIQUE(source_document_id, target_document_id)
        );

        CREATE INDEX IF NOT EXISTS idx_link_suggestions_status
            ON link_suggestions(status);
        CREATE INDEX IF NOT EXISTS idx_link_suggestions_source
            ON link_suggestions(source_document_id);
        ",
    )?;

    Ok(())
}

pub fn apply_schema_v18(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS mdbase_record_cache (
            collection_root TEXT NOT NULL,
            path TEXT NOT NULL,
            revision TEXT NOT NULL,
            dependency_digest TEXT NOT NULL,
            record_model_version INTEGER NOT NULL,
            types_json TEXT NOT NULL,
            effective_frontmatter_json TEXT NOT NULL,
            display_json TEXT,
            contract_views_json TEXT NOT NULL,
            diagnostics_json TEXT NOT NULL,
            PRIMARY KEY (collection_root, path)
        );

        CREATE INDEX IF NOT EXISTS idx_mdbase_record_cache_dependency
            ON mdbase_record_cache(collection_root, dependency_digest);
        ",
    )?;
    Ok(())
}

pub fn clear_cache_tables(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    transaction.execute("DELETE FROM meta WHERE key = 'property_catalog_config'", [])?;
    // Drop all namespaced vector tables and the legacy table.
    let vector_tables: Vec<String> = {
        let mut statement = transaction.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'vectors_%'",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for table in &vector_tables {
        transaction.execute(
            &format!("DROP TABLE IF EXISTS {}", quote_sqlite_identifier(table)),
            [],
        )?;
    }
    transaction.execute_batch("DROP TABLE IF EXISTS vectors;")?;

    for table_name in TABLES_TO_CLEAR {
        let statement = format!("DELETE FROM {table_name}");
        transaction.execute(&statement, [])?;
    }

    // Keep historical versions, just like legacy checkpoint_documents. Property
    // deletes enqueue dirty IDs, so clear live tracking only after projections.
    // Older migration registries can rebuild before v19 has introduced these.
    for table in [
        "checkpoint_dirty_documents",
        "checkpoint_link_dirty_documents",
        "checkpoint_path_dirty",
        "checkpoint_orphan_dirty_documents",
        "checkpoint_vector_inputs",
    ] {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )?;
        if exists {
            transaction.execute(&format!("DELETE FROM {table}"), [])?;
            // If checkpointing after the rebuild fails, the next changed scan
            // must not reuse hashes whose invalidation inputs were just cleared.
            transaction.execute(
                "INSERT OR IGNORE INTO meta(key, value) VALUES ('checkpoint_reset', '1')",
                [],
            )?;
        }
    }

    Ok(())
}

fn quote_sqlite_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn cache_cleanup_does_not_execute_sql_embedded_in_table_names() {
        let mut connection = Connection::open_in_memory().expect("database should open");
        connection
            .execute_batch(
                "CREATE TABLE sentinel (id INTEGER);
                 CREATE TABLE \"vectors_bad]; DROP TABLE sentinel;--\" (id INTEGER);",
            )
            .expect("hostile fixture should create");
        let transaction = connection.transaction().expect("transaction should start");

        let _ = clear_cache_tables(&transaction);

        let sentinel_exists: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sentinel'",
                [],
                |row| row.get(0),
            )
            .expect("sentinel query should succeed");
        assert_eq!(sentinel_exists, 1);
    }
}
