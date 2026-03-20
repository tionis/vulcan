use crate::cache::CacheError;
use crate::parser::{parse_document, LinkKind, OriginContext, ParseDiagnosticKind, ParsedDocument};
use crate::resolver::{resolve_link, LinkResolutionProblem, ResolverDocument, ResolverLink};
use crate::write_lock::acquire_write_lock;
use crate::{load_vault_config, CacheDatabase, VaultPaths, PARSER_VERSION};
use ignore::WalkBuilder;
use rusqlite::{params, Connection, Transaction};
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
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
    let _lock = acquire_write_lock(paths)?;
    scan_vault_unlocked(paths, mode)
}

pub(crate) fn scan_vault_unlocked(
    paths: &VaultPaths,
    mode: ScanMode,
) -> Result<ScanSummary, ScanError> {
    let config = load_vault_config(paths).config;
    let discovered = discover_files(paths.vault_root())?;
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
            database.rebuild_with(|transaction| -> Result<ScanSummary, ScanError> {
                for file in &discovered {
                    let hash = compute_content_hash(&file.absolute_path)?;
                    let id = Ulid::new().to_string();
                    insert_or_update_document(transaction, &id, file, &hash, None)?;
                    if matches!(file.kind, DocumentKind::Note) {
                        index_note_document(transaction, &config, &id, file, &hash)?;
                    }
                }
                resolve_all_links(transaction, config.link_resolution)?;

                Ok(ScanSummary {
                    mode,
                    discovered: discovered_count,
                    added: discovered_count,
                    updated: 0,
                    unchanged: 0,
                    deleted: deleted_count,
                })
            })
        }
        ScanMode::Incremental => {
            database.with_transaction(|transaction| -> Result<ScanSummary, ScanError> {
                let summary = apply_incremental_scan(
                    transaction,
                    &config,
                    &discovered,
                    &existing,
                    &deleted_paths,
                    mode,
                )?;
                resolve_all_links(transaction, config.link_resolution)?;
                Ok(summary)
            })
        }
    }
}

fn apply_incremental_scan(
    transaction: &Transaction<'_>,
    config: &crate::VaultConfig,
    discovered: &[DiscoveredFile],
    existing: &HashMap<String, CachedDocument>,
    deleted_paths: &[String],
    mode: ScanMode,
) -> Result<ScanSummary, ScanError> {
    let mut summary = ScanSummary {
        mode,
        discovered: discovered.len(),
        added: 0,
        updated: 0,
        unchanged: 0,
        deleted: 0,
    };
    let mut seen_paths = HashSet::with_capacity(discovered.len());

    for file in discovered {
        seen_paths.insert(file.relative_path.clone());

        match existing.get(&file.relative_path) {
            Some(cached)
                if cached.file_size == file.file_size
                    && cached.file_mtime == file.file_mtime
                    && cached.parser_version == PARSER_VERSION =>
            {
                summary.unchanged += 1;
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
                    summary.unchanged += 1;
                } else {
                    insert_or_update_document(transaction, &cached.id, file, &hash, None)?;
                    if matches!(file.kind, DocumentKind::Note) {
                        index_note_document(transaction, config, &cached.id, file, &hash)?;
                    }
                    summary.updated += 1;
                }
            }
            None => {
                let hash = compute_content_hash(&file.absolute_path)?;
                let id = Ulid::new().to_string();
                insert_or_update_document(transaction, &id, file, &hash, None)?;
                if matches!(file.kind, DocumentKind::Note) {
                    index_note_document(transaction, config, &id, file, &hash)?;
                }
                summary.added += 1;
            }
        }
    }

    for path in deleted_paths {
        if let Some(cached) = existing.get(path) {
            delete_document(transaction, &cached.id)?;
            summary.deleted += 1;
        }
    }

    debug_assert_eq!(seen_paths.len(), discovered.len());
    Ok(summary)
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
    replace_derived_rows(transaction, id, &parsed)?;

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
    transaction.execute("DELETE FROM documents WHERE id = ?1", [id])?;
    Ok(())
}

fn replace_derived_rows(
    transaction: &Transaction<'_>,
    document_id: &str,
    parsed: &ParsedDocument,
) -> Result<(), ScanError> {
    clear_derived_rows(transaction, document_id)?;
    insert_headings(transaction, document_id, &parsed.headings)?;
    insert_block_refs(transaction, document_id, &parsed.block_refs)?;
    insert_links(transaction, document_id, &parsed.links)?;
    insert_aliases(transaction, document_id, &parsed.aliases)?;
    insert_tags(transaction, document_id, &parsed.tags)?;
    insert_chunks(transaction, document_id, &parsed.chunk_texts)?;
    insert_diagnostics(transaction, document_id, &parsed.diagnostics)?;
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
) -> Result<(), ScanError> {
    for chunk in chunks {
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
                Ulid::new().to_string(),
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
        assert_eq!(count_rows(database.connection(), "diagnostics"), 0);
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

    fn count_rows(connection: &Connection, table_name: &str) -> i64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table_name}"), [], |row| {
                row.get(0)
            })
            .expect("row count should be readable")
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
