use rusqlite::Transaction;

pub const TABLES_TO_CLEAR: &[&str] = &[
    "headings",
    "block_refs",
    "links",
    "aliases",
    "tags",
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

pub fn clear_cache_tables(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    for table_name in TABLES_TO_CLEAR {
        let statement = format!("DELETE FROM {table_name}");
        transaction.execute(&statement, [])?;
    }

    Ok(())
}
