use crate::cache::error::CacheError;
use crate::cache::schema;
use rusqlite::{Connection, Transaction};

pub type MigrationFn = fn(&Transaction<'_>) -> Result<(), rusqlite::Error>;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    apply: MigrationFn,
    rebuild_cache: bool,
}

impl Migration {
    #[must_use]
    pub const fn new(version: u32, name: &'static str, apply: MigrationFn) -> Self {
        Self {
            version,
            name,
            apply,
            rebuild_cache: false,
        }
    }

    #[must_use]
    pub const fn breaking(version: u32, name: &'static str, apply: MigrationFn) -> Self {
        Self {
            version,
            name,
            apply,
            rebuild_cache: true,
        }
    }

    fn run(&self, transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
        (self.apply)(transaction)
    }
}

#[derive(Debug, Clone)]
pub struct MigrationRegistry {
    migrations: Vec<Migration>,
}

impl MigrationRegistry {
    #[must_use]
    pub fn new(migrations: Vec<Migration>) -> Self {
        Self { migrations }
    }

    #[must_use]
    pub fn schema_v1() -> Self {
        Self::new(vec![
            Migration::new(1, "create cache schema v1", schema::apply_schema_v1),
            Migration::new(2, "add chunk content column", schema::apply_schema_v2),
            Migration::new(3, "add chunk search index", schema::apply_schema_v3),
            Migration::new(
                4,
                "repair chunk search schema naming",
                schema::apply_schema_v4,
            ),
            Migration::new(5, "add property storage tables", schema::apply_schema_v5),
            Migration::new(6, "add vector cache state tables", schema::apply_schema_v6),
            Migration::new(7, "add checkpoint history tables", schema::apply_schema_v7),
        ])
    }

    #[must_use]
    pub fn target_version(&self) -> u32 {
        self.migrations
            .last()
            .map_or(0, |migration| migration.version)
    }

    pub fn migrate(&self, connection: &mut Connection) -> Result<(), CacheError> {
        let current_version = current_user_version(connection)?;
        let target_version = self.target_version();

        if current_version > target_version {
            return Err(CacheError::Downgrade {
                database_version: current_version,
                application_version: target_version,
            });
        }

        let pending = self
            .migrations
            .iter()
            .copied()
            .filter(|migration| migration.version > current_version)
            .collect::<Vec<_>>();

        if pending.is_empty() {
            return Ok(());
        }

        let transaction = connection.transaction()?;
        let mut rebuild_required = false;
        for migration in pending {
            migration.run(&transaction)?;
            transaction.pragma_update(None, "user_version", migration.version)?;
            rebuild_required |= migration.rebuild_cache;
        }
        if rebuild_required {
            schema::clear_cache_tables(&transaction)?;
        }
        transaction.commit()?;

        Ok(())
    }
}

fn current_user_version(connection: &Connection) -> Result<u32, rusqlite::Error> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SCHEMA_VERSION;
    use rusqlite::Connection;

    fn create_first_table(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
        transaction.execute("CREATE TABLE first_table (id INTEGER PRIMARY KEY)", [])?;
        Ok(())
    }

    fn create_second_table(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
        transaction.execute("CREATE TABLE second_table (id INTEGER PRIMARY KEY)", [])?;
        Ok(())
    }

    fn add_name_column(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
        transaction.execute(
            "ALTER TABLE first_table ADD COLUMN name TEXT NOT NULL DEFAULT 'unknown'",
            [],
        )?;
        Ok(())
    }

    fn noop_breaking_migration(transaction: &Transaction<'_>) -> Result<(), rusqlite::Error> {
        transaction.execute_batch("")?;
        Ok(())
    }

    #[test]
    fn applies_pending_migrations_in_order() {
        let mut connection = Connection::open_in_memory().expect("in-memory db should open");
        let registry = MigrationRegistry::new(vec![
            Migration::new(1, "first", create_first_table),
            Migration::new(2, "second", create_second_table),
        ]);

        registry
            .migrate(&mut connection)
            .expect("migrations should apply");

        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version should be readable");

        assert_eq!(version, 2);
        assert!(table_exists(&connection, "first_table"));
        assert!(table_exists(&connection, "second_table"));
    }

    #[test]
    fn skips_migrations_that_are_already_applied() {
        let mut connection = Connection::open_in_memory().expect("in-memory db should open");
        let registry = MigrationRegistry::new(vec![Migration::new(1, "first", create_first_table)]);

        registry
            .migrate(&mut connection)
            .expect("initial migration should apply");
        connection
            .execute("INSERT INTO first_table (id) VALUES (1)", [])
            .expect("test row should insert");

        registry
            .migrate(&mut connection)
            .expect("reapplying migration registry should be a no-op");

        let row_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM first_table", [], |row| row.get(0))
            .expect("row count should be readable");

        assert_eq!(row_count, 1);
    }

    #[test]
    fn refuses_database_downgrades() {
        let mut connection = Connection::open_in_memory().expect("in-memory db should open");
        connection
            .pragma_update(None, "user_version", 3)
            .expect("user_version should update");
        let registry = MigrationRegistry::new(vec![
            Migration::new(1, "first", create_first_table),
            Migration::new(2, "second", create_second_table),
        ]);

        let error = registry
            .migrate(&mut connection)
            .expect_err("downgrade should be rejected");

        match error {
            CacheError::Downgrade {
                database_version,
                application_version,
            } => {
                assert_eq!(database_version, 3);
                assert_eq!(application_version, 2);
            }
            other => panic!("expected downgrade error, got {other:?}"),
        }
    }

    #[test]
    fn schema_registry_targets_current_schema_version() {
        assert_eq!(
            MigrationRegistry::schema_v1().target_version(),
            SCHEMA_VERSION
        );
    }

    #[test]
    fn add_chunk_search_migration_backfills_existing_chunks() {
        let mut connection = Connection::open_in_memory().expect("in-memory db should open");
        let transaction = connection
            .transaction()
            .expect("setup transaction should begin");
        schema::apply_schema_v1(&transaction).expect("schema v1 should apply");
        schema::apply_schema_v2(&transaction).expect("schema v2 should apply");
        transaction
            .pragma_update(None, "user_version", 2)
            .expect("user_version should update");
        transaction
            .commit()
            .expect("setup transaction should commit");

        connection
            .execute(
                "
                INSERT INTO documents (
                    id, path, filename, extension, content_hash, raw_frontmatter,
                    file_size, file_mtime, parser_version, indexed_at
                )
                VALUES ('doc-1', 'Home.md', 'Home', 'md', ?1, NULL, 1, 1, 1, '1')
                ",
                [vec![1_u8; 32]],
            )
            .expect("document should insert");
        connection
            .execute(
                "
                INSERT INTO aliases (id, document_id, alias_text)
                VALUES ('alias-1', 'doc-1', 'Start')
                ",
                [],
            )
            .expect("alias should insert");
        connection
            .execute(
                "
                INSERT INTO chunks (
                    id, document_id, sequence_index, heading_path, byte_offset_start,
                    byte_offset_end, content_hash, chunk_strategy, chunk_version, content
                )
                VALUES ('chunk-1', 'doc-1', 0, '[\"Home\"]', 0, 4, ?1, 'heading', 1, 'dashboard note')
                ",
                [vec![2_u8; 32]],
            )
            .expect("chunk should insert");

        MigrationRegistry::schema_v1()
            .migrate(&mut connection)
            .expect("migration to v3 should succeed");

        let search_row: (String, String, String, String) = connection
            .query_row(
                "
                SELECT content, document_title, aliases, headings
                FROM search_chunk_content
                WHERE chunk_id = 'chunk-1'
                ",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("search content row should exist");
        assert_eq!(
            search_row,
            (
                "dashboard note".to_string(),
                "Home".to_string(),
                "Start".to_string(),
                "Home".to_string()
            )
        );

        let matched_paths = search_matches(&connection, "Start");
        assert_eq!(matched_paths, vec!["Home.md".to_string()]);
    }

    #[test]
    fn additive_migration_preserves_existing_rows() {
        let mut connection = Connection::open_in_memory().expect("in-memory db should open");
        let initial_registry =
            MigrationRegistry::new(vec![Migration::new(1, "first", create_first_table)]);
        initial_registry
            .migrate(&mut connection)
            .expect("initial migration should apply");
        connection
            .execute("INSERT INTO first_table (id) VALUES (7)", [])
            .expect("test row should insert");

        let upgraded_registry = MigrationRegistry::new(vec![
            Migration::new(1, "first", create_first_table),
            Migration::new(2, "add name", add_name_column),
        ]);
        upgraded_registry
            .migrate(&mut connection)
            .expect("additive migration should apply");

        let row: (i64, String) = connection
            .query_row("SELECT id, name FROM first_table", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("row should remain readable");
        assert_eq!(row, (7, "unknown".to_string()));
    }

    #[test]
    fn breaking_migration_clears_rebuildable_cache_rows() {
        let mut connection = Connection::open_in_memory().expect("in-memory db should open");
        let registry = MigrationRegistry::schema_v1();
        registry
            .migrate(&mut connection)
            .expect("schema should apply");
        connection
            .execute(
                "
                INSERT INTO documents (
                    id, path, filename, extension, content_hash, raw_frontmatter,
                    file_size, file_mtime, parser_version, indexed_at
                )
                VALUES ('doc-1', 'Home.md', 'Home', 'md', ?1, NULL, 1, 1, 1, '1')
                ",
                [vec![1_u8; 32]],
            )
            .expect("document should insert");
        connection
            .execute(
                "
                INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
                VALUES ('diag-1', 'doc-1', 'parse_error', 'bad yaml', '{}', '1')
                ",
                [],
            )
            .expect("diagnostic should insert");

        let mut upgraded_connection = connection;
        let mut breaking_migrations = MigrationRegistry::schema_v1().migrations;
        breaking_migrations.push(Migration::breaking(
            SCHEMA_VERSION + 1,
            "force rebuild",
            noop_breaking_migration,
        ));
        MigrationRegistry::new(breaking_migrations)
            .migrate(&mut upgraded_connection)
            .expect("breaking migration should apply");

        let version: u32 = upgraded_connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("user_version should be readable");
        assert_eq!(version, SCHEMA_VERSION + 1);
        assert_eq!(
            upgraded_connection
                .query_row("SELECT COUNT(*) FROM documents", [], |row| row
                    .get::<_, i64>(0))
                .expect("document count should be readable"),
            0
        );
        assert_eq!(
            upgraded_connection
                .query_row("SELECT COUNT(*) FROM diagnostics", [], |row| row
                    .get::<_, i64>(0))
                .expect("diagnostic count should be readable"),
            0
        );
    }

    fn table_exists(connection: &Connection, name: &str) -> bool {
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .expect("sqlite_master query should succeed")
            > 0
    }

    fn search_matches(connection: &Connection, query: &str) -> Vec<String> {
        let mut statement = connection
            .prepare(
                "
                SELECT DISTINCT documents.path
                FROM search_chunks_fts
                JOIN search_chunk_content ON search_chunks_fts.rowid = search_chunk_content.id
                JOIN documents ON documents.id = search_chunk_content.document_id
                WHERE search_chunks_fts MATCH ?1
                ORDER BY documents.path
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([query], |row| row.get(0))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    #[test]
    fn repair_chunk_search_schema_recovers_shadow_table_conflict() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir should be created");
        let database_path = temp_dir.path().join("cache.db");
        let mut connection = Connection::open(&database_path).expect("database should open");
        let transaction = connection
            .transaction()
            .expect("setup transaction should begin");
        schema::apply_schema_v1(&transaction).expect("schema v1 should apply");
        schema::apply_schema_v2(&transaction).expect("schema v2 should apply");
        install_broken_v3_search_schema(&transaction).expect("broken v3 schema should apply");
        transaction
            .pragma_update(None, "user_version", 3)
            .expect("user_version should update");
        transaction
            .execute(
                "
                INSERT INTO documents (
                    id, path, filename, extension, content_hash, raw_frontmatter,
                    file_size, file_mtime, parser_version, indexed_at
                )
                VALUES ('doc-1', 'Home.md', 'Home', 'md', ?1, NULL, 1, 1, 1, '1')
                ",
                [vec![1_u8; 32]],
            )
            .expect("document should insert");
        transaction
            .execute(
                "
                INSERT INTO chunks (
                    id, document_id, sequence_index, heading_path, byte_offset_start,
                    byte_offset_end, content_hash, chunk_strategy, chunk_version, content
                )
                VALUES ('chunk-1', 'doc-1', 0, '[\"Home\"]', 0, 4, ?1, 'heading', 1, 'dashboard note')
                ",
                [vec![2_u8; 32]],
            )
            .expect("chunk should insert");
        transaction
            .commit()
            .expect("setup transaction should commit");
        drop(connection);

        let mut reopened = Connection::open(&database_path).expect("database should reopen");
        MigrationRegistry::schema_v1()
            .migrate(&mut reopened)
            .expect("migration to v4 should repair the schema");

        let matched_paths = search_matches(&reopened, "dashboard");
        assert_eq!(matched_paths, vec!["Home.md".to_string()]);
    }

    fn install_broken_v3_search_schema(
        transaction: &Transaction<'_>,
    ) -> Result<(), rusqlite::Error> {
        transaction.execute_batch(
            "
            CREATE TABLE chunk_search_content (
                id INTEGER PRIMARY KEY,
                chunk_id TEXT NOT NULL UNIQUE REFERENCES chunks(id) ON DELETE CASCADE,
                document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                content TEXT NOT NULL,
                document_title TEXT NOT NULL,
                aliases TEXT NOT NULL,
                headings TEXT NOT NULL
            );

            CREATE INDEX idx_chunk_search_content_document_id
                ON chunk_search_content(document_id);

            CREATE VIRTUAL TABLE chunk_search USING fts5(
                content,
                document_title,
                aliases,
                headings,
                content = 'chunk_search_content',
                content_rowid = 'id',
                tokenize = 'unicode61'
            );

            CREATE TRIGGER chunk_search_content_ai AFTER INSERT ON chunk_search_content BEGIN
                INSERT INTO chunk_search(rowid, content, document_title, aliases, headings)
                VALUES (new.id, new.content, new.document_title, new.aliases, new.headings);
            END;

            CREATE TRIGGER chunk_search_content_ad AFTER DELETE ON chunk_search_content BEGIN
                INSERT INTO chunk_search(chunk_search, rowid, content, document_title, aliases, headings)
                VALUES ('delete', old.id, old.content, old.document_title, old.aliases, old.headings);
            END;

            CREATE TRIGGER chunk_search_content_au AFTER UPDATE ON chunk_search_content BEGIN
                INSERT INTO chunk_search(chunk_search, rowid, content, document_title, aliases, headings)
                VALUES ('delete', old.id, old.content, old.document_title, old.aliases, old.headings);
                INSERT INTO chunk_search(rowid, content, document_title, aliases, headings)
                VALUES (new.id, new.content, new.document_title, new.aliases, new.headings);
            END;
            ",
        )?;
        Ok(())
    }
}
