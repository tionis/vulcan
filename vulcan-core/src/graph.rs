use crate::VaultPaths;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;

#[derive(Debug)]
pub enum GraphQueryError {
    AmbiguousIdentifier {
        identifier: String,
        matches: Vec<String>,
    },
    CacheMissing,
    Io(std::io::Error),
    NoteNotFound {
        identifier: String,
    },
    Sqlite(rusqlite::Error),
}

impl Display for GraphQueryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AmbiguousIdentifier {
                identifier,
                matches,
            } => write!(
                formatter,
                "note identifier '{identifier}' is ambiguous: {}",
                matches.join(", ")
            ),
            Self::CacheMissing => {
                formatter.write_str("cache is missing; run `vulcan scan` before querying the graph")
            }
            Self::Io(error) => write!(formatter, "{error}"),
            Self::NoteNotFound { identifier } => {
                write!(formatter, "note not found: {identifier}")
            }
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for GraphQueryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::AmbiguousIdentifier { .. } | Self::CacheMissing | Self::NoteNotFound { .. } => {
                None
            }
        }
    }
}

impl From<std::io::Error> for GraphQueryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for GraphQueryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteMatchKind {
    Path,
    Filename,
    Alias,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    External,
    Resolved,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineContext {
    pub line: usize,
    pub column: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutgoingLinksReport {
    pub note_path: String,
    pub matched_by: NoteMatchKind,
    pub links: Vec<OutgoingLinkRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutgoingLinkRecord {
    pub raw_text: String,
    pub link_kind: String,
    pub display_text: Option<String>,
    pub target_path_candidate: Option<String>,
    pub target_heading: Option<String>,
    pub target_block: Option<String>,
    pub resolved_target_path: Option<String>,
    pub resolution_status: ResolutionStatus,
    pub context: Option<LineContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacklinksReport {
    pub note_path: String,
    pub matched_by: NoteMatchKind,
    pub backlinks: Vec<BacklinkRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BacklinkRecord {
    pub source_path: String,
    pub raw_text: String,
    pub link_kind: String,
    pub display_text: Option<String>,
    pub context: Option<LineContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedNote {
    id: String,
    path: String,
    filename: String,
    aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedNote {
    id: String,
    path: String,
    matched_by: NoteMatchKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteReference {
    pub id: String,
    pub path: String,
    pub matched_by: NoteMatchKind,
}

pub fn resolve_note_reference(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<NoteReference, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let note = resolve_note_identifier(&notes, identifier)?;

    Ok(NoteReference {
        id: note.id,
        path: note.path,
        matched_by: note.matched_by,
    })
}

pub fn query_links(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<OutgoingLinksReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let note = resolve_note_identifier(&notes, identifier)?;
    let mut source_cache = HashMap::new();
    let mut statement = connection.prepare(
        "
        SELECT
            links.raw_text,
            links.link_kind,
            links.display_text,
            links.target_path_candidate,
            links.target_heading,
            links.target_block,
            links.byte_offset,
            target.path
        FROM links
        LEFT JOIN documents AS target ON target.id = links.resolved_target_id
        WHERE links.source_document_id = ?1
        ORDER BY links.byte_offset
        ",
    )?;
    let rows = statement.query_map(params![&note.id], |row| {
        Ok(OutgoingLinkRow {
            raw_text: row.get(0)?,
            link_kind: row.get(1)?,
            display_text: row.get(2)?,
            target_path_candidate: row.get(3)?,
            target_heading: row.get(4)?,
            target_block: row.get(5)?,
            byte_offset: row.get(6)?,
            resolved_target_path: row.get(7)?,
        })
    })?;
    let links = rows
        .map(|row| {
            let row = row?;
            let has_resolved_target = row.resolved_target_path.is_some();
            Ok(OutgoingLinkRecord {
                raw_text: row.raw_text,
                link_kind: row.link_kind.clone(),
                display_text: row.display_text,
                target_path_candidate: row.target_path_candidate,
                target_heading: row.target_heading,
                target_block: row.target_block,
                resolved_target_path: row.resolved_target_path,
                resolution_status: resolution_status(&row.link_kind, has_resolved_target),
                context: load_context(paths, &note.path, row.byte_offset, &mut source_cache),
            })
        })
        .collect::<Result<Vec<_>, GraphQueryError>>()?;

    Ok(OutgoingLinksReport {
        note_path: note.path,
        matched_by: note.matched_by,
        links,
    })
}

pub fn query_backlinks(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<BacklinksReport, GraphQueryError> {
    let connection = open_existing_cache(paths)?;
    let notes = load_indexed_notes(&connection)?;
    let note = resolve_note_identifier(&notes, identifier)?;
    let mut source_cache = HashMap::new();
    let mut statement = connection.prepare(
        "
        SELECT
            source.path,
            links.raw_text,
            links.link_kind,
            links.display_text,
            links.byte_offset
        FROM links
        JOIN documents AS source ON source.id = links.source_document_id
        WHERE links.resolved_target_id = ?1
        ORDER BY source.path, links.byte_offset
        ",
    )?;
    let rows = statement.query_map(params![&note.id], |row| {
        Ok(BacklinkRow {
            source_path: row.get(0)?,
            raw_text: row.get(1)?,
            link_kind: row.get(2)?,
            display_text: row.get(3)?,
            byte_offset: row.get(4)?,
        })
    })?;
    let backlinks = rows
        .map(|row| {
            let row = row?;
            Ok(BacklinkRecord {
                source_path: row.source_path.clone(),
                raw_text: row.raw_text,
                link_kind: row.link_kind,
                display_text: row.display_text,
                context: load_context(paths, &row.source_path, row.byte_offset, &mut source_cache),
            })
        })
        .collect::<Result<Vec<_>, GraphQueryError>>()?;

    Ok(BacklinksReport {
        note_path: note.path,
        matched_by: note.matched_by,
        backlinks,
    })
}

fn open_existing_cache(paths: &VaultPaths) -> Result<Connection, GraphQueryError> {
    if !paths.cache_db().exists() {
        return Err(GraphQueryError::CacheMissing);
    }

    Ok(Connection::open(paths.cache_db())?)
}

fn load_indexed_notes(connection: &Connection) -> Result<Vec<IndexedNote>, GraphQueryError> {
    let mut alias_statement =
        connection.prepare("SELECT document_id, alias_text FROM aliases ORDER BY alias_text")?;
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

    let mut statement = connection
        .prepare("SELECT id, path, filename FROM documents WHERE extension = 'md' ORDER BY path")?;
    let rows = statement.query_map([], |row| {
        let id: String = row.get(0)?;
        Ok(IndexedNote {
            aliases: aliases_by_document.remove(&id).unwrap_or_default(),
            id,
            path: row.get(1)?,
            filename: row.get(2)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(GraphQueryError::from)
}

fn resolve_note_identifier(
    notes: &[IndexedNote],
    identifier: &str,
) -> Result<ResolvedNote, GraphQueryError> {
    for (match_kind, predicate) in [
        (
            NoteMatchKind::Path,
            matches_path as fn(&IndexedNote, &str) -> bool,
        ),
        (
            NoteMatchKind::Filename,
            matches_filename as fn(&IndexedNote, &str) -> bool,
        ),
        (
            NoteMatchKind::Alias,
            matches_alias as fn(&IndexedNote, &str) -> bool,
        ),
    ] {
        let matches = notes
            .iter()
            .filter(|note| predicate(note, identifier))
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [] => {}
            [single] => {
                return Ok(ResolvedNote {
                    id: single.id.clone(),
                    path: single.path.clone(),
                    matched_by: match_kind,
                });
            }
            _ => {
                let mut paths = matches
                    .into_iter()
                    .map(|note| note.path.clone())
                    .collect::<Vec<_>>();
                paths.sort();
                return Err(GraphQueryError::AmbiguousIdentifier {
                    identifier: identifier.to_string(),
                    matches: paths,
                });
            }
        }
    }

    Err(GraphQueryError::NoteNotFound {
        identifier: identifier.to_string(),
    })
}

fn matches_path(note: &IndexedNote, identifier: &str) -> bool {
    note.path.eq_ignore_ascii_case(identifier)
        || strip_markdown_extension(&note.path).eq_ignore_ascii_case(identifier)
}

fn matches_filename(note: &IndexedNote, identifier: &str) -> bool {
    note.filename.eq_ignore_ascii_case(identifier)
        || format!("{}.md", note.filename).eq_ignore_ascii_case(identifier)
}

fn matches_alias(note: &IndexedNote, identifier: &str) -> bool {
    note.aliases
        .iter()
        .any(|alias| alias.eq_ignore_ascii_case(identifier))
}

fn strip_markdown_extension(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

fn resolution_status(link_kind: &str, has_resolved_target: bool) -> ResolutionStatus {
    if link_kind == "external" {
        ResolutionStatus::External
    } else if has_resolved_target {
        ResolutionStatus::Resolved
    } else {
        ResolutionStatus::Unresolved
    }
}

fn load_context(
    paths: &VaultPaths,
    relative_path: &str,
    byte_offset: usize,
    source_cache: &mut HashMap<String, Option<String>>,
) -> Option<LineContext> {
    let source = if let Some(source) = source_cache.get(relative_path) {
        source.clone()
    } else {
        let source = fs::read_to_string(paths.vault_root().join(relative_path)).ok();
        source_cache.insert(relative_path.to_string(), source.clone());
        source
    };

    source.and_then(|text| line_context(&text, byte_offset))
}

fn line_context(source: &str, byte_offset: usize) -> Option<LineContext> {
    let clamped = byte_offset.min(source.len());
    if !source.is_char_boundary(clamped) {
        return None;
    }

    let prefix = &source[..clamped];
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[clamped..]
        .find('\n')
        .map_or(source.len(), |index| clamped + index);
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = source[line_start..clamped].chars().count() + 1;

    Some(LineContext {
        line,
        column,
        text: source[line_start..line_end]
            .trim_end_matches('\r')
            .to_string(),
    })
}

struct OutgoingLinkRow {
    raw_text: String,
    link_kind: String,
    display_text: Option<String>,
    target_path_candidate: Option<String>,
    target_heading: Option<String>,
    target_block: Option<String>,
    byte_offset: usize,
    resolved_target_path: Option<String>,
}

struct BacklinkRow {
    source_path: String,
    raw_text: String,
    link_kind: String,
    display_text: Option<String>,
    byte_offset: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan_vault, ScanMode};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn query_links_resolves_path_filename_and_alias() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let by_path = query_links(&paths, "Home.md").expect("path query should succeed");
        let by_filename = query_links(&paths, "Bob").expect("filename query should succeed");
        let by_alias = query_links(&paths, "Start").expect("alias query should succeed");

        assert_eq!(by_path.note_path, "Home.md");
        assert_eq!(by_path.matched_by, NoteMatchKind::Path);
        assert_eq!(by_filename.note_path, "People/Bob.md");
        assert_eq!(by_filename.matched_by, NoteMatchKind::Filename);
        assert_eq!(by_alias.note_path, "Home.md");
        assert_eq!(by_alias.matched_by, NoteMatchKind::Alias);
        assert_eq!(by_alias.links.len(), 2);
        assert_eq!(
            by_alias
                .links
                .iter()
                .map(|link| link.resolved_target_path.clone())
                .collect::<Vec<_>>(),
            vec![
                Some("Projects/Alpha.md".to_string()),
                Some("People/Bob.md".to_string())
            ]
        );
    }

    #[test]
    fn query_backlinks_returns_sources_with_context() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("basic", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let report = query_backlinks(&paths, "Projects/Alpha").expect("query should succeed");

        assert_eq!(report.note_path, "Projects/Alpha.md");
        assert_eq!(
            report
                .backlinks
                .iter()
                .map(|link| (
                    link.source_path.clone(),
                    link.context.as_ref().map(|context| context.line)
                ))
                .collect::<Vec<_>>(),
            vec![
                ("Home.md".to_string(), Some(10)),
                ("People/Bob.md".to_string(), Some(8))
            ]
        );
    }

    #[test]
    fn ambiguous_identifiers_are_reported() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("ambiguous-links", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let error = query_links(&paths, "Topic").expect_err("query should fail");

        match error {
            GraphQueryError::AmbiguousIdentifier { matches, .. } => assert_eq!(
                matches,
                vec![
                    "Archive/Topic.md".to_string(),
                    "Projects/Topic.md".to_string()
                ]
            ),
            other => panic!("expected ambiguous identifier error, got {other:?}"),
        }
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        std::fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in std::fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).expect("parent directory should exist");
                }
                std::fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}
