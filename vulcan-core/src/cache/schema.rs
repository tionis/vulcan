use rusqlite::Transaction;

pub const TABLES_TO_CLEAR: &[&str] = &[
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

pub fn clear_cache_tables(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    // Drop all namespaced vector tables and the legacy table.
    let vector_tables: Vec<String> = {
        let mut statement = transaction.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'vectors_%'",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for table in &vector_tables {
        transaction.execute_batch(&format!("DROP TABLE IF EXISTS [{table}]"))?;
    }
    transaction.execute_batch("DROP TABLE IF EXISTS vectors;")?;

    for table_name in TABLES_TO_CLEAR {
        let statement = format!("DELETE FROM {table_name}");
        transaction.execute(&statement, [])?;
    }

    Ok(())
}
