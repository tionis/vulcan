use crate::cache::{drop_fts_triggers, rebuild_fts_index, restore_fts_triggers, CacheError};
use crate::extraction::extract_attachment_chunks;
use crate::parser::{parse_document, LinkKind, OriginContext, ParseDiagnosticKind, ParsedDocument};
use crate::properties::{
    extract_indexed_properties, indexed_inline_property_value, rebuild_property_catalog,
    refresh_property_catalog_for_documents, IndexedProperties,
};
use crate::resolver::{LinkResolutionProblem, ResolverDocument, ResolverIndex, ResolverLink};
use crate::tasks::task_recurrence_properties;
use crate::write_lock::acquire_write_lock;
use crate::{load_vault_config, CacheDatabase, VaultPaths, PARSER_VERSION};
use ignore::WalkBuilder;
use rayon::prelude::*;
use rusqlite::{params, Connection, Transaction};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};
use ulid::Ulid;

/// When the number of changed + deleted files exceeds this threshold during an incremental
/// scan, FTS triggers are dropped and the FTS index is rebuilt in one pass. Below this
/// threshold, triggers handle FTS updates incrementally per row (avoiding an O(N) full rebuild).
const FTS_BULK_THRESHOLD: usize = 50;

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
    AttachmentExtraction(String),
    Cache(CacheError),
    Checkpoint(String),
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
            Self::AttachmentExtraction(error) | Self::Checkpoint(error) => {
                write!(formatter, "{error}")
            }
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
            Self::AttachmentExtraction(_) | Self::Checkpoint(_) | Self::MetadataOverflow { .. } => {
                None
            }
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

impl From<crate::extraction::AttachmentExtractionError> for ScanError {
    fn from(error: crate::extraction::AttachmentExtractionError) -> Self {
        Self::AttachmentExtraction(error.to_string())
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

impl From<crate::history::CheckpointError> for ScanError {
    fn from(error: crate::history::CheckpointError) -> Self {
        Self::Checkpoint(error.to_string())
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
#[allow(clippy::struct_excessive_bools)]
struct IncrementalScanResult {
    summary: ScanSummary,
    requires_link_resolution: bool,
    /// When true, the target pool changed (adds/deletes) so all links must be re-resolved.
    /// When false but `requires_link_resolution` is true, only links from changed documents
    /// need re-resolution.
    target_pool_changed: bool,
    /// Document IDs that were added, updated, or deleted.
    changed_document_ids: Vec<String>,
    requires_property_catalog_refresh: bool,
    requires_fts_rebuild: bool,
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
    derived: PreparedDerivedContent,
}

#[derive(Debug, Clone, PartialEq)]
struct PreparedNoteContent {
    source: String,
    parsed: Box<ParsedDocument>,
}

#[derive(Debug, Clone, PartialEq)]
enum PreparedDerivedContent {
    None,
    Note(PreparedNoteContent),
    Attachment(Vec<crate::ChunkText>),
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

    let summary = match mode {
        ScanMode::Full => {
            let discovered_count = discovered.len();
            let deleted_count = deleted_paths.len();
            let prepared = prepare_full_scan_documents(&discovered, &config)?;
            // Disable FK checks for the bulk insert phase.  FK integrity is guaranteed
            // because documents are inserted before all derived rows (headings, links,
            // tags, etc.) within the same transaction.  The pragma must be set outside
            // the transaction.
            database.set_foreign_keys(false).map_err(ScanError::Cache)?;
            let rebuild_result =
                database.rebuild_with(|transaction| -> Result<ScanSummary, ScanError> {
                    // Disable FTS triggers — we'll rebuild the index in one pass at the end.
                    drop_fts_triggers(transaction)?;
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
                    rebuild_fts_index(transaction)?;
                    restore_fts_triggers(transaction)?;

                    Ok(ScanSummary {
                        mode,
                        discovered: discovered_count,
                        added: discovered_count,
                        updated: 0,
                        unchanged: 0,
                        deleted: deleted_count,
                    })
                });
            // Restore FK checks regardless of rebuild outcome.
            let _ = database.set_foreign_keys(true);
            let summary = rebuild_result?;
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
            crate::history::record_scan_checkpoint(database.connection())?;
            summary
        }
        ScanMode::Incremental => {
            let result = database.with_transaction(
                |transaction| -> Result<IncrementalScanResult, ScanError> {
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
                        if result.target_pool_changed {
                            // Documents added/deleted — full catalog rebuild is needed.
                            rebuild_property_catalog(transaction, &config.property_types)?;
                        } else {
                            // Only updates — refresh catalog entries for changed documents.
                            refresh_property_catalog_for_documents(
                                transaction,
                                &result.changed_document_ids,
                                &config.property_types,
                            )?;
                        }
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
                        if result.target_pool_changed {
                            resolve_all_links(transaction, config.link_resolution)?;
                        } else {
                            resolve_changed_links(
                                transaction,
                                config.link_resolution,
                                &result.changed_document_ids,
                            )?;
                        }
                    }
                    if result.requires_fts_rebuild {
                        rebuild_fts_index(transaction)?;
                        restore_fts_triggers(transaction)?;
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
                    Ok(result)
                },
            )?;
            let has_changes = result.summary.added > 0
                || result.summary.updated > 0
                || result.summary.deleted > 0;
            if has_changes {
                crate::history::record_scan_checkpoint_incremental(
                    database.connection(),
                    &result.changed_document_ids,
                )?;
            }
            result.summary
        }
    };
    Ok(summary)
}

/// File that needs I/O work during incremental scan.
struct IncrementalWorkItem<'a> {
    file: &'a DiscoveredFile,
    cached: Option<&'a CachedDocument>,
}

/// Result of preparing an incremental file (parallel phase).
enum IncrementalPrepResult {
    /// Content hash unchanged — only mtime/size metadata needs updating.
    MetadataOnly { cached_id: String },
    /// Needs full reindex (content or parser version changed, or new file).
    Reindex {
        id: String,
        content_hash: Vec<u8>,
        derived: PreparedDerivedContent,
        is_new: bool,
    },
}

fn prepare_incremental_file(
    file: &DiscoveredFile,
    config: &crate::VaultConfig,
    cached: Option<&CachedDocument>,
) -> Result<IncrementalPrepResult, ScanError> {
    let current_version = document_index_version(file.kind, config);

    if let Some(cached) = cached {
        // Determine hash: reuse cached if mtime+size unchanged, otherwise read+hash.
        let (hash, content_bytes) =
            if cached.file_size == file.file_size && cached.file_mtime == file.file_mtime {
                (cached.content_hash.clone(), None)
            } else {
                let bytes = fs::read(&file.absolute_path)?;
                let hash = blake3::hash(&bytes).as_bytes().to_vec();
                (hash, Some(bytes))
            };

        let needs_reindex = hash != cached.content_hash || cached.parser_version != current_version;

        if !needs_reindex {
            return Ok(IncrementalPrepResult::MetadataOnly {
                cached_id: cached.id.clone(),
            });
        }

        let derived = prepare_derived_content(file, config, content_bytes)?;
        Ok(IncrementalPrepResult::Reindex {
            id: cached.id.clone(),
            content_hash: hash,
            derived,
            is_new: false,
        })
    } else {
        let bytes = fs::read(&file.absolute_path)?;
        let content_hash = blake3::hash(&bytes).as_bytes().to_vec();
        let derived = prepare_derived_content(file, config, Some(bytes))?;
        Ok(IncrementalPrepResult::Reindex {
            id: Ulid::new().to_string(),
            content_hash,
            derived,
            is_new: true,
        })
    }
}

fn prepare_derived_content(
    file: &DiscoveredFile,
    config: &crate::VaultConfig,
    content_bytes: Option<Vec<u8>>,
) -> Result<PreparedDerivedContent, ScanError> {
    match file.kind {
        DocumentKind::Note => {
            let bytes = match content_bytes {
                Some(b) => b,
                None => fs::read(&file.absolute_path)?,
            };
            let source = decode_note_source(bytes, &file.absolute_path)?;
            Ok(PreparedDerivedContent::Note(PreparedNoteContent {
                parsed: Box::new(parse_document(&source, config)),
                source,
            }))
        }
        DocumentKind::Attachment => Ok(PreparedDerivedContent::Attachment(
            extract_attachment_chunks(config, &file.absolute_path, &file.relative_path)?,
        )),
        DocumentKind::Base => Ok(PreparedDerivedContent::None),
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
        requires_link_resolution: false,
        target_pool_changed: false,
        changed_document_ids: Vec::new(),
        requires_property_catalog_refresh: false,
        requires_fts_rebuild: false,
    };
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

    // Phase 1: Classify files into "unchanged" (skip) vs "needs work".
    // Precompute version hashes to avoid recomputing per file.
    let note_version = document_index_version(DocumentKind::Note, config);
    let attachment_version = document_index_version(DocumentKind::Attachment, config);
    let base_version = document_index_version(DocumentKind::Base, config);

    let mut work_items: Vec<IncrementalWorkItem<'_>> = Vec::new();
    for file in discovered {
        let expected_version = match file.kind {
            DocumentKind::Note => note_version,
            DocumentKind::Attachment => attachment_version,
            DocumentKind::Base => base_version,
        };
        match existing.get(&file.relative_path) {
            Some(cached)
                if cached.file_size == file.file_size
                    && cached.file_mtime == file.file_mtime
                    && cached.parser_version == expected_version =>
            {
                result.summary.unchanged += 1;
            }
            Some(cached) => {
                work_items.push(IncrementalWorkItem {
                    file,
                    cached: Some(cached),
                });
            }
            None => {
                work_items.push(IncrementalWorkItem { file, cached: None });
            }
        }
    }

    // Phase 2: Prepare files needing work in parallel (read + hash + parse).
    let batch_size = full_scan_prepare_batch_size();
    let mut prepared_results: Vec<IncrementalPrepResult> = Vec::with_capacity(work_items.len());
    for batch in work_items.chunks(batch_size) {
        let mut batch_results = batch
            .par_iter()
            .map(|item| prepare_incremental_file(item.file, config, item.cached))
            .collect::<Result<Vec<_>, _>>()?;
        prepared_results.append(&mut batch_results);
    }

    // Phase 3: Apply all changes sequentially within the transaction.
    // For bulk changes, drop FTS triggers and rebuild the index in one pass at the end.
    // For small changes, keep triggers active so FTS updates happen incrementally per row.
    let total_changes = work_items.len() + deleted_paths.len();
    if total_changes >= FTS_BULK_THRESHOLD {
        drop_fts_triggers(transaction)?;
        result.requires_fts_rebuild = true;
    }
    emit_scan_progress(
        on_progress,
        ScanProgress {
            mode,
            phase: ScanPhase::ScanningFiles,
            discovered: discovered.len(),
            processed: result.summary.unchanged,
            added: 0,
            updated: 0,
            unchanged: result.summary.unchanged,
            deleted: 0,
        },
    );

    for (item, prep) in work_items.iter().zip(prepared_results) {
        match prep {
            IncrementalPrepResult::MetadataOnly { cached_id } => {
                update_document_metadata(transaction, &cached_id, item.file)?;
                result.summary.unchanged += 1;
            }
            IncrementalPrepResult::Reindex {
                id,
                content_hash,
                derived,
                is_new,
            } => {
                let current_version = document_index_version(item.file.kind, config);
                insert_or_update_document(
                    transaction,
                    &id,
                    item.file,
                    &content_hash,
                    match &derived {
                        PreparedDerivedContent::Note(note) => {
                            note.parsed.raw_frontmatter.as_deref()
                        }
                        PreparedDerivedContent::Attachment(_) | PreparedDerivedContent::None => {
                            None
                        }
                    },
                    current_version,
                )?;
                match &derived {
                    PreparedDerivedContent::Note(note) => {
                        replace_derived_rows(
                            transaction,
                            &id,
                            &item.file.filename,
                            is_new,
                            config,
                            note.source.as_str(),
                            &note.parsed,
                        )?;
                        result.requires_link_resolution = true;
                        result.requires_property_catalog_refresh = true;
                    }
                    PreparedDerivedContent::Attachment(chunks) => {
                        replace_attachment_rows(
                            transaction,
                            &id,
                            &item.file.filename,
                            is_new,
                            chunks,
                        )?;
                    }
                    PreparedDerivedContent::None => {}
                }
                result.changed_document_ids.push(id);
                if is_new {
                    result.summary.added += 1;
                    result.requires_link_resolution = true;
                    result.target_pool_changed = true;
                    result.requires_property_catalog_refresh |=
                        matches!(item.file.kind, DocumentKind::Note);
                } else {
                    result.summary.updated += 1;
                }
            }
        }

        emit_scan_progress(
            on_progress,
            ScanProgress {
                mode,
                phase: ScanPhase::ScanningFiles,
                discovered: discovered.len(),
                processed: result.summary.unchanged + result.summary.added + result.summary.updated,
                added: result.summary.added,
                updated: result.summary.updated,
                unchanged: result.summary.unchanged,
                deleted: result.summary.deleted,
            },
        );
    }

    for path in deleted_paths {
        if let Some(cached) = existing.get(path) {
            result.changed_document_ids.push(cached.id.clone());
            delete_document(transaction, &cached.id)?;
            result.requires_link_resolution = true;
            result.target_pool_changed = true;
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

    Ok(result)
}

fn emit_scan_progress(on_progress: &mut impl FnMut(ScanProgress), progress: ScanProgress) {
    on_progress(progress);
}

#[allow(clippy::too_many_lines)]
fn discover_files(vault_root: &Path) -> Result<Vec<DiscoveredFile>, ScanError> {
    let mut builder = WalkBuilder::new(vault_root);
    builder.hidden(true);
    builder.git_ignore(true);
    builder.git_global(false);
    builder.git_exclude(false);
    builder.parents(false);
    builder.require_git(false);

    let files = std::sync::Mutex::new(Vec::new());
    let first_error = std::sync::Mutex::new(None::<ScanError>);

    builder.build_parallel().run(|| {
        let files = &files;
        let first_error = &first_error;
        Box::new(move |entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    let mut guard = first_error
                        .lock()
                        .expect("error lock should not be poisoned");
                    if guard.is_none() {
                        *guard = Some(ScanError::Ignore(err));
                    }
                    return ignore::WalkState::Continue;
                }
            };
            let Some(file_type) = entry.file_type() else {
                return ignore::WalkState::Continue;
            };
            if !file_type.is_file() {
                return ignore::WalkState::Continue;
            }

            let path = entry.path();
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => match fs::metadata(path) {
                    Ok(m) => m,
                    Err(err) => {
                        let mut guard = first_error
                            .lock()
                            .expect("error lock should not be poisoned");
                        if guard.is_none() {
                            *guard = Some(ScanError::Io(err));
                        }
                        return ignore::WalkState::Continue;
                    }
                },
            };

            let relative_path = normalize_relative_path(
                path.strip_prefix(vault_root)
                    .expect("walked paths should always be inside the vault root"),
            );
            let filename = match path
                .file_stem()
                .or_else(|| path.file_name())
                .and_then(|value| value.to_str())
            {
                Some(name) => name.to_string(),
                None => return ignore::WalkState::Continue,
            };
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();

            let Ok(file_size) = i64::try_from(metadata.len()) else {
                return ignore::WalkState::Continue;
            };
            let file_mtime = match metadata
                .modified()
                .map_err(ScanError::Io)
                .and_then(|mtime| system_time_to_millis(mtime, path))
            {
                Ok(mtime) => mtime,
                Err(err) => {
                    let mut guard = first_error
                        .lock()
                        .expect("error lock should not be poisoned");
                    if guard.is_none() {
                        *guard = Some(err);
                    }
                    return ignore::WalkState::Continue;
                }
            };

            files
                .lock()
                .expect("files lock should not be poisoned")
                .push(DiscoveredFile {
                    absolute_path: path.to_path_buf(),
                    relative_path,
                    filename,
                    extension,
                    kind: detect_document_kind(path),
                    file_size,
                    file_mtime,
                });

            ignore::WalkState::Continue
        })
    });

    if let Some(err) = first_error
        .into_inner()
        .expect("error lock should not be poisoned")
    {
        return Err(err);
    }

    let mut files = files
        .into_inner()
        .expect("files lock should not be poisoned");
    files.sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
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

#[cfg(test)]
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
    let derived =
        match file.kind {
            DocumentKind::Note => {
                let source = decode_note_source(bytes, &file.absolute_path)?;
                PreparedDerivedContent::Note(PreparedNoteContent {
                    parsed: Box::new(parse_document(&source, config)),
                    source,
                })
            }
            DocumentKind::Attachment => PreparedDerivedContent::Attachment(
                extract_attachment_chunks(config, &file.absolute_path, &file.relative_path)?,
            ),
            DocumentKind::Base => PreparedDerivedContent::None,
        };

    Ok(PreparedFullScanDocument {
        file: file.clone(),
        content_hash,
        derived,
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
    parser_version: u32,
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
            parser_version,
            current_timestamp()?,
        ],
    )?;

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
        match &prepared.derived {
            PreparedDerivedContent::Note(note) => note.parsed.raw_frontmatter.as_deref(),
            PreparedDerivedContent::Attachment(_) | PreparedDerivedContent::None => None,
        },
        document_index_version(prepared.file.kind, config),
    )?;
    match &prepared.derived {
        PreparedDerivedContent::Note(note) => {
            replace_derived_rows(
                transaction,
                id,
                &prepared.file.filename,
                true,
                config,
                note.source.as_str(),
                &note.parsed,
            )?;
        }
        PreparedDerivedContent::Attachment(chunks) => {
            replace_attachment_rows(transaction, id, &prepared.file.filename, true, chunks)?;
        }
        PreparedDerivedContent::None => {}
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
    document_title: &str,
    is_new: bool,
    config: &crate::VaultConfig,
    source: &str,
    parsed: &ParsedDocument,
) -> Result<(), ScanError> {
    let reusable_chunk_ids = if is_new {
        HashMap::new()
    } else {
        load_reusable_chunk_ids(transaction, document_id)?
    };
    if !is_new {
        clear_derived_rows(transaction, document_id)?;
    }
    insert_headings(transaction, document_id, &parsed.headings)?;
    insert_block_refs(transaction, document_id, &parsed.block_refs)?;
    insert_links(transaction, document_id, &parsed.links)?;
    insert_aliases(transaction, document_id, &parsed.aliases)?;
    insert_tags(transaction, document_id, &parsed.tags)?;
    let aliases_text = parsed.aliases.join(" ");
    insert_chunks_with_search(
        transaction,
        document_id,
        document_title,
        &aliases_text,
        &parsed.chunk_texts,
        reusable_chunk_ids,
    )?;
    insert_diagnostics(transaction, document_id, &parsed.diagnostics)?;
    if let Some(properties) = extract_indexed_properties(parsed, config)
        .map_err(|error| ScanError::Io(std::io::Error::other(error)))?
    {
        insert_properties(transaction, document_id, &properties)?;
        insert_property_values(transaction, document_id, &properties)?;
        insert_property_list_items(transaction, document_id, &properties)?;
        insert_property_diagnostics(transaction, document_id, &properties)?;
    }
    let list_item_ids = insert_list_items(transaction, document_id, &parsed.list_items)?;
    insert_tasks(
        transaction,
        document_id,
        &parsed.tasks,
        &list_item_ids,
        config,
    )?;
    insert_kanban_board(transaction, document_id, parsed, source, config)?;
    insert_dataview_blocks(transaction, document_id, &parsed.dataview_blocks)?;
    insert_tasks_blocks(transaction, document_id, &parsed.tasks_blocks)?;
    insert_inline_expressions(transaction, document_id, &parsed.inline_expressions)?;
    Ok(())
}

fn replace_attachment_rows(
    transaction: &Transaction<'_>,
    document_id: &str,
    document_title: &str,
    is_new: bool,
    chunks: &[crate::ChunkText],
) -> Result<(), ScanError> {
    let reusable_chunk_ids = if is_new {
        HashMap::new()
    } else {
        load_reusable_chunk_ids(transaction, document_id)?
    };
    if !is_new {
        clear_derived_rows(transaction, document_id)?;
    }
    insert_chunks_with_search(
        transaction,
        document_id,
        document_title,
        "",
        chunks,
        reusable_chunk_ids,
    )?;
    Ok(())
}

fn insert_chunks_with_search(
    transaction: &Transaction<'_>,
    document_id: &str,
    document_title: &str,
    aliases: &str,
    chunks: &[crate::ChunkText],
    mut reusable_chunk_ids: HashMap<ChunkReuseKey, VecDeque<String>>,
) -> Result<(), ScanError> {
    // First pass: insert chunks, collecting IDs and heading paths for search rows.
    let mut chunk_ids_and_headings = Vec::with_capacity(chunks.len());
    {
        let mut statement = transaction.prepare_cached(
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
        )?;
        for chunk in chunks {
            let chunk_id = reusable_chunk_ids
                .get_mut(&chunk_reuse_key(chunk))
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| Ulid::new().to_string());
            let heading_path_json = serde_json::to_string(&chunk.heading_path)
                .map_err(|error| ScanError::Io(std::io::Error::other(error)))?;
            statement.execute(params![
                chunk_id,
                document_id,
                i64::try_from(chunk.sequence_index).map_err(|_| ScanError::MetadataOverflow {
                    field: "chunk.sequence_index",
                    path: PathBuf::from(document_id),
                })?,
                &heading_path_json,
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
            ])?;
            chunk_ids_and_headings.push((chunk_id, heading_path_json));
        }
    }
    // Second pass: insert search rows using in-memory data (no SELECT needed).
    {
        let mut statement = transaction.prepare_cached(
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
        )?;
        for (chunk, (chunk_id, heading_path_json)) in chunks.iter().zip(&chunk_ids_and_headings) {
            statement.execute(params![
                chunk_id,
                document_id,
                &chunk.content,
                document_title,
                aliases,
                flatten_heading_path(heading_path_json)?,
            ])?;
        }
    }

    Ok(())
}

fn insert_headings(
    transaction: &Transaction<'_>,
    document_id: &str,
    headings: &[crate::RawHeading],
) -> Result<(), ScanError> {
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO headings (id, document_id, level, text, byte_offset)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ",
    )?;
    for heading in headings {
        statement.execute(params![
            Ulid::new().to_string(),
            document_id,
            i64::from(heading.level),
            &heading.text,
            i64::try_from(heading.byte_offset).map_err(|_| ScanError::MetadataOverflow {
                field: "heading.byte_offset",
                path: PathBuf::from(document_id),
            })?,
        ])?;
    }
    Ok(())
}

fn insert_block_refs(
    transaction: &Transaction<'_>,
    document_id: &str,
    block_refs: &[crate::RawBlockRef],
) -> Result<(), ScanError> {
    let mut statement = transaction.prepare_cached(
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
    )?;
    for block_ref in block_refs {
        statement.execute(params![
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
        ])?;
    }
    Ok(())
}

fn insert_links(
    transaction: &Transaction<'_>,
    document_id: &str,
    links: &[crate::RawLink],
) -> Result<(), ScanError> {
    let mut statement = transaction.prepare_cached(
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
    )?;
    for link in links {
        statement.execute(params![
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
        ])?;
    }
    Ok(())
}

fn insert_aliases(
    transaction: &Transaction<'_>,
    document_id: &str,
    aliases: &[String],
) -> Result<(), ScanError> {
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO aliases (id, document_id, alias_text)
        VALUES (?1, ?2, ?3)
        ",
    )?;
    for alias in aliases {
        statement.execute(params![Ulid::new().to_string(), document_id, alias])?;
    }
    Ok(())
}

fn insert_tags(
    transaction: &Transaction<'_>,
    document_id: &str,
    tags: &[crate::RawTag],
) -> Result<(), ScanError> {
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO tags (id, document_id, tag_text)
        VALUES (?1, ?2, ?3)
        ",
    )?;
    for tag in tags {
        statement.execute(params![Ulid::new().to_string(), document_id, &tag.tag_text])?;
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
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ",
    )?;
    for diagnostic in diagnostics {
        statement.execute(params![
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
        ])?;
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
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO property_values (
            id,
            document_id,
            key,
            value_text,
            value_number,
            value_bool,
            value_date,
            value_type,
            origin
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ",
    )?;
    for property in &properties.values {
        statement.execute(params![
            Ulid::new().to_string(),
            document_id,
            &property.key,
            property.value_text.as_deref(),
            property.value_number,
            property.value_bool.map(i64::from),
            property.value_date.as_deref(),
            &property.value_type,
            property.origin.as_str(),
        ])?;
    }
    Ok(())
}

fn insert_property_list_items(
    transaction: &Transaction<'_>,
    document_id: &str,
    properties: &IndexedProperties,
) -> Result<(), ScanError> {
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO property_list_items (document_id, key, item_index, value_text)
        VALUES (?1, ?2, ?3, ?4)
        ",
    )?;
    for item in &properties.list_items {
        statement.execute(params![
            document_id,
            &item.key,
            i64::try_from(item.item_index).map_err(|_| ScanError::MetadataOverflow {
                field: "property_list_items.item_index",
                path: PathBuf::from(document_id),
            })?,
            &item.value_text,
        ])?;
    }
    Ok(())
}

fn insert_property_diagnostics(
    transaction: &Transaction<'_>,
    document_id: &str,
    properties: &IndexedProperties,
) -> Result<(), ScanError> {
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
        VALUES (?1, ?2, 'type_mismatch', ?3, ?4, ?5)
        ",
    )?;
    for diagnostic in &properties.diagnostics {
        statement.execute(params![
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
        ])?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn insert_tasks(
    transaction: &Transaction<'_>,
    document_id: &str,
    tasks: &[crate::RawTask],
    list_item_ids: &[String],
    config: &crate::VaultConfig,
) -> Result<(), ScanError> {
    if tasks.is_empty() {
        return Ok(());
    }

    let task_ids = tasks
        .iter()
        .map(|_| Ulid::new().to_string())
        .collect::<Vec<_>>();
    let mut task_statement = transaction.prepare_cached(
        "
        INSERT INTO tasks (
            id,
            document_id,
            list_item_id,
            status_char,
            text,
            byte_offset,
            parent_task_id,
            section_heading,
            line_number
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        ",
    )?;

    for (index, task) in tasks.iter().enumerate() {
        task_statement.execute(params![
            &task_ids[index],
            document_id,
            list_item_ids.get(task.list_item_index).ok_or_else(|| {
                ScanError::Io(std::io::Error::other(format!(
                    "missing list item id for task at index {}",
                    task.list_item_index
                )))
            })?,
            task.status_char.to_string(),
            &task.text,
            i64::try_from(task.byte_offset).map_err(|_| ScanError::MetadataOverflow {
                field: "tasks.byte_offset",
                path: PathBuf::from(document_id),
            })?,
            task.parent_task_index
                .and_then(|parent_index| task_ids.get(parent_index))
                .map(String::as_str),
            task.section_heading.as_deref(),
            i64::try_from(task.line_number).map_err(|_| ScanError::MetadataOverflow {
                field: "tasks.line_number",
                path: PathBuf::from(document_id),
            })?,
        ])?;
    }

    let mut property_statement = transaction.prepare_cached(
        "
        INSERT INTO task_properties (
            id,
            task_id,
            key,
            value_text,
            value_number,
            value_bool,
            value_date,
            value_type
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ",
    )?;

    for (task_id, task) in task_ids.iter().zip(tasks) {
        let mut source_properties = Map::new();

        for inline_field in &task.inline_fields {
            insert_task_property_text(
                &mut property_statement,
                task_id,
                &inline_field.key,
                &inline_field.value_text,
                inline_field.kind,
                config,
            )?;
            source_properties
                .entry(inline_field.key.clone())
                .or_insert_with(|| Value::String(inline_field.value_text.clone()));
        }

        for (key, value_text) in extract_task_text_properties(&task.text) {
            insert_task_property_text(
                &mut property_statement,
                task_id,
                &key,
                &value_text,
                crate::parser::types::InlineFieldKind::Bare,
                config,
            )?;
            source_properties
                .entry(key)
                .or_insert_with(|| Value::String(value_text));
        }

        for (key, value) in task_recurrence_properties(&source_properties) {
            if source_properties.contains_key(&key) {
                continue;
            }
            insert_task_property_json_value(
                &mut property_statement,
                task_id,
                &key,
                &value,
                config,
            )?;
        }
    }

    Ok(())
}

fn insert_task_property_text(
    statement: &mut rusqlite::Statement<'_>,
    task_id: &str,
    key: &str,
    value_text: &str,
    kind: crate::parser::types::InlineFieldKind,
    config: &crate::VaultConfig,
) -> Result<(), ScanError> {
    let expected_type = config
        .property_types
        .get(key)
        .map(String::as_str)
        .map(crate::properties::canonical_property_type);
    let value = indexed_inline_property_value(key, value_text, kind, config, expected_type);
    statement.execute(params![
        Ulid::new().to_string(),
        task_id,
        key,
        value.value_text.as_deref(),
        value.value_number,
        value.value_bool.map(i64::from),
        value.value_date.as_deref(),
        value.value_type,
    ])?;
    Ok(())
}

fn insert_task_property_json_value(
    statement: &mut rusqlite::Statement<'_>,
    task_id: &str,
    key: &str,
    value: &Value,
    config: &crate::VaultConfig,
) -> Result<(), ScanError> {
    match value {
        Value::Array(values) => {
            for entry in values {
                insert_task_property_json_value(statement, task_id, key, entry, config)?;
            }
        }
        Value::String(text) => insert_task_property_text(
            statement,
            task_id,
            key,
            text,
            crate::parser::types::InlineFieldKind::Bare,
            config,
        )?,
        Value::Number(number) => insert_task_property_text(
            statement,
            task_id,
            key,
            &number.to_string(),
            crate::parser::types::InlineFieldKind::Bare,
            config,
        )?,
        Value::Bool(value_bool) => insert_task_property_text(
            statement,
            task_id,
            key,
            if *value_bool { "true" } else { "false" },
            crate::parser::types::InlineFieldKind::Bare,
            config,
        )?,
        Value::Null | Value::Object(_) => {}
    }

    Ok(())
}

fn extract_task_text_properties(text: &str) -> Vec<(String, String)> {
    let mut properties = Vec::new();

    for (key, markers) in [
        ("due", &["🗓️", "🗓"][..]),
        ("completion", &["✅"][..]),
        ("created", &["➕"][..]),
        ("start", &["🛫"][..]),
        ("scheduled", &["⏳"][..]),
    ] {
        if let Some(value) = extract_task_marker_token(text, markers) {
            properties.push((key.to_string(), value));
        }
    }

    for (marker, value) in [
        ("⏫", "highest"),
        ("🔺", "high"),
        ("🔼", "medium"),
        ("🔽", "low"),
        ("⏬", "lowest"),
    ] {
        if text.contains(marker) {
            properties.push(("priority".to_string(), value.to_string()));
            break;
        }
    }

    if let Some(value) = extract_task_marker_segment(text, "🔁") {
        properties.push(("recurrence".to_string(), value));
    }
    if let Some(value) = extract_task_marker_token(text, &["⛔"]) {
        properties.push(("blocked-by".to_string(), value));
    }
    if let Some(value) = extract_task_marker_token(text, &["🆔"]) {
        properties.push(("id".to_string(), value));
    }

    properties
}

fn extract_task_marker_token(text: &str, markers: &[&str]) -> Option<String> {
    markers.iter().find_map(|marker| {
        text.find(marker).and_then(|index| {
            text[index + marker.len()..]
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
    })
}

fn extract_task_marker_segment(text: &str, marker: &str) -> Option<String> {
    let index = text.find(marker)?;
    let remainder = text[index + marker.len()..].trim_start();
    let end = task_annotation_markers()
        .iter()
        .filter_map(|candidate| remainder.find(candidate))
        .min()
        .unwrap_or(remainder.len());
    let value = remainder[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn task_annotation_markers() -> &'static [&'static str] {
    &[
        "🗓️", "🗓", "✅", "➕", "🛫", "⏳", "⏫", "🔺", "🔼", "🔽", "⏬", "🔁", "⛔", "🆔",
    ]
}

fn insert_list_items(
    transaction: &Transaction<'_>,
    document_id: &str,
    list_items: &[crate::RawListItem],
) -> Result<Vec<String>, ScanError> {
    if list_items.is_empty() {
        return Ok(Vec::new());
    }

    let list_item_ids = list_items
        .iter()
        .map(|_| Ulid::new().to_string())
        .collect::<Vec<_>>();
    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO list_items (
            id,
            document_id,
            text,
            tags_json,
            outlinks_json,
            line_number,
            line_count,
            byte_offset,
            section_heading,
            parent_item_id,
            is_task,
            block_id,
            annotated,
            symbol
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
        ",
    )?;

    for (index, list_item) in list_items.iter().enumerate() {
        statement.execute(params![
            &list_item_ids[index],
            document_id,
            &list_item.text,
            serde_json::to_string(&list_item.tags)
                .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
            serde_json::to_string(&list_item.outlinks)
                .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
            i64::try_from(list_item.line_number).map_err(|_| ScanError::MetadataOverflow {
                field: "list_items.line_number",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(list_item.line_count).map_err(|_| ScanError::MetadataOverflow {
                field: "list_items.line_count",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(list_item.byte_offset).map_err(|_| ScanError::MetadataOverflow {
                field: "list_items.byte_offset",
                path: PathBuf::from(document_id),
            })?,
            list_item.section_heading.as_deref(),
            list_item
                .parent_item_index
                .and_then(|parent_index| list_item_ids.get(parent_index))
                .map(String::as_str),
            i64::from(list_item.is_task),
            list_item.block_id.as_deref(),
            i64::from(list_item.annotated),
            &list_item.symbol,
        ])?;
    }

    Ok(list_item_ids)
}

fn insert_dataview_blocks(
    transaction: &Transaction<'_>,
    document_id: &str,
    blocks: &[crate::RawDataviewBlock],
) -> Result<(), ScanError> {
    if blocks.is_empty() {
        return Ok(());
    }

    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO dataview_blocks (
            id,
            document_id,
            language,
            block_index,
            byte_offset_start,
            byte_offset_end,
            line_number,
            raw_text
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ",
    )?;
    for block in blocks {
        statement.execute(params![
            Ulid::new().to_string(),
            document_id,
            &block.language,
            i64::try_from(block.block_index).map_err(|_| ScanError::MetadataOverflow {
                field: "dataview_blocks.block_index",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(block.byte_range.start).map_err(|_| ScanError::MetadataOverflow {
                field: "dataview_blocks.byte_offset_start",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(block.byte_range.end).map_err(|_| ScanError::MetadataOverflow {
                field: "dataview_blocks.byte_offset_end",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(block.line_number).map_err(|_| ScanError::MetadataOverflow {
                field: "dataview_blocks.line_number",
                path: PathBuf::from(document_id),
            })?,
            &block.text,
        ])?;
    }
    Ok(())
}

fn insert_kanban_board(
    transaction: &Transaction<'_>,
    document_id: &str,
    parsed: &ParsedDocument,
    source: &str,
    config: &crate::VaultConfig,
) -> Result<(), ScanError> {
    let Some(board) = crate::kanban::extract_indexed_board(parsed, source, config) else {
        return Ok(());
    };

    transaction.execute(
        "
        INSERT INTO kanban_boards (
            document_id,
            format,
            settings_json,
            date_trigger,
            time_trigger
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        ",
        params![
            document_id,
            board.format,
            serde_json::to_string(&Value::Object(board.settings))
                .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
            board.date_trigger,
            board.time_trigger,
        ],
    )?;

    Ok(())
}

fn insert_tasks_blocks(
    transaction: &Transaction<'_>,
    document_id: &str,
    blocks: &[crate::RawTasksBlock],
) -> Result<(), ScanError> {
    if blocks.is_empty() {
        return Ok(());
    }

    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO tasks_blocks (
            id,
            document_id,
            block_index,
            byte_offset_start,
            byte_offset_end,
            line_number,
            raw_text
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ",
    )?;
    for block in blocks {
        statement.execute(params![
            Ulid::new().to_string(),
            document_id,
            i64::try_from(block.block_index).map_err(|_| ScanError::MetadataOverflow {
                field: "tasks_blocks.block_index",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(block.byte_range.start).map_err(|_| ScanError::MetadataOverflow {
                field: "tasks_blocks.byte_offset_start",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(block.byte_range.end).map_err(|_| ScanError::MetadataOverflow {
                field: "tasks_blocks.byte_offset_end",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(block.line_number).map_err(|_| ScanError::MetadataOverflow {
                field: "tasks_blocks.line_number",
                path: PathBuf::from(document_id),
            })?,
            &block.text,
        ])?;
    }
    Ok(())
}

fn insert_inline_expressions(
    transaction: &Transaction<'_>,
    document_id: &str,
    expressions: &[crate::RawInlineExpression],
) -> Result<(), ScanError> {
    if expressions.is_empty() {
        return Ok(());
    }

    let mut statement = transaction.prepare_cached(
        "
        INSERT INTO inline_expressions (
            id,
            document_id,
            expression,
            byte_offset_start,
            byte_offset_end,
            line_number
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ",
    )?;
    for expression in expressions {
        statement.execute(params![
            Ulid::new().to_string(),
            document_id,
            &expression.expression,
            i64::try_from(expression.byte_range.start).map_err(|_| {
                ScanError::MetadataOverflow {
                    field: "inline_expressions.byte_offset_start",
                    path: PathBuf::from(document_id),
                }
            })?,
            i64::try_from(expression.byte_range.end).map_err(|_| ScanError::MetadataOverflow {
                field: "inline_expressions.byte_offset_end",
                path: PathBuf::from(document_id),
            })?,
            i64::try_from(expression.line_number).map_err(|_| ScanError::MetadataOverflow {
                field: "inline_expressions.line_number",
                path: PathBuf::from(document_id),
            })?,
        ])?;
    }
    Ok(())
}

fn clear_derived_rows(
    transaction: &Transaction<'_>,
    document_id: &str,
) -> Result<(), rusqlite::Error> {
    // Static SQL strings so prepare_cached can reuse them across calls.
    static CLEAR_STATEMENTS: &[&str] = &[
        "DELETE FROM kanban_boards WHERE document_id = ?1",
        "DELETE FROM task_properties WHERE task_id IN (SELECT id FROM tasks WHERE document_id = ?1)",
        "DELETE FROM tasks WHERE document_id = ?1",
        "DELETE FROM list_items WHERE document_id = ?1",
        "DELETE FROM dataview_blocks WHERE document_id = ?1",
        "DELETE FROM tasks_blocks WHERE document_id = ?1",
        "DELETE FROM inline_expressions WHERE document_id = ?1",
        "DELETE FROM headings WHERE document_id = ?1",
        "DELETE FROM block_refs WHERE document_id = ?1",
        "DELETE FROM links WHERE source_document_id = ?1",
        "DELETE FROM aliases WHERE document_id = ?1",
        "DELETE FROM tags WHERE document_id = ?1",
        "DELETE FROM properties WHERE document_id = ?1",
        "DELETE FROM property_values WHERE document_id = ?1",
        "DELETE FROM property_list_items WHERE document_id = ?1",
        "DELETE FROM search_chunk_content WHERE document_id = ?1",
        "DELETE FROM chunks WHERE document_id = ?1",
        "DELETE FROM diagnostics WHERE document_id = ?1",
    ];
    for sql in CLEAR_STATEMENTS {
        transaction.prepare_cached(sql)?.execute([document_id])?;
    }
    Ok(())
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
    let index = ResolverIndex::build(&documents);

    let mut update_statement =
        transaction.prepare_cached("UPDATE links SET resolved_target_id = ?2 WHERE id = ?1")?;
    let mut diag_statement = transaction.prepare_cached(
        "
        INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
        VALUES (?1, ?2, 'unresolved_link', ?3, ?4, ?5)
        ",
    )?;
    let timestamp = current_timestamp()?;
    for link in &links {
        let resolution = index.resolve(&link.resolver_link, mode);
        // Only UPDATE links that actually resolved — unresolved and external links
        // already have NULL from the INSERT, so writing NULL again is wasted work.
        if resolution.resolved_target_id.is_some() {
            update_statement.execute(params![link.id, resolution.resolved_target_id])?;
        }

        if let Some(problem) = resolution.problem {
            diag_statement.execute(params![
                Ulid::new().to_string(),
                link.resolver_link.source_document_id,
                resolution_problem_message(&problem, &link.resolver_link),
                serde_json::to_string(&resolution_problem_detail(&problem, &link.resolver_link))
                    .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
                &timestamp,
            ])?;
        }
    }

    Ok(())
}

/// Resolve only links originating from the given document IDs.
/// Used when documents were updated but no documents were added/deleted
/// (so the target pool is unchanged and only source links need re-resolution).
fn resolve_changed_links(
    transaction: &Transaction<'_>,
    mode: crate::LinkResolutionMode,
    changed_document_ids: &[String],
) -> Result<(), ScanError> {
    if changed_document_ids.is_empty() {
        return Ok(());
    }

    // Delete diagnostics only for the changed documents.
    let mut delete_diag_statement = transaction.prepare_cached(
        "DELETE FROM diagnostics WHERE document_id = ?1 AND kind = 'unresolved_link'",
    )?;
    for doc_id in changed_document_ids {
        delete_diag_statement.execute([doc_id])?;
    }

    // Build the full resolver index (needed to resolve against all targets).
    let documents = load_resolver_documents(transaction)?;
    let index = ResolverIndex::build(&documents);

    // Load only links from changed documents.
    let placeholders = changed_document_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "
        SELECT l.id, l.source_document_id, s.path, l.target_path_candidate, l.link_kind
        FROM links l
        JOIN documents s ON s.id = l.source_document_id
        WHERE l.source_document_id IN ({placeholders})
        ORDER BY l.byte_offset
        "
    );
    let mut statement = transaction.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(changed_document_ids), |row| {
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
    let links = rows.collect::<Result<Vec<_>, _>>()?;

    let mut update_statement =
        transaction.prepare_cached("UPDATE links SET resolved_target_id = ?2 WHERE id = ?1")?;
    let mut diag_statement = transaction.prepare_cached(
        "
        INSERT INTO diagnostics (id, document_id, kind, message, detail, created_at)
        VALUES (?1, ?2, 'unresolved_link', ?3, ?4, ?5)
        ",
    )?;
    let timestamp = current_timestamp()?;
    for link in &links {
        let resolution = index.resolve(&link.resolver_link, mode);
        if resolution.resolved_target_id.is_some() {
            update_statement.execute(params![link.id, resolution.resolved_target_id])?;
        }
        if let Some(problem) = resolution.problem {
            diag_statement.execute(params![
                Ulid::new().to_string(),
                link.resolver_link.source_document_id,
                resolution_problem_message(&problem, &link.resolver_link),
                serde_json::to_string(&resolution_problem_detail(&problem, &link.resolver_link))
                    .map_err(|error| ScanError::Io(std::io::Error::other(error)))?,
                &timestamp,
            ])?;
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
        ParseDiagnosticKind::HtmlLink
        | ParseDiagnosticKind::LinkInComment
        | ParseDiagnosticKind::UnsupportedSyntax => "unsupported_syntax",
    }
}

fn current_timestamp() -> Result<String, ScanError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .to_string())
}

fn document_index_version(kind: DocumentKind, config: &crate::VaultConfig) -> u32 {
    match kind {
        DocumentKind::Attachment => attachment_index_version(config),
        DocumentKind::Note | DocumentKind::Base => PARSER_VERSION,
    }
}

fn attachment_index_version(config: &crate::VaultConfig) -> u32 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&crate::EXTRACTION_VERSION.to_le_bytes());
    if let Some(extraction) = config.extraction.as_ref() {
        let serialized =
            serde_json::to_vec(extraction).expect("attachment extraction config should serialize");
        hasher.update(&serialized);
    } else {
        hasher.update(b"disabled");
    }
    let digest = hasher.finalize();
    u32::from_le_bytes(
        digest.as_bytes()[..4]
            .try_into()
            .expect("digest should contain four bytes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        load_vault_config, AttachmentExtractionConfig, LinkResolutionMode, LinkStylePreference,
    };
    use crate::expression::eval::{evaluate, EvalContext};
    use crate::expression::parse::Parser;
    use crate::file_metadata::FileMetadataResolver;
    use crate::properties::load_note_index;
    use crate::{search_vault, SearchQuery};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    type DashboardListItemRow = (
        String,
        i64,
        Option<String>,
        i64,
        i64,
        String,
        String,
        String,
    );
    type DashboardPropertyRow = (
        String,
        String,
        Option<String>,
        Option<f64>,
        Option<i64>,
        Option<String>,
        String,
    );

    fn assert_dataview_fixture_row_counts(connection: &rusqlite::Connection) {
        assert_eq!(count_rows(connection, "list_items"), 6);
        assert_eq!(count_rows(connection, "tasks"), 4);
        assert_eq!(count_rows(connection, "task_properties"), 17);
        assert_eq!(count_rows(connection, "dataview_blocks"), 2);
        assert_eq!(count_rows(connection, "tasks_blocks"), 0);
        assert_eq!(count_rows(connection, "inline_expressions"), 1);
    }

    fn assert_beta_task_properties(connection: &rusqlite::Connection) {
        let beta_task_properties: Vec<(String, String)> = connection
            .prepare(
                "
                SELECT task_properties.key, COALESCE(task_properties.value_text, task_properties.value_date, CAST(task_properties.value_number AS TEXT))
                FROM task_properties
                JOIN tasks ON tasks.id = task_properties.task_id
                JOIN documents ON documents.id = tasks.document_id
                WHERE documents.path = 'Projects/Beta.md' AND tasks.line_number = 9
                ORDER BY task_properties.key, task_properties.value_text, task_properties.value_date, task_properties.value_number
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows should collect");
        assert!(
            beta_task_properties
                .iter()
                .any(|(key, value)| key == "recurrenceRule" && value == "FREQ=WEEKLY;INTERVAL=1"),
            "expected recurrenceRule in task_properties: {beta_task_properties:?}"
        );
        assert!(
            beta_task_properties
                .iter()
                .any(|(key, value)| key == "recurrenceAnchor" && value == "2026-04-05"),
            "expected recurrenceAnchor in task_properties: {beta_task_properties:?}"
        );
    }

    fn assert_dashboard_dataview_blocks(connection: &rusqlite::Connection) {
        let dataview_blocks: Vec<(String, i64, String)> = connection
            .prepare(
                "
                SELECT dataview_blocks.language, dataview_blocks.block_index, dataview_blocks.raw_text
                FROM dataview_blocks
                JOIN documents ON documents.id = dataview_blocks.document_id
                WHERE documents.path = 'Dashboard.md'
                ORDER BY dataview_blocks.block_index
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows should collect");
        assert_eq!(
            dataview_blocks,
            vec![
                (
                    "dataview".to_string(),
                    0,
                    "TABLE status, priority\nFROM #project\nWHERE reviewed = true\nSORT file.name ASC"
                        .to_string(),
                ),
                (
                    "dataviewjs".to_string(),
                    1,
                    "dv.table([\"Status\"], [[dv.current().status]])".to_string(),
                ),
            ]
        );

        let inline_expressions: Vec<String> = connection
            .prepare(
                "
                SELECT inline_expressions.expression
                FROM inline_expressions
                JOIN documents ON documents.id = inline_expressions.document_id
                WHERE documents.path = 'Dashboard.md'
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| row.get(0))
            .expect("query should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows should collect");
        assert_eq!(inline_expressions, vec!["this.status".to_string()]);
    }

    fn load_dashboard_list_items(connection: &rusqlite::Connection) -> Vec<DashboardListItemRow> {
        connection
            .prepare(
                "
                SELECT list_items.text, list_items.is_task, list_items.block_id, list_items.annotated, list_items.line_number, list_items.symbol, list_items.tags_json, list_items.outlinks_json
                FROM list_items
                JOIN documents ON documents.id = list_items.document_id
                WHERE documents.path = 'Dashboard.md'
                ORDER BY list_items.line_number
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            })
            .expect("query should succeed")
            .map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn assert_dashboard_list_items(connection: &rusqlite::Connection) {
        let dashboard_list_items = load_dashboard_list_items(connection);
        assert_eq!(dashboard_list_items.len(), 4);
        assert_eq!(
            dashboard_list_items[0],
            (
                "Plain list item [[Projects/Alpha]] #project/list [kind:: note]".to_string(),
                0,
                None,
                1,
                19,
                "-".to_string(),
                "[\"#project/list\"]".to_string(),
                "[\"[[Projects/Alpha]]\"]".to_string(),
            )
        );
        assert_eq!(
            dashboard_list_items[1],
            (
                "Nested numbered item ^list-child".to_string(),
                0,
                Some("list-child".to_string()),
                0,
                20,
                "1.".to_string(),
                "[]".to_string(),
                "[]".to_string(),
            )
        );
    }

    fn load_dashboard_properties(connection: &rusqlite::Connection) -> Vec<DashboardPropertyRow> {
        connection
            .prepare(
                "
                SELECT key, origin, value_text, value_number, value_bool, value_date, value_type
                FROM property_values
                JOIN documents ON documents.id = property_values.document_id
                WHERE documents.path = 'Dashboard.md'
                ORDER BY key, origin
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .expect("query should succeed")
            .map(|row| row.expect("row should deserialize"))
            .collect()
    }

    fn assert_dashboard_text_properties(dashboard_properties: &[DashboardPropertyRow]) {
        assert!(dashboard_properties.contains(&(
            "🎅".to_string(),
            "inline_bracket".to_string(),
            Some("gifts".to_string()),
            None,
            None,
            None,
            "text".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "choices".to_string(),
            "inline".to_string(),
            None,
            None,
            None,
            None,
            "list".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "due date".to_string(),
            "inline".to_string(),
            None,
            None,
            None,
            Some("2026-04".to_string()),
            "date".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "duration".to_string(),
            "inline".to_string(),
            Some("1d 3h".to_string()),
            None,
            None,
            None,
            "duration".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "month".to_string(),
            "inline".to_string(),
            None,
            None,
            None,
            Some("2026-04".to_string()),
            "date".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "noël".to_string(),
            "inline".to_string(),
            Some("un jeu de console".to_string()),
            None,
            None,
            None,
            "text".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "owner".to_string(),
            "inline_bracket".to_string(),
            Some("[[People/Bob]]".to_string()),
            None,
            None,
            None,
            "link".to_string(),
        )));
    }

    fn assert_dashboard_numeric_and_status_properties(
        dashboard_properties: &[DashboardPropertyRow],
    ) {
        assert!(dashboard_properties.contains(&(
            "priority".to_string(),
            "inline".to_string(),
            None,
            Some(2.0),
            None,
            None,
            "number".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "priority".to_string(),
            "inline_paren".to_string(),
            None,
            Some(3.0),
            None,
            None,
            "number".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "reviewed".to_string(),
            "frontmatter".to_string(),
            None,
            None,
            Some(1),
            None,
            "boolean".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "reviewed".to_string(),
            "inline".to_string(),
            None,
            None,
            Some(0),
            None,
            "boolean".to_string(),
        )));
        assert!(dashboard_properties.contains(&(
            "status".to_string(),
            "frontmatter".to_string(),
            Some("draft".to_string()),
            None,
            None,
            None,
            "text".to_string(),
        )));
    }

    fn assert_dashboard_properties(connection: &rusqlite::Connection) {
        let dashboard_properties = load_dashboard_properties(connection);
        assert_dashboard_text_properties(&dashboard_properties);
        assert_dashboard_numeric_and_status_properties(&dashboard_properties);
        assert_eq!(
            property_list_items(connection, "Dashboard.md", "choices"),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    fn assert_unsupported_js_messages(connection: &rusqlite::Connection) {
        let unsupported_messages: Vec<String> = connection
            .prepare(
                "
                SELECT message
                FROM diagnostics
                JOIN documents ON documents.id = diagnostics.document_id
                WHERE documents.path = 'Dashboard.md' AND kind = 'unsupported_syntax'
                ORDER BY message
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| row.get(0))
            .expect("query should succeed")
            .map(|row| row.expect("row should deserialize"))
            .collect();
        if cfg!(feature = "js_runtime") {
            assert!(unsupported_messages.is_empty());
        } else {
            assert!(unsupported_messages
                .iter()
                .any(|message| message.contains("require the `js_runtime` feature flag")));
        }
    }

    fn assert_task_to_list_links(connection: &rusqlite::Connection) {
        let task_to_list_links: Vec<(String, String)> = connection
            .prepare(
                "
                SELECT tasks.text, list_items.text
                FROM tasks
                JOIN documents ON documents.id = tasks.document_id
                JOIN list_items ON list_items.id = tasks.list_item_id
                ORDER BY documents.path, tasks.line_number
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query should succeed")
            .map(|row| row.expect("row should deserialize"))
            .collect();
        assert_eq!(
            task_to_list_links,
            vec![
                (
                    "Write docs [due:: 2026-04-01]".to_string(),
                    "Write docs [due:: 2026-04-01]".to_string(),
                ),
                (
                    "Ship release [owner:: [[People/Bob]]]".to_string(),
                    "Ship release [owner:: [[People/Bob]]]".to_string(),
                ),
                (
                    "Follow up [due:: 2026-04-02]".to_string(),
                    "Follow up [due:: 2026-04-02]".to_string(),
                ),
                (
                    "Prepare backlog 🗓\u{fe0f} 2026-04-03 ✅ 2026-04-04 ➕ 2026-04-01 🛫 2026-04-02 ⏳ 2026-04-05 🔺 🔁 every week ⛔ ALPHA-1 🆔 BETA-1".to_string(),
                    "Prepare backlog 🗓\u{fe0f} 2026-04-03 ✅ 2026-04-04 ➕ 2026-04-01 🛫 2026-04-02 ⏳ 2026-04-05 🔺 🔁 every week ⛔ ALPHA-1 🆔 BETA-1".to_string(),
                ),
            ]
        );
    }

    fn assert_dashboard_list_metadata(paths: &VaultPaths) {
        let note_index = load_note_index(paths).expect("note index should load");
        let dashboard = note_index
            .get("Dashboard")
            .expect("dashboard note should be indexed");
        let dashboard_lists = FileMetadataResolver::field(dashboard, "lists");
        let dashboard_lists = dashboard_lists
            .as_array()
            .expect("file.lists should return an array");
        assert_eq!(dashboard_lists.len(), 4);
        assert_eq!(dashboard_lists[0]["line"], Value::Number(19.into()));
        assert_eq!(dashboard_lists[0]["annotated"], Value::Bool(true));
        assert_eq!(dashboard_lists[0]["task"], Value::Bool(false));
        assert_eq!(
            dashboard_lists[0]["tags"],
            serde_json::json!(["#project/list"])
        );
        assert_eq!(
            dashboard_lists[0]["outlinks"],
            serde_json::json!(["[[Projects/Alpha]]"])
        );
        assert_eq!(
            dashboard_lists[0]["children"][0]["line"],
            Value::Number(20.into())
        );
        assert_eq!(
            dashboard_lists[1]["link"],
            Value::String("[[Dashboard#^list-child]]".to_string())
        );
    }

    fn assert_dashboard_task_metadata(paths: &VaultPaths) {
        let note_index = load_note_index(paths).expect("note index should load");
        let dashboard = note_index
            .get("Dashboard")
            .expect("dashboard note should be indexed");
        let dashboard_tasks = FileMetadataResolver::field(dashboard, "tasks");
        let dashboard_tasks = dashboard_tasks
            .as_array()
            .expect("file.tasks should return an array");
        assert_eq!(dashboard_tasks.len(), 2);
        assert_eq!(dashboard_tasks[0]["status"], Value::String(" ".to_string()));
        assert_eq!(
            dashboard_tasks[0]["statusType"],
            Value::String("TODO".to_string())
        );
        assert_eq!(dashboard_tasks[0]["checked"], Value::Bool(false));
        assert_eq!(dashboard_tasks[0]["completed"], Value::Bool(false));
        assert_eq!(dashboard_tasks[0]["fullyCompleted"], Value::Bool(false));
        assert_eq!(
            dashboard_tasks[0]["due"],
            Value::String("2026-04-01".to_string())
        );
        assert_eq!(
            dashboard_tasks[0]["children"][0]["status"],
            Value::String("x".to_string())
        );
        assert_eq!(
            dashboard_tasks[1]["owner"],
            Value::String("[[People/Bob]]".to_string())
        );
    }

    fn assert_project_task_metadata(paths: &VaultPaths) {
        let note_index = load_note_index(paths).expect("note index should load");
        let alpha = note_index
            .get("Alpha")
            .expect("alpha note should be indexed");
        let alpha_tasks = FileMetadataResolver::field(alpha, "tasks");
        let alpha_tasks = alpha_tasks
            .as_array()
            .expect("alpha file.tasks should return an array");
        assert_eq!(alpha_tasks.len(), 1);
        assert_eq!(alpha_tasks[0]["status"], Value::String(" ".to_string()));
        assert_eq!(alpha_tasks[0]["priority"], serde_json::json!(1.0));
        assert_eq!(alpha_tasks[0]["reviewed"], Value::Bool(true));

        let beta = note_index.get("Beta").expect("beta note should be indexed");
        let beta_tasks = FileMetadataResolver::field(beta, "tasks");
        let beta_tasks = beta_tasks
            .as_array()
            .expect("beta file.tasks should return an array");
        assert_eq!(beta_tasks.len(), 1);
        assert_eq!(beta_tasks[0]["status"], Value::String("/".to_string()));
        assert_eq!(
            beta_tasks[0]["statusType"],
            Value::String("IN_PROGRESS".to_string())
        );
        assert_eq!(
            beta_tasks[0]["due"],
            Value::String("2026-04-03".to_string())
        );
        assert_eq!(
            beta_tasks[0]["completion"],
            Value::String("2026-04-04".to_string())
        );
        assert_eq!(
            beta_tasks[0]["created"],
            Value::String("2026-04-01".to_string())
        );
        assert_eq!(
            beta_tasks[0]["start"],
            Value::String("2026-04-02".to_string())
        );
        assert_eq!(
            beta_tasks[0]["scheduled"],
            Value::String("2026-04-05".to_string())
        );
        assert_eq!(beta_tasks[0]["priority"], Value::String("high".to_string()));
        assert_eq!(
            beta_tasks[0]["recurrence"],
            Value::String("every week".to_string())
        );
        assert_eq!(
            beta_tasks[0]["blocked-by"],
            Value::String("ALPHA-1".to_string())
        );
        assert_eq!(beta_tasks[0]["id"], Value::String("BETA-1".to_string()));
    }

    fn assert_dataview_search_excludes_js_block_contents(paths: &VaultPaths) {
        let search = search_vault(
            paths,
            &SearchQuery {
                text: "dv.table".to_string(),
                ..SearchQuery::default()
            },
        )
        .expect("search should succeed");
        assert!(search.hits.is_empty());
    }

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
    fn attachment_index_version_changes_with_extraction_config() {
        let disabled = attachment_index_version(&crate::VaultConfig::default());
        let enabled = attachment_index_version(&crate::VaultConfig {
            extraction: Some(AttachmentExtractionConfig {
                command: "sh".to_string(),
                args: vec!["-c".to_string(), "cat \"$1.txt\"".to_string()],
                extensions: vec!["pdf".to_string()],
                max_output_bytes: Some(4096),
            }),
            ..crate::VaultConfig::default()
        });

        assert_ne!(disabled, enabled);
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
    fn attachment_extraction_indexes_search_rows_for_supported_assets() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("attachments", &vault_root);
        write_attachment_sidecar(
            &vault_root,
            "assets/guide.pdf.txt",
            "dashboard manual reference",
        );
        write_attachment_sidecar(&vault_root, "assets/logo.png.txt", "dashboard logo");
        write_attachment_extraction_config(&vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("full scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let search_rows = search_content_rows(database.connection());

        assert!(search_rows.iter().any(|(path, content)| {
            path == "assets/guide.pdf" && content.contains("dashboard manual reference")
        }));
        assert!(search_rows
            .iter()
            .any(|(path, content)| path == "assets/logo.png" && content.contains("dashboard")));
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
    fn dataview_fixture_indexes_inline_fields_tasks_and_metadata() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let connection = database.connection();

        assert_dataview_fixture_row_counts(connection);
        assert_beta_task_properties(connection);
        assert_dashboard_dataview_blocks(connection);
        assert_dashboard_list_items(connection);
        assert_dashboard_properties(connection);
        assert_unsupported_js_messages(connection);
        assert_task_to_list_links(connection);
        assert_dashboard_list_metadata(&paths);
        assert_dashboard_task_metadata(&paths);
        assert_project_task_metadata(&paths);
        assert_dataview_search_excludes_js_block_contents(&paths);
    }

    #[test]
    fn scan_persists_tasks_query_blocks() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Tasks.md"),
            concat!(
                "```tasks\n",
                "not done\n",
                "sort by due reverse\n",
                "```\n\n",
                "```tasks\n",
                "done\n",
                "limit 5\n",
                "```\n"
            ),
        )
        .expect("note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let connection = database.connection();

        assert_eq!(count_rows(connection, "tasks_blocks"), 2);
        let tasks_blocks: Vec<(i64, i64, String)> = connection
            .prepare(
                "
                SELECT tasks_blocks.block_index, tasks_blocks.line_number, tasks_blocks.raw_text
                FROM tasks_blocks
                JOIN documents ON documents.id = tasks_blocks.document_id
                WHERE documents.path = 'Tasks.md'
                ORDER BY tasks_blocks.block_index
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows should collect");
        assert_eq!(
            tasks_blocks,
            vec![
                (0, 1, "not done\nsort by due reverse".to_string()),
                (1, 6, "done\nlimit 5".to_string()),
            ]
        );
    }

    #[test]
    fn dataview_fixture_expression_evaluation_covers_core_semantics() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let note_index = load_note_index(&paths).expect("note index should load");
        let dashboard = note_index
            .get("Dashboard")
            .expect("dashboard note should be indexed");
        let formulas = BTreeMap::new();
        let eval = |input: &str| {
            let expr = Parser::new(input)
                .expect("expression should parse")
                .parse()
                .expect("expression should build AST");
            let ctx = EvalContext::new(dashboard, &formulas).with_note_lookup(&note_index);
            evaluate(&expr, &ctx).expect("expression should evaluate")
        };

        assert_eq!(eval("typeof(month)"), Value::String("date".to_string()));
        assert_eq!(eval("DUE-DATE.month"), json!(4));
        assert_eq!(
            eval(r#"dateformat(month + dur(1d), "yyyy-MM-dd")"#),
            Value::String("2026-04-02".to_string())
        );
        assert_eq!(
            eval("typeof(duration)"),
            Value::String("duration".to_string())
        );
        assert_eq!(eval("duration + dur(1h)"), json!(100_800_000_i64));
        assert_eq!(
            eval(r#"choice(reviewed, "yes", "no")"#),
            json!(["yes", "no"])
        );
        assert_eq!(
            eval(r#"regexreplace(file.tasks.text, "(\w+) (.+)", "$2: $1")"#),
            json!([
                "docs [due:: 2026-04-01]: Write",
                "release [owner:: [[People/Bob]]]: Ship"
            ])
        );
        assert_eq!(
            eval(r#"any(regextest("release", file.tasks.text))"#),
            json!(true)
        );
        assert_eq!(
            eval("file.tasks.owner"),
            json!(["[[People/Bob]]", "[[People/Bob]]"])
        );
        assert_eq!(eval("file.tasks.statusType"), json!(["TODO", "DONE"]));
        assert_eq!(eval("file.tasks.status.type"), json!(["TODO", "DONE"]));
        assert_eq!(eval("file.tasks.status.name"), json!(["Todo", "Done"]));
        assert_eq!(
            eval(r#"default(file.day, "none")"#),
            Value::String("none".to_string())
        );
        assert_eq!(
            eval(r"[[Alpha]].file.tasks[0].due"),
            Value::String("2026-04-02".to_string())
        );
        assert_eq!(
            eval(r"[[Alpha]].FILE.NAME"),
            Value::String("Alpha".to_string())
        );
        assert_eq!(eval(r"[[Bob]].role"), Value::String("editor".to_string()));
        assert_eq!(eval(r"[[Dashboard]].inline_expressions"), Value::Null);
    }

    #[test]
    fn dataview_fixture_inline_expressions_evaluate_against_note_metadata() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("dataview", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let expressions: Vec<String> = database
            .connection()
            .prepare(
                "
                SELECT inline_expressions.expression
                FROM inline_expressions
                JOIN documents ON documents.id = inline_expressions.document_id
                WHERE documents.path = 'Dashboard.md'
                ORDER BY inline_expressions.byte_offset_start
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| row.get(0))
            .expect("query should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows should collect");
        assert_eq!(expressions, vec!["this.status".to_string()]);

        let note_index = load_note_index(&paths).expect("note index should load");
        let dashboard = note_index
            .get("Dashboard")
            .expect("dashboard note should be indexed");
        let formulas = BTreeMap::new();
        let results = expressions
            .iter()
            .map(|expression| {
                let expr = Parser::new(expression)
                    .expect("expression should parse")
                    .parse()
                    .expect("expression should build AST");
                let ctx = EvalContext::new(dashboard, &formulas).with_note_lookup(&note_index);
                evaluate(&expr, &ctx).expect("expression should evaluate")
            })
            .collect::<Vec<_>>();

        assert_eq!(results, vec![Value::String("draft".to_string())]);
    }

    #[test]
    fn extracts_tasks_plugin_text_annotations() {
        let properties = extract_task_text_properties(
            "Prepare backlog 🗓️ 2026-04-03 ✅ 2026-04-04 ➕ 2026-04-01 🛫 2026-04-02 ⏳ 2026-04-05 🔺 🔁 every week ⛔ ALPHA-1 🆔 BETA-1",
        );

        assert_eq!(
            properties,
            vec![
                ("due".to_string(), "2026-04-03".to_string()),
                ("completion".to_string(), "2026-04-04".to_string()),
                ("created".to_string(), "2026-04-01".to_string()),
                ("start".to_string(), "2026-04-02".to_string()),
                ("scheduled".to_string(), "2026-04-05".to_string()),
                ("priority".to_string(), "high".to_string()),
                ("recurrence".to_string(), "every week".to_string()),
                ("blocked-by".to_string(), "ALPHA-1".to_string()),
                ("id".to_string(), "BETA-1".to_string()),
            ]
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
    fn dataview_plugin_settings_control_inline_expression_prefixes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".obsidian/plugins/dataview"))
            .expect("dataview plugin dir should be created");
        fs::write(
            vault_root.join(".obsidian/plugins/dataview/data.json"),
            r#"{
              "inlineQueryPrefix": "dv:"
            }"#,
        )
        .expect("dataview settings should be written");
        fs::write(
            vault_root.join("Dashboard.md"),
            "status:: draft\n`dv: this.status`\n`= this.other`\n",
        )
        .expect("note should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let database = CacheDatabase::open(&paths).expect("database should open");
        let expressions: Vec<String> = database
            .connection()
            .prepare(
                "
                SELECT inline_expressions.expression
                FROM inline_expressions
                JOIN documents ON documents.id = inline_expressions.document_id
                WHERE documents.path = 'Dashboard.md'
                ORDER BY inline_expressions.byte_offset_start
                ",
            )
            .expect("statement should prepare")
            .query_map([], |row| row.get(0))
            .expect("query should succeed")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows should collect");

        assert_eq!(expressions, vec!["this.status".to_string()]);
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

    fn fixture_names() -> [&'static str; 7] {
        [
            "basic",
            "ambiguous-links",
            "mixed-properties",
            "broken-frontmatter",
            "move-rewrite",
            "attachments",
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

    fn search_content_rows(connection: &Connection) -> Vec<(String, String)> {
        let mut statement = connection
            .prepare(
                "
                SELECT documents.path, search_chunk_content.content
                FROM search_chunk_content
                JOIN documents ON documents.id = search_chunk_content.document_id
                ORDER BY documents.path, search_chunk_content.id
                ",
            )
            .expect("statement should prepare");
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
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

    fn write_attachment_extraction_config(vault_root: &Path) {
        fs::create_dir_all(vault_root.join(".vulcan")).expect("config dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            "[extraction]\ncommand = \"sh\"\nargs = [\"-c\", \"cat \\\"$1.txt\\\"\", \"sh\", \"{path}\"]\nextensions = [\"pdf\", \"png\"]\nmax_output_bytes = 4096\n",
        )
        .expect("config should write");
    }

    fn write_attachment_sidecar(vault_root: &Path, relative_path: &str, contents: &str) {
        let path = vault_root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("sidecar parent should exist");
        }
        fs::write(path, contents).expect("sidecar should write");
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
