use crate::cache::error::CacheError;
use crate::cache::schema;
use rusqlite::{Connection, Transaction};

pub type MigrationFn = fn(&Transaction<'_>) -> Result<(), rusqlite::Error>;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    apply: MigrationFn,
}

impl Migration {
    #[must_use]
    pub const fn new(version: u32, name: &'static str, apply: MigrationFn) -> Self {
        Self {
            version,
            name,
            apply,
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
        for migration in pending {
            migration.run(&transaction)?;
            transaction.pragma_update(None, "user_version", migration.version)?;
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
                FROM chunk_search_content
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
                FROM chunk_search
                JOIN chunk_search_content ON chunk_search.rowid = chunk_search_content.id
                JOIN documents ON documents.id = chunk_search_content.document_id
                WHERE chunk_search MATCH ?1
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
}
