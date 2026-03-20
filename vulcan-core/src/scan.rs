use crate::cache::CacheError;
use crate::parser::{parse_document, LinkKind, OriginContext, ParseDiagnosticKind, ParsedDocument};
use crate::properties::{extract_indexed_properties, rebuild_property_catalog, IndexedProperties};
use crate::resolver::{resolve_link, LinkResolutionProblem, ResolverDocument, ResolverLink};
use crate::write_lock::acquire_write_lock;
use crate::{load_vault_config, CacheDatabase, VaultPaths, PARSER_VERSION};
use ignore::WalkBuilder;
use rayon::prelude::*;
use rusqlite::{params, Connection, Transaction};
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};
use ulid::Ulid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Note,
    Base,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanSummary {
    pub mode: ScanMode,
    pub discovered: usize,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    PreparingFiles,
    ScanningFiles,
    RefreshingPropertyCatalog,
    ResolvingLinks,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanProgress {
    pub mode: ScanMode,
    pub phase: ScanPhase,
    pub discovered: usize,
    pub processed: usize,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub deleted: usize,
}

#[derive(Debug)]
pub enum ScanError {
    Cache(CacheError),
    Ignore(ignore::Error),
    Io(std::io::Error),
    MetadataOverflow { field: &'static str, path: PathBuf },
    Sqlite(rusqlite::Error),
    Time(SystemTimeError),
}

impl Display for ScanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Ignore(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::MetadataOverflow { field, path } => {
                write!(formatter, "{field} overflowed for {}", path.display())
            }
            Self::Sqlite(error) => write!(formatter, "{error}"),
            Self::Time(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for ScanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Ignore(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::MetadataOverflow { .. } => None,
            Self::Sqlite(error) => Some(error),
            Self::Time(error) => Some(error),
        }
    }
}

impl From<CacheError> for ScanError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<ignore::Error> for ScanError {
    fn from(error: ignore::Error) -> Self {
        Self::Ignore(error)
    }
}

impl From<std::io::Error> for ScanError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for ScanError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<SystemTimeError> for ScanError {
    fn from(error: SystemTimeError) -> Self {
        Self::Time(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredFile {
    absolute_path: PathBuf,
    relative_path: String,
    filename: String,
    extension: String,
    kind: DocumentKind,
    file_size: i64,
    file_mtime: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedDocument {
    id: String,
    file_size: i64,
    file_mtime: i64,
    content_hash: Vec<u8>,
    parser_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IncrementalScanResult {
    summary: ScanSummary,
    requires_link_resolution: bool,
    requires_property_catalog_refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChunkReuseKey {
    heading_path: Vec<String>,
    content_hash: Vec<u8>,
    chunk_strategy: String,
    chunk_version: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct PreparedFullScanDocument {
    file: DiscoveredFile,
    content_hash: Vec<u8>,
    parsed: Option<ParsedDocument>,
}

#[must_use]
pub fn detect_document_kind(path: &Path) -> DocumentKind {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md") => DocumentKind::Note,
        Some("base") => DocumentKind::Base,
        _ => DocumentKind::Attachment,
    }
}

pub fn scan_vault(paths: &VaultPaths, mode: ScanMode) -> Result<ScanSummary, ScanError> {
    scan_vault_with_progress(paths, mode, |_| {})
}

pub fn scan_vault_with_progress<F>(
    paths: &VaultPaths,
    mode: ScanMode,
    mut on_progress: F,
) -> Result<ScanSummary, ScanError>
where
    F: FnMut(ScanProgress),
{
    let _lock = acquire_write_lock(paths)?;
    scan_vault_unlocked_with_progress(paths, mode, &mut on_progress)
}

pub(crate) fn scan_vault_unlocked(
    paths: &VaultPaths,
    mode: ScanMode,
) -> Result<ScanSummary, ScanError> {
    let mut noop = |_| {};
    scan_vault_unlocked_with_progress(paths, mode, &mut noop)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn scan_vault_unlocked_with_progress<F>(
    paths: &VaultPaths,
    mode: ScanMode,
    on_progress: &mut F,
) -> Result<ScanSummary, ScanError>
where
    F: FnMut(ScanProgress),
{
    let config = load_vault_config(paths).config;
    let discovered = discover_files(paths.vault_root())?;
    emit_scan_progress(
        on_progress,
        ScanProgress {
            mode,
            phase: ScanPhase::PreparingFiles,
            discovered: discovered.len(),
            processed: 0,
            added: 0,
            updated: 0,
            unchanged: 0,
            deleted: 0,
        },
    );
    let current_paths = discovered
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<HashSet<_>>();
    let mut database = CacheDatabase::open(paths)?;
    let existing = load_cached_documents(database.connection())?;
    let deleted_paths = existing
        .keys()
        .filter(|path| !current_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();

    match mode {
        ScanMode::Full => {
            let discovered_count = discovered.len();
            let deleted_count = deleted_paths.len();
            let prepared = prepare_full_scan_documents(&discovered, &config)?;
            database.rebuild_with(|transaction| -> Result<ScanSummary, ScanError> {
                emit_scan_progress(
                    on_progress,
                    ScanProgress {
                        mode,
                        phase: ScanPhase::ScanningFiles,
                        discovered: discovered_count,
                        processed: 0,
                        added: 0,
                        updated: 0,
                        unchanged: 0,
                        deleted: deleted_count,
                    },
                );
                for (index, prepared_document) in prepared.iter().enumerate() {
                    let id = Ulid::new().to_string();
                    apply_prepared_full_scan_document(
                        transaction,
                        &config,
                        &id,
                        prepared_document,
                    )?;
                    emit_scan_progress(
                        on_progress,
                        ScanProgress {
                            mode,
                            phase: ScanPhase::ScanningFiles,
                            discovered: discovered_count,
                            processed: index + 1,
                            added: index + 1,
                            updated: 0,
                            unchanged: 0,
                            deleted: deleted_count,
                        },
                    );
                }
                emit_scan_progress(
                    on_progress,
                    ScanProgress {
                        mode,
                        phase: ScanPhase::RefreshingPropertyCatalog,
                        discovered: discovered_count,
                        processed: discovered_count,
                        added: discovered_count,
                        updated: 0,
                        unchanged: 0,
                        deleted: deleted_count,
                    },
                );
                rebuild_property_catalog(transaction, &config.property_types)?;
                emit_scan_progress(
                    on_progress,
                    ScanProgress {
                        mode,
                        phase: ScanPhase::ResolvingLinks,
                        discovered: discovered_count,
                        processed: discovered_count,
                        added: discovered_count,
                        updated: 0,
                        unchanged: 0,
                        deleted: deleted_count,
                    },
                );
                resolve_all_links(transaction, config.link_resolution)?;

                let summary = ScanSummary {
                    mode,
                    discovered: discovered_count,
                    added: discovered_count,
                    updated: 0,
                    unchanged: 0,
                    deleted: deleted_count,
                };
                emit_scan_progress(
                    on_progress,
                    ScanProgress {
                        mode,
                        phase: ScanPhase::Completed,
                        discovered: summary.discovered,
                        processed: summary.discovered,
                        added: summary.added,
                        updated: summary.updated,
                        unchanged: summary.unchanged,
                        deleted: summary.deleted,
                    },
                );
                Ok(summary)
            })
        }
        ScanMode::Incremental => {
            database.with_transaction(|transaction| -> Result<ScanSummary, ScanError> {
                let result = apply_incremental_scan(
                    transaction,
                    &config,
                    &discovered,
                    &existing,
                    &deleted_paths,
                    mode,
                    on_progress,
                )?;
                if result.requires_property_catalog_refresh {
                    emit_scan_progress(
                        on_progress,
                        ScanProgress {
                            mode,
                            phase: ScanPhase::RefreshingPropertyCatalog,
                            discovered: result.summary.discovered,
                            processed: result.summary.discovered,
                            added: result.summary.added,
                            updated: result.summary.updated,
                            unchanged: result.summary.unchanged,
                            deleted: result.summary.deleted,
                        },
                    );
                    rebuild_property_catalog(transaction, &config.property_types)?;
                }
                if result.requires_link_resolution {
                    emit_scan_progress(
                        on_progress,
                        ScanProgress {
                            mode,
                            phase: ScanPhase::ResolvingLinks,
                            discovered: result.summary.discovered,
                            processed: result.summary.discovered,
                            added: result.summary.added,
                            updated: result.summary.updated,
                            unchanged: result.summary.unchanged,
                            deleted: result.summary.deleted,
                        },
                    );
                    resolve_all_links(transaction, config.link_resolution)?;
                }
                emit_scan_progress(
                    on_progress,
                    ScanProgress {
                        mode,
                        phase: ScanPhase::Completed,
                        discovered: result.summary.discovered,
                        processed: result.summary.discovered,
                        added: result.summary.added,
                        updated: result.summary.updated,
                        unchanged: result.summary.unchanged,
                        deleted: result.summary.deleted,
                    },
                );
                Ok(result.summary)
            })
        }
    }
}

#[allow(clippy::too_many_lines)]
fn apply_incremental_scan(
    transaction: &Transaction<'_>,
    config: &crate::VaultConfig,
    discovered: &[DiscoveredFile],
    existing: &HashMap<String, CachedDocument>,
    deleted_paths: &[String],
    mode: ScanMode,
    on_progress: &mut impl FnMut(ScanProgress),
) -> Result<IncrementalScanResult, ScanError> {
    let mut result = IncrementalScanResult {
        summary: ScanSummary {
            mode,
            discovered: discovered.len(),
            added: 0,
            updated: 0,
            unchanged: 0,
            deleted: 0,
        },
        // Link resolution is only needed when the set of resolvable targets or
        // the extracted links/aliases of a note changed.
        requires_link_resolution: false,
        requires_property_catalog_refresh: false,
    };
    emit_scan_progress(
        on_progress,
        ScanProgress {
            mode,
            phase: ScanPhase::ScanningFiles,
            discovered: discovered.len(),
            processed: 0,
            added: 0,
            updated: 0,
            unchanged: 0,
            deleted: 0,
        },
    );
    let mut seen_paths = HashSet::with_capacity(discovered.len());

    for (index, file) in discovered.iter().enumerate() {
        seen_paths.insert(file.relative_path.clone());

        match existing.get(&file.relative_path) {
            Some(cached)
                if cached.file_size == file.file_size
                    && cached.file_mtime == file.file_mtime
                    && cached.parser_version == PARSER_VERSION =>
            {
                result.summary.unchanged += 1;
            }
            Some(cached) => {
                let hash =
                    if cached.file_size == file.file_size && cached.file_mtime == file.file_mtime {
                        cached.content_hash.clone()
                    } else {
                        compute_content_hash(&file.absolute_path)?
                    };
                let requires_reindex = matches!(file.kind, DocumentKind::Note)
                    && (hash != cached.content_hash || cached.parser_version != PARSER_VERSION);

                if hash == cached.content_hash && !requires_reindex {
                    update_document_metadata(transaction, &cached.id, file)?;
                    result.summary.unchanged += 1;
                } else {
                    insert_or_update_document(transaction, &cached.id, file, &hash, None)?;
                    if matches!(file.kind, DocumentKind::Note) {
                        index_note_document(transaction, config, &cached.id, file, &hash)?;
                        result.requires_link_resolution = true;
                        result.requires_property_catalog_refresh = true;
                    }
                    result.summary.updated += 1;
                }
            }
            None => {
                let hash = compute_content_hash(&file.absolute_path)?;
                let id = Ulid::new().to_string();
                insert_or_update_document(transaction, &id, file, &hash, None)?;
                if matches!(file.kind, DocumentKind::Note) {
                    index_note_document(transaction, config, &id, file, &hash)?;
                }
                result.requires_link_resolution = true;
                result.requires_property_catalog_refresh = matches!(file.kind, DocumentKind::Note);
                result.summary.added += 1;
            }
        }

        emit_scan_progress(
            on_progress,
            ScanProgress {
                mode,
                phase: ScanPhase::ScanningFiles,
                discovered: discovered.len(),
                processed: index + 1,
                added: result.summary.added,
                updated: result.summary.updated,
                unchanged: result.summary.unchanged,
                deleted: result.summary.deleted,
            },
        );
    }

    for path in deleted_paths {
        if let Some(cached) = existing.get(path) {
            delete_document(transaction, &cached.id)?;
            result.requires_link_resolution = true;
            result.requires_property_catalog_refresh = true;
            result.summary.deleted += 1;
            emit_scan_progress(
                on_progress,
                ScanProgress {
                    mode,
                    phase: ScanPhase::ScanningFiles,
                    discovered: discovered.len(),
                    processed: discovered.len(),
                    added: result.summary.added,
                    updated: result.summary.updated,
                    unchanged: result.summary.unchanged,
                    deleted: result.summary.deleted,
                },
            );
        }
    }

    debug_assert_eq!(seen_paths.len(), discovered.len());
    Ok(result)
}

fn emit_scan_progress(on_progress: &mut impl FnMut(ScanProgress), progress: ScanProgress) {
    on_progress(progress);
}

fn discover_files(vault_root: &Path) -> Result<Vec<DiscoveredFile>, ScanError> {
    let mut builder = WalkBuilder::new(vault_root);
    builder.hidden(true);
    builder.git_ignore(true);
    builder.git_global(false);
    builder.git_exclude(false);
    builder.parents(false);
    builder.require_git(false);

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let path = entry.path();
        let metadata = fs::metadata(path)?;
        let relative_path = normalize_relative_path(
            path.strip_prefix(vault_root)
                .expect("walked paths should always be inside the vault root"),
        );
        let filename = path
            .file_stem()
            .or_else(|| path.file_name())
            .and_then(|value| value.to_str())
            .ok_or_else(|| ScanError::MetadataOverflow {
                field: "filename",
                path: path.to_path_buf(),
            })?
            .to_string();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();

        files.push(DiscoveredFile {
            absolute_path: path.to_path_buf(),
            relative_path,
            filename,
            extension,
            kind: detect_document_kind(path),
            file_size: i64::try_from(metadata.len()).map_err(|_| ScanError::MetadataOverflow {
                field: "file_size",
                path: path.to_path_buf(),
            })?,
            file_mtime: system_time_to_millis(metadata.modified()?, path)?,
        });
    }

    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

pub(crate) fn discover_relative_paths(vault_root: &Path) -> Result<Vec<String>, ScanError> {
    Ok(discover_files(vault_root)?
        .into_iter()
        .map(|file| file.relative_path)
        .collect())
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::CurDir => None,
            other => Some(other.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn compute_content_hash(path: &Path) -> Result<Vec<u8>, ScanError> {
    Ok(blake3::hash(&fs::read(path)?).as_bytes().to_vec())
}

fn prepare_full_scan_documents(
    discovered: &[DiscoveredFile],
    config: &crate::VaultConfig,
) -> Result<Vec<PreparedFullScanDocument>, ScanError> {
    let batch_size = full_scan_prepare_batch_size();
    let mut prepared = Vec::with_capacity(discovered.len());

    for batch in discovered.chunks(batch_size) {
        let mut prepared_batch = batch
            .par_iter()
            .map(|file| prepare_full_scan_document(file, config))
            .collect::<Result<Vec<_>, _>>()?;
        prepared.append(&mut prepared_batch);
    }

    Ok(prepared)
}

fn full_scan_prepare_batch_size() -> usize {
    let workers = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    workers.saturating_mul(8).max(32)
}

fn prepare_full_scan_document(
    file: &DiscoveredFile,
    config: &crate::VaultConfig,
) -> Result<PreparedFullScanDocument, ScanError> {
    let bytes = fs::read(&file.absolute_path)?;
    let content_hash = blake3::hash(&bytes).as_bytes().to_vec();
    let parsed = if matches!(file.kind, DocumentKind::Note) {
        Some(parse_document(
            &decode_note_source(bytes, &file.absolute_path)?,
            config,
        ))
    } else {
        None
    };

    Ok(PreparedFullScanDocument {
        file: file.clone(),
        content_hash,
        parsed,
    })
}

fn decode_note_source(bytes: Vec<u8>, path: &Path) -> Result<String, ScanError> {
    String::from_utf8(bytes).map_err(|error| {
        ScanError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not valid UTF-8: {error}", path.display()),
        ))
    })
}

fn system_time_to_millis(time: SystemTime, path: &Path) -> Result<i64, ScanError> {
    let millis = time.duration_since(UNIX_EPOCH)?.as_millis();
    i64::try_from(millis).map_err(|_| ScanError::MetadataOverflow {
        field: "file_mtime",
        path: path.to_path_buf(),
    })
}

fn load_cached_documents(
    connection: &Connection,
) -> Result<HashMap<String, CachedDocument>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "
        SELECT id, path, file_size, file_mtime, content_hash, parser_version
        FROM documents
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            CachedDocument {
                id: row.get(0)?,
                file_size: row.get(2)?,
                file_mtime: row.get(3)?,
                content_hash: row.get(4)?,
                parser_version: row.get(5)?,
            },
        ))
    })?;

    rows.collect::<Result<HashMap<_, _>, _>>()
}

fn insert_or_update_document(
    transaction: &Transaction<'_>,
    id: &str,
    file: &DiscoveredFile,
    content_hash: &[u8],
    raw_frontmatter: Option<&str>,
) -> Result<(), ScanError> {
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
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(path) DO UPDATE SET
            filename = excluded.filename,
            extension = excluded.extension,
            content_hash = excluded.content_hash,
            raw_frontmatter = excluded.raw_frontmatter,
            file_size = excluded.file_size,
            file_mtime = excluded.file_mtime,
            parser_version = excluded.parser_version,
            indexed_at = excluded.indexed_at
        ",
        params![
            id,
            file.relative_path,
            file.filename,
            file.extension,
            content_hash,
            raw_frontmatter,
            file.file_size,
            file.file_mtime,
            PARSER_VERSION,
            current_timestamp()?,
        ],
    )?;

    Ok(())
}

fn index_note_document(
    transaction: &Transaction<'_>,
    config: &crate::VaultConfig,
    id: &str,
    file: &DiscoveredFile,
    content_hash: &[u8],
) -> Result<(), ScanError> {
    let source = fs::read_to_string(&file.absolute_path)?;
    let parsed = parse_document(&source, config);
    insert_or_update_document(
        transaction,
        id,
        file,
        content_hash,
        parsed.raw_frontmatter.as_deref(),
    )?;
    replace_derived_rows(transaction, id, config, &parsed)?;

    Ok(())
}

fn apply_prepared_full_scan_document(
    transaction: &Transaction<'_>,
    config: &crate::VaultConfig,
    id: &str,
    prepared: &PreparedFullScanDocument,
) -> Result<(), ScanError> {
    insert_or_update_document(
        transaction,
        id,
        &prepared.file,
        &prepared.content_hash,
        prepared
            .parsed
            .as_ref()
            .and_then(|parsed| parsed.raw_frontmatter.as_deref()),
    )?;
    if let Some(parsed) = prepared.parsed.as_ref() {
        replace_derived_rows(transaction, id, config, parsed)?;
    }
    Ok(())
}

fn update_document_metadata(
    transaction: &Transaction<'_>,
    id: &str,
    file: &DiscoveredFile,
) -> Result<(), ScanError> {
    transaction.execute(
        "
        UPDATE documents
        SET filename = ?2,
            extension = ?3,
            file_size = ?4,
            file_mtime = ?5
        WHERE id = ?1
        ",
        params![
            id,
            file.filename,
            file.extension,
            file.file_size,
            file.file_mtime
        ],
    )?;

    Ok(())
}

fn delete_document(transaction: &Transaction<'_>, id: &str) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "DELETE FROM search_chunk_content WHERE document_id = ?1",
        [id],
    )?;
    transaction.execute("DELETE FROM documents WHERE id = ?1", [id])?;
    Ok(())
}

fn replace_derived_rows(
    transaction: &Transaction<'_>,
    document_id: &str,
    config: &crate::VaultConfig,
    parsed: &ParsedDocument,
) -> Result<(), ScanError> {
    let reusable_chunk_ids = load_reusable_chunk_ids(transaction, document_id)?;
    clear_derived_rows(transaction, document_id)?;
    insert_headings(transaction, document_id, &parsed.headings)?;
    insert_block_refs(transaction, document_id, &parsed.block_refs)?;
    insert_links(transaction, document_id, &parsed.links)?;
    insert_aliases(transaction, document_id, &parsed.aliases)?;
    insert_tags(transaction, document_id, &parsed.tags)?;
    insert_chunks(
        transaction,
        document_id,
        &parsed.chunk_texts,
        reusable_chunk_ids,
    )?;
    replace_chunk_search_rows(transaction, document_id)?;
    insert_diagnostics(transaction, document_id, &parsed.diagnostics)?;
    if let Some(properties) = extract_indexed_properties(
        parsed.raw_frontmatter.as_deref(),
        parsed.frontmatter.as_ref(),
        config,
    )
    .map_err(|error| ScanError::Io(std::io::Error::other(error)))?
    {
        insert_properties(transaction, document_id, &properties)?;
        insert_property_values(transaction, document_id, &properties)?;
        insert_property_list_items(transaction, document_id, &properties)?;
        insert_property_diagnostics(transaction, document_id, &properties)?;
    }
    Ok(())
}

fn replace_chunk_search_rows(
    transaction: &Transaction<'_>,
    document_id: &str,
) -> Result<(), ScanError> {
    let document_title: String = transaction.query_row(
        "SELECT filename FROM documents WHERE id = ?1",
        [document_id],
        |row| row.get(0),
    )?;
    let aliases = load_search_alias_text(transaction, document_id)?;
    let mut statement = transaction.prepare(
        "
        SELECT id, content, heading_path
        FROM chunks
        WHERE document_id = ?1
        ORDER BY sequence_index
        ",
    )?;
    let rows = statement.query_map([document_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for row in rows {
        let (chunk_id, content, heading_path) = row?;
        transaction.execute(
            "
            INSERT INTO search_chunk_content (
                chunk_id,
                document_id,
                content,
                document_title,
                aliases,
                headings
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                chunk_id,
                document_id,
                content,
                document_title,
                aliases,
                flatten_heading_path(&heading_path)?,
            ],
        )?;
    }

    Ok(())
}

fn insert_headings(
    transaction: &Transaction<'_>,
    document_id: &str,
    headings: &[crate::RawHeading],
) -> Result<(), ScanError> {
    for heading in headings {
        transaction.execute(
            "
            INSERT INTO headings (id, document_id, level, text, byte_offset)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            params![
                Ulid::new().to_string(),
                document_id,
                i64::from(heading.level),
                &heading.text,
                i64::try_from(heading.byte_offset).map_err(|_| ScanError::MetadataOverflow {
                    field: "heading.byte_offset",
                    path: PathBuf::from(document_id),
                })?,
            ],
        )?;
    }
    Ok(())
}

fn insert_block_refs(
    transaction: &Transaction<'_>,
    document_id: &str,
    block_refs: &[crate::RawBlockRef],
) -> Result<(), ScanError> {
    for block_ref in block_refs {
        transaction.execute(
            "
            INSERT INTO block_refs (
                id,
                document_id,
                block_id_text,
                block_id_byte_offset,
                target_block_byte_start,
                target_block_byte_end
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                Ulid::new().to_string(),
                document_id,
                &block_ref.block_id_text,
                i64::try_from(block_ref.block_id_byte_offset).map_err(|_| {
                    ScanError::MetadataOverflow {
                        field: "block_ref.byte_offset",
                        path: PathBuf::from(document_id),
                    }
                })?,
                i64::try_from(block_ref.target_block_byte_start).map_err(|_| {
                    ScanError::MetadataOverflow {
                        field: "block_ref.target_start",
                        path: PathBuf::from(document_id),
                    }
                })?,
                i64::try_from(block_ref.target_block_byte_end).map_err(|_| {
                    ScanError::MetadataOverflow {
                        field: "block_ref.target_end",
                        path: PathBuf::from(document_id),
                    }
                })?,
            ],
        )?;
    }
    Ok(())
}

fn insert_links(
    transaction: &Transaction<'_>,
    document_id: &str,
    links: &[crate::RawLink],
) -> Result<(), ScanError> {
    for link in links {
        transaction.execute(
            "
            INSERT INTO links (
                id,
                source_document_id,
                raw_text,
                link_kind,
                display_text,
                target_path_candidate,
                target_heading,
                target_block,
                resolved_target_id,
                origin_context,
                byte_offset
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)
            ",
            params![
                Ulid::new().to_string(),
                document_id,
                &link.raw_text,
                link_kind_name(link.link_kind),
                link.display_text.as_deref(),
                link.target_path_candidate.as_deref(),
                link.target_heading.as_deref(),
                link.target_block.as_deref(),
                origin_context_name(link.origin_context),
                i64::try_from(link.byte_offset).map_err(|_| ScanError::MetadataOverflow {
                    field: "link.byte_offset",
                    path: PathBuf::from(document_id),
                })?,
            ],
        )?;
    }
    Ok(())
}

fn insert_aliases(
    transaction: &Transaction<'_>,
    document_id: &str,
    aliases: &[String],
) -> Result<(), ScanError> {
    for alias in aliases {
        transaction.execute(
            "
            INSERT INTO aliases (id, document_id, alias_text)
            VALUES (?1, ?2, ?3)
            ",
            params![Ulid::new().to_string(), document_id, alias],
        )?;
    }
    Ok(())
}

fn insert_tags(
    transaction: &Transaction<'_>,
    document_id: &str,
    tags: &[crate::RawTag],
) -> Result<(), ScanError> {
    for tag in tags {
        transaction.execute(
            "
            INSERT INTO tags (id, document_id, tag_text)
            VALUES (?1, ?2, ?3)
            ",
            params![Ulid::new().to_string(), document_id, &tag.tag_text],
        )?;
    }
    Ok(())
}

fn insert_chunks(
    transaction: &Transaction<'_>,
    document_id: &str,
    chunks: &[crate::ChunkText],
    mut reusable_chunk_ids: HashMap<ChunkReuseKey, VecDeque<String>>,
) -> Result<(), ScanError> {
    for chunk in chunks {
        let chunk_id = reusable_chunk_ids
            .get_mut(&chunk_reuse_key(chunk))
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| Ulid::new().to_string());
        transaction.execute(
            "
            INSERT INTO chunks (
                id,
                document_id,
                sequence_index,
                heading_path,
                byte_offset_start,
                byte_offset_end,
                content_hash,
                content,
                chunk_strategy,
                chunk_version
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                chunk_id,
                document_id,
                i64::try_from(chunk.sequence_index).map_err(|_| ScanError::MetadataOverflow {
                    field: "chunk.sequence_index",
                    path: PathBuf::from(document_id),
                })?,
                serde_json::to_string(&chunk.heading_path)
                    .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
                i64::try_from(chunk.byte_offset_start).map_err(|_| {
                    ScanError::MetadataOverflow {
                        field: "chunk.byte_offset_start",
                        path: PathBuf::from(document_id),
                    }
                })?,
                i64::try_from(chunk.byte_offset_end).map_err(|_| ScanError::MetadataOverflow {
                    field: "chunk.byte_offset_end",
                    path: PathBuf::from(document_id),
                })?,
                chunk.content_hash.clone(),
                &chunk.content,
                &chunk.chunk_strategy,
                i64::from(chunk.chunk_version),
            ],
        )?;
    }
    Ok(())
}

fn load_reusable_chunk_ids(
    transaction: &Transaction<'_>,
    document_id: &str,
) -> Result<HashMap<ChunkReuseKey, VecDeque<String>>, ScanError> {
    let mut statement = transaction.prepare(
        "
        SELECT id, heading_path, content_hash, chunk_strategy, chunk_version
        FROM chunks
        WHERE document_id = ?1
        ORDER BY sequence_index
        ",
    )?;
    let rows = statement.query_map([document_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, u32>(4)?,
        ))
    })?;

    let mut reusable_chunk_ids = HashMap::<ChunkReuseKey, VecDeque<String>>::new();
    for row in rows {
        let (chunk_id, heading_path, content_hash, chunk_strategy, chunk_version) = row?;
        let key = ChunkReuseKey {
            heading_path: serde_json::from_str(&heading_path)
                .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
            content_hash,
            chunk_strategy,
            chunk_version,
        };
        reusable_chunk_ids
            .entry(key)
            .or_default()
            .push_back(chunk_id);
    }

    Ok(reusable_chunk_ids)
}

fn chunk_reuse_key(chunk: &crate::ChunkText) -> ChunkReuseKey {
    ChunkReuseKey {
        heading_path: chunk.heading_path.clone(),
        content_hash: chunk.content_hash.clone(),
        chunk_strategy: chunk.chunk_strategy.clone(),
        chunk_version: chunk.chunk_version,
    }
}

fn insert_diagnostics(
    transaction: &Transaction<'_>,
    document_id: &str,
    diagnostics: &[crate::ParseDiagnostic],
) -> Result<(), ScanError> {
    for diagnostic in diagnostics {
        transaction.execute(
            "
            INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                Ulid::new().to_string(),
                document_id,
                diagnostic_kind_name(diagnostic.kind),
                &diagnostic.message,
                serde_json::to_string(&json!({
                    "byte_range": diagnostic.byte_range.as_ref().map(|range| {
                        json!({"start": range.start, "end": range.end})
                    }),
                }))
                .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
                current_timestamp()?,
            ],
        )?;
    }
    Ok(())
}

fn insert_properties(
    transaction: &Transaction<'_>,
    document_id: &str,
    properties: &IndexedProperties,
) -> Result<(), ScanError> {
    transaction.execute(
        "
        INSERT INTO properties (document_id, raw_yaml, canonical_json)
        VALUES (?1, ?2, ?3)
        ",
        params![
            document_id,
            &properties.raw_yaml,
            &properties.canonical_json
        ],
    )?;
    Ok(())
}

fn insert_property_values(
    transaction: &Transaction<'_>,
    document_id: &str,
    properties: &IndexedProperties,
) -> Result<(), ScanError> {
    for property in &properties.values {
        transaction.execute(
            "
            INSERT INTO property_values (
                document_id,
                key,
                value_text,
                value_number,
                value_bool,
                value_date,
                value_type
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                document_id,
                &property.key,
                property.value_text.as_deref(),
                property.value_number,
                property.value_bool.map(i64::from),
                property.value_date.as_deref(),
                &property.value_type,
            ],
        )?;
    }
    Ok(())
}

fn insert_property_list_items(
    transaction: &Transaction<'_>,
    document_id: &str,
    properties: &IndexedProperties,
) -> Result<(), ScanError> {
    for item in &properties.list_items {
        transaction.execute(
            "
            INSERT INTO property_list_items (document_id, key, item_index, value_text)
            VALUES (?1, ?2, ?3, ?4)
            ",
            params![
                document_id,
                &item.key,
                i64::try_from(item.item_index).map_err(|_| ScanError::MetadataOverflow {
                    field: "property_list_items.item_index",
                    path: PathBuf::from(document_id),
                })?,
                &item.value_text,
            ],
        )?;
    }
    Ok(())
}

fn insert_property_diagnostics(
    transaction: &Transaction<'_>,
    document_id: &str,
    properties: &IndexedProperties,
) -> Result<(), ScanError> {
    for diagnostic in &properties.diagnostics {
        transaction.execute(
            "
            INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
            VALUES (?1, ?2, 'type_mismatch', ?3, ?4, ?5)
            ",
            params![
                Ulid::new().to_string(),
                document_id,
                &diagnostic.message,
                serde_json::to_string(&json!({
                    "key": diagnostic.key,
                    "expected_type": diagnostic.expected_type,
                    "actual_type": diagnostic.actual_type,
                }))
                .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
                current_timestamp()?,
            ],
        )?;
    }
    Ok(())
}

fn clear_derived_rows(
    transaction: &Transaction<'_>,
    document_id: &str,
) -> Result<(), rusqlite::Error> {
    for table_name in [
        "headings",
        "block_refs",
        "links",
        "aliases",
        "tags",
        "properties",
        "property_values",
        "property_list_items",
        "search_chunk_content",
        "chunks",
        "diagnostics",
    ] {
        let key_column = if table_name == "links" {
            "source_document_id"
        } else {
            "document_id"
        };
        transaction.execute(
            &format!("DELETE FROM {table_name} WHERE {key_column} = ?1"),
            [document_id],
        )?;
    }
    Ok(())
}

fn load_search_alias_text(
    transaction: &Transaction<'_>,
    document_id: &str,
) -> Result<String, ScanError> {
    let mut statement = transaction.prepare(
        "
        SELECT alias_text
        FROM aliases
        WHERE document_id = ?1
        ORDER BY alias_text
        ",
    )?;
    let rows = statement.query_map([document_id], |row| row.get::<_, String>(0))?;

    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .collect::<Vec<_>>()
        .join(" "))
}

fn flatten_heading_path(heading_path: &str) -> Result<String, ScanError> {
    let values = serde_json::from_str::<Vec<String>>(heading_path)
        .map_err(|error| ScanError::Io(std::io::Error::other(error)))?;
    Ok(values.join(" "))
}

fn resolve_all_links(
    transaction: &Transaction<'_>,
    mode: crate::LinkResolutionMode,
) -> Result<(), ScanError> {
    transaction.execute("DELETE FROM diagnostics WHERE kind = 'unresolved_link'", [])?;

    let documents = load_resolver_documents(transaction)?;
    let links = load_resolver_links(transaction)?;

    for link in links {
        let resolution = resolve_link(&documents, &link.resolver_link, mode);
        transaction.execute(
            "UPDATE links SET resolved_target_id = ?2 WHERE id = ?1",
            params![link.id, resolution.resolved_target_id],
        )?;

        if let Some(problem) = resolution.problem {
            transaction.execute(
                "
                INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
                VALUES (?1, ?2, 'unresolved_link', ?3, ?4, ?5)
                ",
                params![
                    Ulid::new().to_string(),
                    link.resolver_link.source_document_id,
                    resolution_problem_message(&problem, &link.resolver_link),
                    serde_json::to_string(&resolution_problem_detail(
                        &problem,
                        &link.resolver_link
                    ))
                    .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
                    current_timestamp()?,
                ],
            )?;
        }
    }

    Ok(())
}

fn load_resolver_documents(
    transaction: &Transaction<'_>,
) -> Result<Vec<ResolverDocument>, ScanError> {
    let mut alias_statement = transaction
        .prepare("SELECT document_id, alias_text FROM aliases ORDER BY document_id, alias_text")?;
    let alias_rows = alias_statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut aliases_by_document = HashMap::new();
    for row in alias_rows {
        let (document_id, alias_text) = row?;
        aliases_by_document
            .entry(document_id)
            .or_insert_with(Vec::new)
            .push(alias_text);
    }

    let mut statement =
        transaction.prepare("SELECT id, path, filename FROM documents ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        let id: String = row.get(0)?;
        Ok(ResolverDocument {
            aliases: aliases_by_document.remove(&id).unwrap_or_default(),
            path: row.get(1)?,
            filename: row.get(2)?,
            id,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>().map_err(ScanError::from)
}

fn load_resolver_links(transaction: &Transaction<'_>) -> Result<Vec<ResolverLinkRow>, ScanError> {
    let mut statement = transaction.prepare(
        "
        SELECT
            l.id,
            l.source_document_id,
            s.path,
            l.target_path_candidate,
            l.link_kind
        FROM links l
        JOIN documents s ON s.id = l.source_document_id
        ORDER BY l.byte_offset
        ",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ResolverLinkRow {
            id: row.get(0)?,
            resolver_link: ResolverLink {
                source_document_id: row.get(1)?,
                source_path: row.get(2)?,
                target_path_candidate: row.get(3)?,
                link_kind: parse_link_kind(&row.get::<_, String>(4)?),
            },
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>().map_err(ScanError::from)
}

fn resolution_problem_message(problem: &LinkResolutionProblem, link: &ResolverLink) -> String {
    let target = link.target_path_candidate.as_deref().unwrap_or("(self)");
    match problem {
        LinkResolutionProblem::Unresolved => format!("Unresolved link target: {target}"),
        LinkResolutionProblem::Ambiguous(_) => format!("Ambiguous link target: {target}"),
    }
}

fn resolution_problem_detail(
    problem: &LinkResolutionProblem,
    link: &ResolverLink,
) -> serde_json::Value {
    match problem {
        LinkResolutionProblem::Unresolved => json!({
            "reason": "unresolved",
            "target": link.target_path_candidate,
        }),
        LinkResolutionProblem::Ambiguous(matches) => json!({
            "reason": "ambiguous",
            "target": link.target_path_candidate,
            "matches": matches,
        }),
    }
}

fn parse_link_kind(link_kind: &str) -> LinkKind {
    match link_kind {
        "wikilink" => LinkKind::Wikilink,
        "embed" => LinkKind::Embed,
        "external" => LinkKind::External,
        _ => LinkKind::Markdown,
    }
}

struct ResolverLinkRow {
    id: String,
    resolver_link: ResolverLink,
}

fn link_kind_name(link_kind: LinkKind) -> &'static str {
    match link_kind {
        LinkKind::Wikilink => "wikilink",
        LinkKind::Markdown => "markdown",
        LinkKind::Embed => "embed",
        LinkKind::External => "external",
    }
}

fn origin_context_name(origin_context: OriginContext) -> &'static str {
    match origin_context {
        OriginContext::Body => "body",
        OriginContext::Frontmatter => "frontmatter",
        OriginContext::Property => "property",
    }
}

fn diagnostic_kind_name(kind: ParseDiagnosticKind) -> &'static str {
    match kind {
        ParseDiagnosticKind::MalformedFrontmatter => "parse_error",
        ParseDiagnosticKind::HtmlLink | ParseDiagnosticKind::LinkInComment => "unsupported_syntax",
    }
}

fn current_timestamp() -> Result<String, ScanError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{load_vault_config, LinkResolutionMode, LinkStylePreference};
    use serde_json::{json, Value};
    use tempfile::TempDir;

    #[test]
    fn normalize_relative_path_uses_forward_slashes() {
        let path = PathBuf::from_iter(["people", "bob.md"]);

        assert_eq!(normalize_relative_path(&path), "people/bob.md");
    }

    #[test]
    fn document_kind_detection_matches_extensions() {
        assert_eq!(
            detect_document_kind(Path::new("note.md")),
            DocumentKind::Note
        );
        assert_eq!(
            detect_document_kind(Path::new("view.base")),
            DocumentKind::Base
        );
        assert_eq!(
            detect_document_kind(Path::new("image.png")),
            DocumentKind::Attachment
        );
    }

    #[test]
    fn content_hash_matches_blake3_output() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let file = temp_dir.path().join("note.md");
        fs::write(&file, "hello world").expect("fixture file should be written");

        let hash = compute_content_hash(&file).expect("hash should be computed");

        assert_eq!(hash, blake3::hash(b"hello world").as_bytes().to_vec());
    }

    #[test]
    fn full_scan_indexes_fixture_vault() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        let summary = scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");

        assert_eq!(
            summary,
            ScanSummary {
                mode: ScanMode::Full,
                discovered: 3,
                added: 3,
                updated: 0,
                unchanged: 0,
                deleted: 0,
            }
        );

        let database = CacheDatabase::open(&paths).expect("database should open");
        assert_eq!(
            document_paths(database.connection()),
            vec![
                "Home.md".to_string(),
                "People/Bob.md".to_string(),
                "Projects/Alpha.md".to_string(),
            ]
        );
        assert_eq!(count_rows(database.connection(), "headings"), 4);
        assert_eq!(count_rows(database.connection(), "block_refs"), 0);
        assert_eq!(count_rows(database.connection(), "links"), 5);
        assert_eq!(count_rows(database.connection(), "aliases"), 2);
        assert_eq!(count_rows(database.connection(), "tags"), 5);
        assert_eq!(count_rows(database.connection(), "chunks"), 4);
        assert_eq!(count_rows(database.connection(), "search_chunk_content"), 4);
        assert_eq!(count_rows(database.connection(), "diagnostics"), 0);
    }

    #[test]
    fn scan_progress_reports_scan_and_resolve_phases() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);
        let mut events = Vec::new();

        let summary = scan_vault_with_progress(&paths, ScanMode::Full, |progress| {
            events.push(progress);
        })
        .expect("scan should succeed");

        assert_eq!(summary.discovered, 3);
        assert!(!events.is_empty());
        assert_eq!(
            events.first().expect("first event should exist").phase,
            ScanPhase::PreparingFiles
        );
        assert_eq!(
            events
                .iter()
                .rfind(|event| event.phase == ScanPhase::ScanningFiles)
                .expect("scan event should exist")
                .processed,
            3
        );
        assert!(events
            .iter()
            .any(|event| event.phase == ScanPhase::RefreshingPropertyCatalog));
        assert!(events
            .iter()
            .any(|event| event.phase == ScanPhase::ResolvingLinks));
        assert_eq!(
            events.last().expect("last event should exist").phase,
            ScanPhase::Completed
        );
    }

    #[test]
    fn mixed_properties_vault_indexes_property_projections_and_type_diagnostics() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("mixed-properties", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        let summary = scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.discovered, 3);
        assert_eq!(count_rows(database.connection(), "properties"), 3);
        assert_eq!(count_rows(database.connection(), "property_values"), 18);
        assert_eq!(count_rows(database.connection(), "property_list_items"), 4);
        assert_eq!(
            property_value_type(database.connection(), "Done.md", "due"),
            Some("date".to_string())
        );
        assert_eq!(
            property_value_type(database.connection(), "Done.md", "empty_list"),
            Some("list".to_string())
        );
        assert_eq!(
            property_list_items(database.connection(), "Done.md", "related"),
            vec!["[[Backlog]]".to_string(), "sprint".to_string()]
        );
        assert_eq!(
            diagnostic_count_by_kind(database.connection(), "type_mismatch"),
            5
        );
    }

    #[test]
    fn incremental_scan_skips_unchanged_files() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("initial full scan should succeed");
        let summary =
            scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");

        assert_eq!(
            summary,
            ScanSummary {
                mode: ScanMode::Incremental,
                discovered: 3,
                added: 0,
                updated: 0,
                unchanged: 3,
                deleted: 0,
            }
        );
    }

    #[test]
    fn no_op_incremental_scan_preserves_existing_link_resolution_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("ambiguous-links", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("initial full scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let before_links = resolved_links(database.connection());
        let before_diagnostic_ids =
            diagnostic_ids_by_kind(database.connection(), "unresolved_link");
        drop(database);

        let summary =
            scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.updated, 0);
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.added, 0);
        assert_eq!(summary.unchanged, 4);
        assert_eq!(resolved_links(database.connection()), before_links);
        assert_eq!(
            diagnostic_ids_by_kind(database.connection(), "unresolved_link"),
            before_diagnostic_ids
        );
    }

    #[test]
    fn no_op_incremental_scan_preserves_chunk_search_rows() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("initial full scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let before_rows = chunk_search_rows(database.connection());
        drop(database);

        let summary =
            scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.updated, 0);
        assert_eq!(summary.deleted, 0);
        assert_eq!(summary.added, 0);
        assert_eq!(summary.unchanged, 3);
        assert_eq!(chunk_search_rows(database.connection()), before_rows);
    }

    #[test]
    fn full_scan_populates_chunk_search_index() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(
            search_chunk_paths(database.connection(), "dashboard"),
            vec!["Home.md"]
        );
        assert_eq!(
            search_chunk_paths(database.connection(), "Start"),
            vec!["Home.md"]
        );
        assert_eq!(
            search_chunk_paths(database.connection(), "Robert"),
            vec!["People/Bob.md"]
        );
    }

    #[test]
    fn incremental_scan_refreshes_chunk_search_rows_for_changed_notes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("initial full scan should succeed");
        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Launch\n---\n\n# Home\n\nPortal note.\n",
        )
        .expect("updated note should be written");

        let summary =
            scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.updated, 1);
        assert_eq!(
            search_chunk_paths(database.connection(), "dashboard"),
            Vec::<String>::new()
        );
        assert_eq!(
            search_chunk_paths(database.connection(), "Start"),
            Vec::<String>::new()
        );
        assert_eq!(
            search_chunk_paths(database.connection(), "Launch"),
            vec!["Home.md"]
        );
        assert_eq!(
            search_chunk_paths(database.connection(), "Portal"),
            vec!["Home.md"]
        );
    }

    #[test]
    fn incremental_scan_reuses_chunk_ids_when_frontmatter_changes_only() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("initial full scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let before_chunk_ids = chunk_ids_for_document(database.connection(), "Home.md");
        drop(database);

        fs::write(
            vault_root.join("Home.md"),
            "---\naliases:\n  - Start\n  - Landing\n\
             tags:\n  - dashboard\n---\n\n# Home\n\n\
             Home links to [[Projects/Alpha]] and [[People/Bob|Bob]].\n\n\
             The dashboard note uses the tag #index.\n",
        )
        .expect("updated note should be written");

        let summary =
            scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.updated, 1);
        assert_eq!(
            chunk_ids_for_document(database.connection(), "Home.md"),
            before_chunk_ids
        );
    }

    #[test]
    fn scan_respects_gitignore_and_hidden_directories() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join("notes")).expect("notes dir should be created");
        fs::create_dir_all(vault_root.join(".hidden")).expect("hidden dir should be created");
        fs::write(vault_root.join(".gitignore"), "ignored.md\n").expect("gitignore should exist");
        fs::write(vault_root.join("notes/keep.md"), "# keep").expect("keep note should exist");
        fs::write(vault_root.join("ignored.md"), "# ignored").expect("ignored note should exist");
        fs::write(vault_root.join(".hidden/secret.md"), "# secret")
            .expect("hidden note should exist");
        let paths = VaultPaths::new(&vault_root);

        let summary = scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.discovered, 1);
        assert_eq!(
            document_paths(database.connection()),
            vec!["notes/keep.md".to_string()]
        );
    }

    #[test]
    fn scan_ignores_parent_gitignore_outside_the_vault() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let parent_root = temp_dir.path().join("parent");
        let vault_root = parent_root.join("vault");
        fs::create_dir_all(vault_root.join("notes")).expect("notes dir should be created");
        fs::write(parent_root.join(".gitignore"), "**/*.md\n")
            .expect("parent gitignore should exist");
        fs::write(vault_root.join("a.md"), "# a").expect("root note should exist");
        fs::write(vault_root.join("notes/b.md"), "# b").expect("nested note should exist");
        let paths = VaultPaths::new(&vault_root);

        let summary = scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.discovered, 2);
        assert_eq!(
            document_paths(database.connection()),
            vec!["a.md".to_string(), "notes/b.md".to_string()]
        );
    }

    #[test]
    fn fixture_config_is_loaded_with_obsidian_defaults() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        let loaded = load_vault_config(&paths);

        assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.config.link_resolution, LinkResolutionMode::Shortest);
        assert_eq!(loaded.config.link_style, LinkStylePreference::Wikilink);
        assert_eq!(
            loaded.config.property_types.get("status"),
            Some(&"text".to_string())
        );
    }

    #[test]
    fn broken_frontmatter_vault_emits_parse_diagnostics() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("broken-frontmatter", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        let summary = scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(summary.discovered, 2);
        assert_eq!(count_rows(database.connection(), "documents"), 2);
        assert_eq!(count_rows(database.connection(), "diagnostics"), 1);
        assert_eq!(
            diagnostic_kinds(database.connection()),
            vec!["parse_error".to_string()]
        );
    }

    #[test]
    fn messy_block_scalar_frontmatter_is_recovered_without_parse_errors() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault should be created");
        fs::write(
            vault_root.join("card.md"),
            concat!(
                "---\n",
                "title: Panam\n",
                "cardFirstMessage: |-\n",
                "  **Panam:** First line.\n",
                "![Tumblr](assets/example.gif)\n",
                "cardSummary: Summary\n",
                "---\n",
                "\n",
                "# Card\n",
                "\n",
                "Body.\n",
            ),
        )
        .expect("fixture note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");

        assert_eq!(
            diagnostic_count_by_kind(database.connection(), "parse_error"),
            0
        );
        assert_eq!(count_rows(database.connection(), "documents"), 1);
    }

    #[test]
    fn ambiguous_links_fixture_resolves_and_emits_diagnostics() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("ambiguous-links", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let resolved = resolved_links(database.connection());

        assert_eq!(
            resolved,
            vec![
                (
                    "Projects/Source.md".to_string(),
                    "[[Topic]]".to_string(),
                    Some("Projects/Topic.md".to_string())
                ),
                (
                    "Projects/Source.md".to_string(),
                    "[[Archived Topic]]".to_string(),
                    Some("Archive/Topic.md".to_string())
                ),
                ("Root.md".to_string(), "[[Topic]]".to_string(), None),
            ]
        );
        assert_eq!(
            diagnostic_kinds(database.connection()),
            vec!["unresolved_link".to_string()]
        );
    }

    #[test]
    fn fixture_vaults_reindex_idempotently() {
        for fixture in fixture_names() {
            let temp_dir = TempDir::new().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault(fixture, &vault_root);
            let paths = VaultPaths::new(&vault_root);

            scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
            let before = cache_signature(
                CacheDatabase::open(&paths)
                    .expect("database should open")
                    .connection(),
            );

            let summary =
                scan_vault(&paths, ScanMode::Incremental).expect("incremental scan should succeed");
            let after = cache_signature(
                CacheDatabase::open(&paths)
                    .expect("database should open")
                    .connection(),
            );

            assert_eq!(summary.added, 0, "fixture {fixture} should not add rows");
            assert_eq!(
                summary.updated, 0,
                "fixture {fixture} should not update rows"
            );
            assert_eq!(
                summary.deleted, 0,
                "fixture {fixture} should not delete rows"
            );
            assert_eq!(before, after, "fixture {fixture} should be idempotent");
        }
    }

    #[test]
    fn rebuild_matches_incremental_cache_state_for_fixture_vaults() {
        for fixture in fixture_names() {
            let temp_dir = TempDir::new().expect("temp dir should be created");
            let vault_root = temp_dir.path().join("vault");
            copy_fixture_vault(fixture, &vault_root);
            let paths = VaultPaths::new(&vault_root);

            scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
            let baseline = cache_signature(
                CacheDatabase::open(&paths)
                    .expect("database should open")
                    .connection(),
            );

            crate::rebuild_vault(&paths, &crate::RebuildQuery { dry_run: false })
                .expect("rebuild should succeed");
            let rebuilt = cache_signature(
                CacheDatabase::open(&paths)
                    .expect("database should open")
                    .connection(),
            );

            assert_eq!(
                baseline, rebuilt,
                "fixture {fixture} should rebuild to the same logical state"
            );
        }
    }

    fn fixture_names() -> [&'static str; 6] {
        [
            "basic",
            "ambiguous-links",
            "mixed-properties",
            "broken-frontmatter",
            "move-rewrite",
            "bases",
        ]
    }

    fn cache_signature(connection: &Connection) -> Value {
        json!({
            "documents": document_signature_rows(connection),
            "headings": heading_signature_rows(connection),
            "block_refs": block_ref_signature_rows(connection),
            "links": link_signature_rows(connection),
            "aliases": alias_signature_rows(connection),
            "tags": tag_signature_rows(connection),
            "chunks": chunk_signature_rows(connection),
            "search_chunk_content": search_signature_rows(connection),
            "diagnostics": diagnostic_signature_rows(connection),
            "properties": property_signature_rows(connection),
            "property_values": property_value_signature_rows(connection),
            "property_list_items": property_list_item_signature_rows(connection),
            "property_catalog": property_catalog_signature_rows(connection),
        })
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

    fn resolved_links(connection: &Connection) -> Vec<(String, String, Option<String>)> {
        let mut statement = connection
            .prepare(
                "
                SELECT source.path, links.raw_text, target.path
                FROM links
                JOIN documents AS source ON source.id = links.source_document_id
                LEFT JOIN documents AS target ON target.id = links.resolved_target_id
                ORDER BY source.path, links.byte_offset
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn diagnostic_kinds(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT kind FROM diagnostics ORDER BY kind")
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| row.get(0))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn diagnostic_ids_by_kind(connection: &Connection, kind: &str) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT id FROM diagnostics WHERE kind = ?1 ORDER BY id")
            .expect("statement should prepare");
        let rows = statement
            .query_map([kind], |row| row.get(0))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn diagnostic_count_by_kind(connection: &Connection, kind: &str) -> i64 {
        connection
            .query_row(
                "SELECT COUNT(*) FROM diagnostics WHERE kind = ?1",
                [kind],
                |row| row.get(0),
            )
            .expect("diagnostic count should be readable")
    }

    fn count_rows(connection: &Connection, table_name: &str) -> i64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table_name}"), [], |row| {
                row.get(0)
            })
            .expect("row count should be readable")
    }

    fn search_chunk_paths(connection: &Connection, query: &str) -> Vec<String> {
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

    fn chunk_search_rows(connection: &Connection) -> Vec<(i64, String, String)> {
        let mut statement = connection
            .prepare(
                "
                SELECT id, chunk_id, content
                FROM search_chunk_content
                ORDER BY id
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn chunk_ids_for_document(connection: &Connection, path: &str) -> Vec<String> {
        let mut statement = connection
            .prepare(
                "
                SELECT chunks.id
                FROM chunks
                JOIN documents ON documents.id = chunks.document_id
                WHERE documents.path = ?1
                ORDER BY chunks.sequence_index
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([path], |row| row.get(0))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn property_value_type(connection: &Connection, path: &str, key: &str) -> Option<String> {
        connection
            .query_row(
                "
                SELECT property_values.value_type
                FROM property_values
                JOIN documents ON documents.id = property_values.document_id
                WHERE documents.path = ?1 AND property_values.key = ?2
                ",
                params![path, key],
                |row| row.get(0),
            )
            .ok()
    }

    fn property_list_items(connection: &Connection, path: &str, key: &str) -> Vec<String> {
        let mut statement = connection
            .prepare(
                "
                SELECT property_list_items.value_text
                FROM property_list_items
                JOIN documents ON documents.id = property_list_items.document_id
                WHERE documents.path = ?1 AND property_list_items.key = ?2
                ORDER BY property_list_items.item_index
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map(params![path, key], |row| row.get(0))
            .expect("query should succeed");

        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn document_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT path, filename, extension, raw_frontmatter, file_size, parser_version
                FROM documents
                ORDER BY path
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "path": row.get::<_, String>(0)?,
                    "filename": row.get::<_, String>(1)?,
                    "extension": row.get::<_, String>(2)?,
                    "raw_frontmatter": row.get::<_, Option<String>>(3)?,
                    "file_size": row.get::<_, i64>(4)?,
                    "parser_version": row.get::<_, u32>(5)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn heading_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, headings.level, headings.text, headings.byte_offset
                FROM headings
                JOIN documents ON documents.id = headings.document_id
                ORDER BY documents.path, headings.byte_offset
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "level": row.get::<_, i64>(1)?,
                    "text": row.get::<_, String>(2)?,
                    "byte_offset": row.get::<_, i64>(3)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn block_ref_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT
                    documents.path,
                    block_refs.block_id_text,
                    block_refs.block_id_byte_offset,
                    block_refs.target_block_byte_start,
                    block_refs.target_block_byte_end
                FROM block_refs
                JOIN documents ON documents.id = block_refs.document_id
                ORDER BY documents.path, block_refs.block_id_byte_offset
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "block_id_text": row.get::<_, String>(1)?,
                    "block_id_byte_offset": row.get::<_, i64>(2)?,
                    "target_block_byte_start": row.get::<_, i64>(3)?,
                    "target_block_byte_end": row.get::<_, i64>(4)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn link_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT
                    source.path,
                    links.raw_text,
                    links.link_kind,
                    links.display_text,
                    links.target_path_candidate,
                    links.target_heading,
                    links.target_block,
                    target.path,
                    links.origin_context,
                    links.byte_offset
                FROM links
                JOIN documents AS source ON source.id = links.source_document_id
                LEFT JOIN documents AS target ON target.id = links.resolved_target_id
                ORDER BY source.path, links.byte_offset, links.raw_text
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "source_path": row.get::<_, String>(0)?,
                    "raw_text": row.get::<_, String>(1)?,
                    "link_kind": row.get::<_, String>(2)?,
                    "display_text": row.get::<_, Option<String>>(3)?,
                    "target_path_candidate": row.get::<_, Option<String>>(4)?,
                    "target_heading": row.get::<_, Option<String>>(5)?,
                    "target_block": row.get::<_, Option<String>>(6)?,
                    "resolved_target_path": row.get::<_, Option<String>>(7)?,
                    "origin_context": row.get::<_, String>(8)?,
                    "byte_offset": row.get::<_, i64>(9)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn alias_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, aliases.alias_text
                FROM aliases
                JOIN documents ON documents.id = aliases.document_id
                ORDER BY documents.path, aliases.alias_text
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "alias_text": row.get::<_, String>(1)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn tag_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, tags.tag_text
                FROM tags
                JOIN documents ON documents.id = tags.document_id
                ORDER BY documents.path, tags.tag_text
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "tag_text": row.get::<_, String>(1)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn chunk_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT
                    documents.path,
                    chunks.sequence_index,
                    chunks.heading_path,
                    chunks.byte_offset_start,
                    chunks.byte_offset_end,
                    chunks.content_hash,
                    chunks.content,
                    chunks.chunk_strategy,
                    chunks.chunk_version
                FROM chunks
                JOIN documents ON documents.id = chunks.document_id
                ORDER BY documents.path, chunks.sequence_index
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "sequence_index": row.get::<_, i64>(1)?,
                    "heading_path": row.get::<_, String>(2)?,
                    "byte_offset_start": row.get::<_, i64>(3)?,
                    "byte_offset_end": row.get::<_, i64>(4)?,
                    "content_hash": blob_to_hex(&row.get::<_, Vec<u8>>(5)?),
                    "content": row.get::<_, String>(6)?,
                    "chunk_strategy": row.get::<_, String>(7)?,
                    "chunk_version": row.get::<_, i64>(8)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn search_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, search_chunk_content.content, search_chunk_content.document_title,
                       search_chunk_content.aliases, search_chunk_content.headings
                FROM search_chunk_content
                JOIN documents ON documents.id = search_chunk_content.document_id
                ORDER BY documents.path, search_chunk_content.id
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "content": row.get::<_, String>(1)?,
                    "document_title": row.get::<_, String>(2)?,
                    "aliases": row.get::<_, String>(3)?,
                    "headings": row.get::<_, String>(4)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn diagnostic_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, diagnostics.kind, diagnostics.message, diagnostics.detail
                FROM diagnostics
                LEFT JOIN documents ON documents.id = diagnostics.document_id
                ORDER BY documents.path, diagnostics.kind, diagnostics.message, diagnostics.detail
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, Option<String>>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "message": row.get::<_, String>(2)?,
                    "detail": normalize_diagnostic_detail(connection, &row.get::<_, String>(3)?),
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn normalize_diagnostic_detail(connection: &Connection, detail: &str) -> Value {
        let Ok(mut parsed) = serde_json::from_str::<Value>(detail) else {
            return Value::String(detail.to_string());
        };
        let Some(matches) = parsed.get_mut("matches").and_then(Value::as_array_mut) else {
            return parsed;
        };

        let mut normalized = matches
            .iter()
            .filter_map(Value::as_str)
            .map(|document_id| {
                connection
                    .query_row(
                        "SELECT path FROM documents WHERE id = ?1",
                        [document_id],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap_or_else(|_| document_id.to_string())
            })
            .collect::<Vec<_>>();
        normalized.sort();
        *matches = normalized.into_iter().map(Value::String).collect();

        parsed
    }

    fn property_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, properties.raw_yaml, properties.canonical_json
                FROM properties
                JOIN documents ON documents.id = properties.document_id
                ORDER BY documents.path
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "raw_yaml": row.get::<_, String>(1)?,
                    "canonical_json": row.get::<_, String>(2)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn property_value_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT
                    documents.path,
                    property_values.key,
                    property_values.value_text,
                    property_values.value_number,
                    property_values.value_bool,
                    property_values.value_date,
                    property_values.value_type
                FROM property_values
                JOIN documents ON documents.id = property_values.document_id
                ORDER BY documents.path, property_values.key
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "key": row.get::<_, String>(1)?,
                    "value_text": row.get::<_, Option<String>>(2)?,
                    "value_number": row.get::<_, Option<f64>>(3)?,
                    "value_bool": row.get::<_, Option<i64>>(4)?,
                    "value_date": row.get::<_, Option<String>>(5)?,
                    "value_type": row.get::<_, String>(6)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn property_list_item_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, property_list_items.key, property_list_items.item_index, property_list_items.value_text
                FROM property_list_items
                JOIN documents ON documents.id = property_list_items.document_id
                ORDER BY documents.path, property_list_items.key, property_list_items.item_index
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "document_path": row.get::<_, String>(0)?,
                    "key": row.get::<_, String>(1)?,
                    "item_index": row.get::<_, i64>(2)?,
                    "value_text": row.get::<_, String>(3)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn property_catalog_signature_rows(connection: &Connection) -> Vec<Value> {
        let mut statement = connection
            .prepare(
                "
                SELECT key, observed_type, usage_count, namespace
                FROM property_catalog
                ORDER BY key, observed_type, namespace
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok(json!({
                    "key": row.get::<_, String>(0)?,
                    "observed_type": row.get::<_, String>(1)?,
                    "usage_count": row.get::<_, i64>(2)?,
                    "namespace": row.get::<_, String>(3)?,
                }))
            })
            .expect("query should succeed");
        rows.map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn blob_to_hex(bytes: &[u8]) -> String {
        let mut hex = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut hex, "{byte:02x}").expect("writing to string should succeed");
        }
        hex
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
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}
