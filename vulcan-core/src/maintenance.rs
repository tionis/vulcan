use crate::cache::rebuild_search_index;
use crate::scan::{
    discover_relative_paths, scan_vault_unlocked_with_progress, ScanMode, ScanProgress, ScanSummary,
};
use crate::write_lock::acquire_write_lock;
use crate::{doctor_vault, CacheDatabase, CacheError, SearchError, VaultPaths};
use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;

#[derive(Debug)]
pub enum MaintenanceError {
    Cache(CacheError),
    CacheMissing,
    Doctor(String),
    Io(std::io::Error),
    Scan(crate::ScanError),
    Search(SearchError),
    Sqlite(rusqlite::Error),
}

impl Display for MaintenanceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before repairing indexes")
            }
            Self::Doctor(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Scan(error) => write!(formatter, "{error}"),
            Self::Search(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for MaintenanceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Search(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::CacheMissing | Self::Doctor(_) => None,
        }
    }
}

impl From<CacheError> for MaintenanceError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<std::io::Error> for MaintenanceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::DoctorError> for MaintenanceError {
    fn from(error: crate::DoctorError) -> Self {
        Self::Doctor(error.to_string())
    }
}

impl From<crate::ScanError> for MaintenanceError {
    fn from(error: crate::ScanError) -> Self {
        Self::Scan(error)
    }
}

impl From<SearchError> for MaintenanceError {
    fn from(error: SearchError) -> Self {
        Self::Search(error)
    }
}

impl From<rusqlite::Error> for MaintenanceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildQuery {
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RebuildReport {
    pub dry_run: bool,
    pub discovered: usize,
    pub existing_documents: usize,
    pub summary: Option<ScanSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairFtsQuery {
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepairFtsReport {
    pub dry_run: bool,
    pub indexed_documents: usize,
    pub indexed_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheInspectReport {
    pub cache_path: String,
    pub database_bytes: u64,
    pub documents: usize,
    pub notes: usize,
    pub attachments: usize,
    pub bases: usize,
    pub links: usize,
    pub chunks: usize,
    pub diagnostics: usize,
    pub search_rows: usize,
    pub vector_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheVerifyReport {
    pub healthy: bool,
    pub checks: Vec<CacheVerifyCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheVerifyCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheVacuumQuery {
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheVacuumReport {
    pub dry_run: bool,
    pub before_bytes: u64,
    pub after_bytes: Option<u64>,
    pub reclaimed_bytes: Option<u64>,
}

pub fn rebuild_vault(
    paths: &VaultPaths,
    query: &RebuildQuery,
) -> Result<RebuildReport, MaintenanceError> {
    rebuild_vault_with_progress(paths, query, |_| {})
}

pub fn rebuild_vault_with_progress<F>(
    paths: &VaultPaths,
    query: &RebuildQuery,
    mut on_progress: F,
) -> Result<RebuildReport, MaintenanceError>
where
    F: FnMut(ScanProgress),
{
    let discovered = discover_relative_paths(paths.vault_root())?.len();
    let existing_documents = existing_document_count(paths)?;
    if query.dry_run {
        return Ok(RebuildReport {
            dry_run: true,
            discovered,
            existing_documents,
            summary: None,
        });
    }

    let _lock = acquire_write_lock(paths)?;
    let summary = scan_vault_unlocked_with_progress(paths, ScanMode::Full, &mut on_progress)?;
    Ok(RebuildReport {
        dry_run: false,
        discovered,
        existing_documents,
        summary: Some(summary),
    })
}

pub fn repair_fts(
    paths: &VaultPaths,
    query: &RepairFtsQuery,
) -> Result<RepairFtsReport, MaintenanceError> {
    if !paths.cache_db().exists() {
        return Err(MaintenanceError::CacheMissing);
    }

    if query.dry_run {
        let database = CacheDatabase::open(paths)?;
        return Ok(RepairFtsReport {
            dry_run: true,
            indexed_documents: count_distinct_chunk_documents(database.connection())?,
            indexed_chunks: count_chunks(database.connection())?,
        });
    }

    let _lock = acquire_write_lock(paths)?;
    let mut database = CacheDatabase::open(paths)?;
    database.with_transaction(|transaction| {
        rebuild_search_index(transaction).map_err(MaintenanceError::from)?;
        Ok::<_, MaintenanceError>(RepairFtsReport {
            dry_run: false,
            indexed_documents: count_distinct_search_documents_tx(transaction)?,
            indexed_chunks: count_search_rows_tx(transaction)?,
        })
    })
}

pub fn inspect_cache(paths: &VaultPaths) -> Result<CacheInspectReport, MaintenanceError> {
    if !paths.cache_db().exists() {
        return Err(MaintenanceError::CacheMissing);
    }
    let database = CacheDatabase::open(paths)?;
    let connection = database.connection();

    Ok(CacheInspectReport {
        cache_path: paths.cache_db().display().to_string(),
        database_bytes: fs::metadata(paths.cache_db())?.len(),
        documents: count_rows(connection, "documents")?,
        notes: count_where(connection, "documents", "extension = 'md'")?,
        attachments: count_where(connection, "documents", "extension NOT IN ('md', 'base')")?,
        bases: count_where(connection, "documents", "extension = 'base'")?,
        links: count_rows(connection, "links")?,
        chunks: count_rows(connection, "chunks")?,
        diagnostics: count_rows(connection, "diagnostics")?,
        search_rows: count_search_rows(connection)?,
        vector_rows: count_optional_rows(connection, "vectors")?,
    })
}

pub fn verify_cache(paths: &VaultPaths) -> Result<CacheVerifyReport, MaintenanceError> {
    if !paths.cache_db().exists() {
        return Err(MaintenanceError::CacheMissing);
    }
    let database = CacheDatabase::open(paths)?;
    let connection = database.connection();
    let chunks = count_chunks(connection)?;
    let search_rows = count_search_rows(connection)?;
    let vector_rows = count_optional_rows(connection, "vectors")?;
    let doctor = doctor_vault(paths)?;
    let doctor_clean = doctor.summary.stale_index_rows == 0
        && doctor.summary.missing_index_rows == 0
        && doctor.summary.parse_failures == 0;

    let checks = vec![
        CacheVerifyCheck {
            name: "search_rows_match_chunks".to_string(),
            ok: chunks == search_rows,
            detail: format!("chunks={chunks}, search_rows={search_rows}"),
        },
        CacheVerifyCheck {
            name: "vector_rows_do_not_exceed_chunks".to_string(),
            ok: vector_rows <= chunks,
            detail: format!("vector_rows={vector_rows}, chunks={chunks}"),
        },
        CacheVerifyCheck {
            name: "doctor_reconciliation_clean".to_string(),
            ok: doctor_clean,
            detail: format!(
                "stale={}, missing={}, parse_failures={}",
                doctor.summary.stale_index_rows,
                doctor.summary.missing_index_rows,
                doctor.summary.parse_failures
            ),
        },
    ];

    Ok(CacheVerifyReport {
        healthy: checks.iter().all(|check| check.ok),
        checks,
    })
}

pub fn cache_vacuum(
    paths: &VaultPaths,
    query: &CacheVacuumQuery,
) -> Result<CacheVacuumReport, MaintenanceError> {
    if !paths.cache_db().exists() {
        return Err(MaintenanceError::CacheMissing);
    }
    let before_bytes = fs::metadata(paths.cache_db())?.len();
    if query.dry_run {
        return Ok(CacheVacuumReport {
            dry_run: true,
            before_bytes,
            after_bytes: None,
            reclaimed_bytes: None,
        });
    }

    let _lock = acquire_write_lock(paths)?;
    let database = CacheDatabase::open(paths)?;
    database.connection().execute_batch("VACUUM")?;
    let after_bytes = fs::metadata(paths.cache_db())?.len();

    Ok(CacheVacuumReport {
        dry_run: false,
        before_bytes,
        after_bytes: Some(after_bytes),
        reclaimed_bytes: Some(before_bytes.saturating_sub(after_bytes)),
    })
}

fn existing_document_count(paths: &VaultPaths) -> Result<usize, MaintenanceError> {
    if !paths.cache_db().exists() {
        return Ok(0);
    }

    let database = CacheDatabase::open(paths)?;
    let count: i64 =
        database
            .connection()
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn count_chunks(connection: &rusqlite::Connection) -> Result<usize, MaintenanceError> {
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn count_rows(connection: &rusqlite::Connection, table: &str) -> Result<usize, MaintenanceError> {
    let count: i64 = connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn count_optional_rows(
    connection: &rusqlite::Connection,
    table: &str,
) -> Result<usize, MaintenanceError> {
    if !table_exists(connection, table)? {
        return Ok(0);
    }

    count_rows(connection, table)
}

fn count_where(
    connection: &rusqlite::Connection,
    table: &str,
    predicate: &str,
) -> Result<usize, MaintenanceError> {
    let count: i64 = connection.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {predicate}"),
        [],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn count_search_rows(connection: &rusqlite::Connection) -> Result<usize, MaintenanceError> {
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM search_chunk_content", [], |row| {
            row.get(0)
        })?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn table_exists(connection: &rusqlite::Connection, table: &str) -> Result<bool, MaintenanceError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn count_distinct_chunk_documents(
    connection: &rusqlite::Connection,
) -> Result<usize, MaintenanceError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(DISTINCT document_id) FROM chunks",
        [],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn count_distinct_search_documents_tx(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<usize, MaintenanceError> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(DISTINCT document_id) FROM search_chunk_content",
        [],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

fn count_search_rows_tx(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<usize, MaintenanceError> {
    let count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM search_chunk_content", [], |row| {
            row.get(0)
        })?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        initialize_vulcan_dir, scan_vault, search_vault, CacheDatabase, ScanMode, SearchQuery,
    };
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;
    use ulid::Ulid;

    #[test]
    fn rebuild_vault_dry_run_reports_scope_without_mutating_cache() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let mut database = CacheDatabase::open(&paths).expect("database should open");
        insert_stale_document(&mut database);

        let report = rebuild_vault(&paths, &RebuildQuery { dry_run: true })
            .expect("dry-run rebuild should succeed");

        assert!(report.dry_run);
        assert_eq!(report.discovered, 3);
        assert_eq!(report.existing_documents, 4);
        assert_eq!(report.summary, None);
        assert_eq!(document_count(&paths), 4);
    }

    #[test]
    fn rebuild_vault_resets_cache_and_rescans_from_disk() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let mut database = CacheDatabase::open(&paths).expect("database should open");
        insert_stale_document(&mut database);

        let report =
            rebuild_vault(&paths, &RebuildQuery { dry_run: false }).expect("rebuild should work");
        let summary = report.summary.expect("rebuild should include summary");

        assert!(!report.dry_run);
        assert_eq!(summary.mode, ScanMode::Full);
        assert_eq!(summary.discovered, 3);
        assert_eq!(summary.added, 3);
        assert_eq!(summary.deleted, 1);
        assert_eq!(document_count(&paths), 3);
    }

    #[test]
    fn repair_fts_rebuilds_missing_search_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let mut database = CacheDatabase::open(&paths).expect("database should open");
        database
            .with_transaction(|transaction| {
                transaction
                    .execute("DELETE FROM search_chunk_content", [])
                    .expect("search rows should delete");
                Ok::<_, MaintenanceError>(())
            })
            .expect("corruption setup should succeed");

        let missing_report = search_vault(
            &paths,
            &SearchQuery {
                text: "dashboard".to_string(),
                ..SearchQuery::default()
            },
        )
        .expect("search should not error");
        assert!(missing_report.hits.is_empty());

        let report = repair_fts(&paths, &RepairFtsQuery { dry_run: false })
            .expect("fts repair should succeed");

        assert_eq!(report.indexed_documents, 3);
        assert_eq!(report.indexed_chunks, 4);
        let repaired_report = search_vault(
            &paths,
            &SearchQuery {
                text: "dashboard".to_string(),
                ..SearchQuery::default()
            },
        )
        .expect("search should succeed");
        assert!(!repaired_report.hits.is_empty());
    }

    #[test]
    fn repair_fts_dry_run_does_not_mutate_search_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let mut database = CacheDatabase::open(&paths).expect("database should open");
        database
            .with_transaction(|transaction| {
                transaction
                    .execute("DELETE FROM search_chunk_content", [])
                    .expect("search rows should delete");
                Ok::<_, MaintenanceError>(())
            })
            .expect("corruption setup should succeed");

        let report =
            repair_fts(&paths, &RepairFtsQuery { dry_run: true }).expect("dry run should work");

        assert!(report.dry_run);
        assert_eq!(report.indexed_documents, 3);
        assert_eq!(report.indexed_chunks, 4);
        let remaining_rows: i64 = CacheDatabase::open(&paths)
            .expect("database should open")
            .connection()
            .query_row("SELECT COUNT(*) FROM search_chunk_content", [], |row| {
                row.get(0)
            })
            .expect("row count should be readable");
        assert_eq!(remaining_rows, 0);
    }

    #[test]
    fn inspect_cache_reports_document_and_index_counts() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = inspect_cache(&paths).expect("inspect should succeed");

        assert_eq!(report.documents, 3);
        assert_eq!(report.notes, 3);
        assert_eq!(report.attachments, 0);
        assert_eq!(report.bases, 0);
        assert_eq!(report.links, 5);
        assert_eq!(report.chunks, 4);
        assert_eq!(report.search_rows, 4);
    }

    #[test]
    fn verify_cache_detects_search_mismatch() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        CacheDatabase::open(&paths)
            .expect("database should open")
            .with_transaction(|transaction| {
                transaction
                    .execute("DELETE FROM search_chunk_content", [])
                    .expect("search rows should delete");
                Ok::<_, MaintenanceError>(())
            })
            .expect("corruption setup should succeed");

        let report = verify_cache(&paths).expect("verify should succeed");

        assert!(!report.healthy);
        assert!(report
            .checks
            .iter()
            .any(|check| check.name == "search_rows_match_chunks" && !check.ok));
    }

    #[test]
    fn vacuum_cache_reports_sizes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vault should initialize");

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let dry_run = cache_vacuum(&paths, &CacheVacuumQuery { dry_run: true })
            .expect("dry-run vacuum should succeed");
        assert!(dry_run.dry_run);
        assert!(dry_run.after_bytes.is_none());

        let applied = cache_vacuum(&paths, &CacheVacuumQuery { dry_run: false })
            .expect("vacuum should succeed");
        assert!(!applied.dry_run);
        assert!(applied.after_bytes.is_some());
    }

    fn copy_fixture_vault(fixture_name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(fixture_name);
        copy_dir_recursive(&source, destination);
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination should be created");
        for entry in fs::read_dir(source).expect("fixture dir should be readable") {
            let entry = entry.expect("dir entry should be readable");
            let entry_path = entry.path();
            let dest_path = destination.join(entry.file_name());
            if entry.file_type().expect("file type should load").is_dir() {
                copy_dir_recursive(&entry_path, &dest_path);
            } else {
                fs::copy(&entry_path, &dest_path).expect("fixture file should copy");
            }
        }
    }

    fn insert_stale_document(database: &mut CacheDatabase) {
        database
            .with_transaction(|transaction| {
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
                    VALUES (?1, 'Stale.md', 'Stale', 'md', ?2, NULL, 1, 1, ?3, '2026-03-20T00:00:00Z')
                    ",
                    rusqlite::params![
                        Ulid::new().to_string(),
                        vec![1_u8; 32],
                        crate::PARSER_VERSION
                    ],
                )?;
                Ok::<_, MaintenanceError>(())
            })
            .expect("stale document should insert");
    }

    fn document_count(paths: &VaultPaths) -> i64 {
        CacheDatabase::open(paths)
            .expect("database should open")
            .connection()
            .query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))
            .expect("document count should be readable")
    }
}
