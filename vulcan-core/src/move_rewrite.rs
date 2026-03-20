use crate::graph::resolve_note_reference;
use crate::parser::{parse_document, RawLink};
use crate::scan::scan_vault_unlocked;
use crate::write_lock::acquire_write_lock;
use crate::{
    load_vault_config, GraphQueryError, LinkResolutionMode, ScanError, ScanMode, VaultPaths,
};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum MoveError {
    DestinationExists(PathBuf),
    Graph(GraphQueryError),
    InvalidDestination(String),
    Io(std::io::Error),
    MissingLinkSpan { path: String, byte_offset: usize },
    Scan(ScanError),
    Sqlite(rusqlite::Error),
}

impl Display for MoveError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DestinationExists(path) => {
                write!(formatter, "destination already exists: {}", path.display())
            }
            Self::Graph(error) => write!(formatter, "{error}"),
            Self::InvalidDestination(path) => write!(formatter, "invalid destination path: {path}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::MissingLinkSpan { path, byte_offset } => {
                write!(
                    formatter,
                    "failed to locate cached link at byte offset {byte_offset} in {path}"
                )
            }
            Self::Scan(error) => write!(formatter, "{error}"),
            Self::Sqlite(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for MoveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Graph(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::DestinationExists(_)
            | Self::InvalidDestination(_)
            | Self::MissingLinkSpan { .. } => None,
        }
    }
}

impl From<GraphQueryError> for MoveError {
    fn from(error: GraphQueryError) -> Self {
        Self::Graph(error)
    }
}

impl From<std::io::Error> for MoveError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for MoveError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<ScanError> for MoveError {
    fn from(error: ScanError) -> Self {
        Self::Scan(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveSummary {
    pub dry_run: bool,
    pub source_path: String,
    pub destination_path: String,
    pub rewritten_files: Vec<RewrittenFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RewrittenFile {
    pub path: String,
    pub changes: Vec<LinkChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkChange {
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedInboundLink {
    source_path: String,
    raw_text: String,
    byte_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextEdit {
    start: usize,
    end: usize,
    replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRewritePlan {
    original_path: String,
    output_path: String,
    updated_contents: String,
    changes: Vec<LinkChange>,
}

pub fn move_note(
    paths: &VaultPaths,
    source_identifier: &str,
    destination: &str,
    dry_run: bool,
) -> Result<MoveSummary, MoveError> {
    let _lock = acquire_write_lock(paths)?;
    let source = resolve_note_reference(paths, source_identifier)?;
    let destination_path = normalize_destination_path(destination)?;
    if source.path == destination_path {
        return Ok(MoveSummary {
            dry_run,
            source_path: source.path,
            destination_path,
            rewritten_files: Vec::new(),
        });
    }

    let destination_absolute = paths.vault_root().join(&destination_path);
    if destination_absolute.exists() {
        return Err(MoveError::DestinationExists(destination_absolute));
    }

    let config = load_vault_config(paths).config;
    let connection = open_existing_cache(paths)?;
    let inbound_links = load_inbound_links(&connection, &source.id)?;
    let note_paths = load_note_paths(&connection)?;
    let note_paths_after_move = note_paths
        .into_iter()
        .map(|path| {
            if path == source.path {
                destination_path.clone()
            } else {
                path
            }
        })
        .collect::<Vec<_>>();
    let rewrite_plans = plan_rewrites(
        paths,
        &source.path,
        &destination_path,
        &inbound_links,
        &note_paths_after_move,
        &config,
        config.link_resolution,
    )?;
    let rewritten_files = rewrite_plans
        .iter()
        .filter(|plan| !plan.changes.is_empty())
        .map(|plan| RewrittenFile {
            path: plan.output_path.clone(),
            changes: plan.changes.clone(),
        })
        .collect::<Vec<_>>();

    if dry_run {
        return Ok(MoveSummary {
            dry_run: true,
            source_path: source.path,
            destination_path,
            rewritten_files,
        });
    }

    if let Some(parent) = destination_absolute.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(
        paths.vault_root().join(&source.path),
        paths.vault_root().join(&destination_path),
    )?;
    for plan in &rewrite_plans {
        if plan.changes.is_empty() {
            continue;
        }

        fs::write(
            paths.vault_root().join(&plan.output_path),
            &plan.updated_contents,
        )?;
    }

    scan_vault_unlocked(paths, ScanMode::Incremental)?;

    Ok(MoveSummary {
        dry_run: false,
        source_path: source.path,
        destination_path,
        rewritten_files,
    })
}

fn open_existing_cache(paths: &VaultPaths) -> Result<Connection, MoveError> {
    if !paths.cache_db().exists() {
        return Err(MoveError::Graph(GraphQueryError::CacheMissing));
    }

    Ok(Connection::open(paths.cache_db())?)
}

fn normalize_destination_path(destination: &str) -> Result<String, MoveError> {
    if destination.chars().any(char::is_control) {
        return Err(MoveError::InvalidDestination(destination.to_string()));
    }

    let mut parts = Vec::new();
    for component in Path::new(destination).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(MoveError::InvalidDestination(destination.to_string()));
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(MoveError::InvalidDestination(destination.to_string()));
            }
        }
    }

    if parts.is_empty() {
        return Err(MoveError::InvalidDestination(destination.to_string()));
    }

    let mut normalized = parts.join("/");
    if Path::new(&normalized).extension().is_none() {
        normalized.push_str(".md");
    }
    if Path::new(&normalized)
        .extension()
        .and_then(|value| value.to_str())
        != Some("md")
    {
        return Err(MoveError::InvalidDestination(destination.to_string()));
    }

    Ok(normalized)
}

fn load_inbound_links(
    connection: &Connection,
    document_id: &str,
) -> Result<Vec<CachedInboundLink>, MoveError> {
    let mut statement = connection.prepare(
        "
        SELECT source.path, links.raw_text, links.byte_offset
        FROM links
        JOIN documents AS source ON source.id = links.source_document_id
        WHERE links.resolved_target_id = ?1
        ORDER BY source.path, links.byte_offset
        ",
    )?;
    let rows = statement.query_map(params![document_id], |row| {
        Ok(CachedInboundLink {
            source_path: row.get(0)?,
            raw_text: row.get(1)?,
            byte_offset: row.get(2)?,
        })
    })?;

    rows.collect::<Result<Vec<_>, _>>().map_err(MoveError::from)
}

fn load_note_paths(connection: &Connection) -> Result<Vec<String>, MoveError> {
    let mut statement =
        connection.prepare("SELECT path FROM documents WHERE extension = 'md' ORDER BY path")?;
    let rows = statement.query_map([], |row| row.get(0))?;

    rows.collect::<Result<Vec<_>, _>>().map_err(MoveError::from)
}

fn plan_rewrites(
    paths: &VaultPaths,
    source_path: &str,
    destination_path: &str,
    inbound_links: &[CachedInboundLink],
    note_paths_after_move: &[String],
    config: &crate::VaultConfig,
    resolution_mode: LinkResolutionMode,
) -> Result<Vec<FileRewritePlan>, MoveError> {
    let mut inbound_by_file = BTreeMap::<String, Vec<CachedInboundLink>>::new();
    for inbound_link in inbound_links {
        inbound_by_file
            .entry(inbound_link.source_path.clone())
            .or_default()
            .push(inbound_link.clone());
    }

    let mut plans = Vec::new();
    for (original_path, links) in inbound_by_file {
        let source_contents = fs::read_to_string(paths.vault_root().join(&original_path))?;
        let parsed = parse_document(&source_contents, config);
        let output_path = if original_path == source_path {
            destination_path.to_string()
        } else {
            original_path.clone()
        };
        let mut edits = Vec::new();
        let mut changes = Vec::new();

        for inbound_link in links {
            let raw_link = parsed
                .links
                .iter()
                .find(|link| {
                    link.byte_offset == inbound_link.byte_offset
                        && link.raw_text == inbound_link.raw_text
                })
                .ok_or_else(|| MoveError::MissingLinkSpan {
                    path: original_path.clone(),
                    byte_offset: inbound_link.byte_offset,
                })?;
            let replacement = rewrite_link(
                raw_link,
                &output_path,
                destination_path,
                note_paths_after_move,
                resolution_mode,
            );
            if replacement != raw_link.raw_text {
                edits.push(TextEdit {
                    start: raw_link.byte_offset,
                    end: raw_link.byte_offset + raw_link.raw_text.len(),
                    replacement: replacement.clone(),
                });
                changes.push(LinkChange {
                    before: raw_link.raw_text.clone(),
                    after: replacement,
                });
            }
        }

        let updated_contents = apply_edits(&source_contents, &edits);
        plans.push(FileRewritePlan {
            original_path,
            output_path,
            updated_contents,
            changes,
        });
    }

    Ok(plans)
}

fn rewrite_link(
    link: &RawLink,
    source_path: &str,
    destination_path: &str,
    note_paths_after_move: &[String],
    resolution_mode: LinkResolutionMode,
) -> String {
    let Some(original_target) = link.target_path_candidate.as_deref() else {
        return link.raw_text.clone();
    };
    let preserve_extension = Path::new(original_target)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    let rewritten_target = match resolution_mode {
        LinkResolutionMode::Absolute => format_target_path(destination_path, preserve_extension),
        LinkResolutionMode::Relative => {
            let relative = relative_path_from_source(source_path, destination_path);
            format_target_path(&relative, preserve_extension)
        }
        LinkResolutionMode::Shortest => {
            shortest_unique_path(destination_path, note_paths_after_move, preserve_extension)
        }
    };
    let suffix = if let Some(heading) = link.target_heading.as_deref() {
        format!("#{heading}")
    } else if let Some(block) = link.target_block.as_deref() {
        format!("#^{block}")
    } else {
        String::new()
    };
    let target = format!("{rewritten_target}{suffix}");

    if link.raw_text.starts_with("![[") {
        if let Some(display_text) = link.display_text.as_deref() {
            format!("![[{target}|{display_text}]]")
        } else {
            format!("![[{target}]]")
        }
    } else if link.raw_text.starts_with("[[") {
        if let Some(display_text) = link.display_text.as_deref() {
            format!("[[{target}|{display_text}]]")
        } else {
            format!("[[{target}]]")
        }
    } else if link.raw_text.starts_with("![") {
        let label = link
            .display_text
            .clone()
            .unwrap_or_else(|| extract_markdown_label(&link.raw_text));
        format!("![{label}]({target})")
    } else if link.raw_text.starts_with('[') {
        let label = link
            .display_text
            .clone()
            .unwrap_or_else(|| extract_markdown_label(&link.raw_text));
        format!("[{label}]({target})")
    } else {
        link.raw_text.clone()
    }
}

fn extract_markdown_label(raw_text: &str) -> String {
    let start = if raw_text.starts_with("![") { 2 } else { 1 };
    raw_text[start..]
        .split("](")
        .next()
        .unwrap_or_default()
        .to_string()
}

fn format_target_path(path: &str, preserve_extension: bool) -> String {
    if preserve_extension {
        if Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            path.to_string()
        } else {
            format!("{path}.md")
        }
    } else {
        path.strip_suffix(".md").unwrap_or(path).to_string()
    }
}

fn relative_path_from_source(source_path: &str, destination_path: &str) -> String {
    let source_dir = Path::new(source_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let source_parts = source_dir
        .components()
        .filter_map(component_to_string)
        .collect::<Vec<_>>();
    let destination_parts = Path::new(destination_path)
        .components()
        .filter_map(component_to_string)
        .collect::<Vec<_>>();
    let shared = source_parts
        .iter()
        .zip(destination_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = Vec::new();
    for _ in 0..(source_parts.len() - shared) {
        parts.push("..".to_string());
    }
    parts.extend(destination_parts.into_iter().skip(shared));
    parts.join("/")
}

fn component_to_string(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
        Component::CurDir | Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
            None
        }
    }
}

fn shortest_unique_path(
    destination_path: &str,
    note_paths: &[String],
    preserve_extension: bool,
) -> String {
    let destination = strip_markdown_extension(destination_path);
    let destination_parts = destination.split('/').collect::<Vec<_>>();
    for suffix_len in 1..=destination_parts.len() {
        let candidate_parts = &destination_parts[destination_parts.len() - suffix_len..];
        let matches = note_paths
            .iter()
            .filter(|path| path_suffix_matches(path, candidate_parts))
            .count();
        if matches == 1 {
            return format_target_path(&candidate_parts.join("/"), preserve_extension);
        }
    }

    format_target_path(destination_path, preserve_extension)
}

fn strip_markdown_extension(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

fn path_suffix_matches(path: &str, candidate_parts: &[&str]) -> bool {
    let path_parts = strip_markdown_extension(path)
        .split('/')
        .collect::<Vec<_>>();
    path_parts.ends_with(candidate_parts)
}

fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut updated = source.to_string();
    let mut sorted = edits.to_vec();
    sorted.sort_by(|left, right| right.start.cmp(&left.start));
    for edit in sorted {
        updated.replace_range(edit.start..edit.end, &edit.replacement);
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{doctor_vault, scan_vault};
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    #[test]
    fn rewrite_link_preserves_style_and_subpaths() {
        let link = RawLink {
            raw_text: "[[Projects/Alpha#Status|Project Alpha]]".to_string(),
            link_kind: crate::LinkKind::Wikilink,
            display_text: Some("Project Alpha".to_string()),
            target_path_candidate: Some("Projects/Alpha".to_string()),
            target_heading: Some("Status".to_string()),
            target_block: None,
            origin_context: crate::OriginContext::Body,
            byte_offset: 0,
            is_note_embed: false,
        };

        assert_eq!(
            rewrite_link(
                &link,
                "People/Bob.md",
                "Archive/Alpha.md",
                &["Archive/Alpha.md".to_string(), "People/Bob.md".to_string()],
                LinkResolutionMode::Relative,
            ),
            "[[../Archive/Alpha#Status|Project Alpha]]"
        );
    }

    #[test]
    fn shortest_unique_path_uses_smallest_unique_suffix() {
        assert_eq!(
            shortest_unique_path(
                "Projects/Alpha.md",
                &[
                    "Projects/Alpha.md".to_string(),
                    "Archive/Alpha.md".to_string(),
                    "Projects/Beta.md".to_string()
                ],
                false,
            ),
            "Projects/Alpha"
        );
    }

    #[test]
    fn apply_edits_runs_back_to_front() {
        let source = "[[One]] and [[Two]]";
        let updated = apply_edits(
            source,
            &[
                TextEdit {
                    start: 0,
                    end: 7,
                    replacement: "[[Alpha]]".to_string(),
                },
                TextEdit {
                    start: 12,
                    end: 19,
                    replacement: "[[Beta]]".to_string(),
                },
            ],
        );

        assert_eq!(updated, "[[Alpha]] and [[Beta]]");
    }

    #[test]
    fn move_rewrite_fixture_updates_inbound_links_and_roundtrips() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("move-rewrite", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let dry_run = move_note(&paths, "Projects/Alpha.md", "Archive/Alpha.md", true)
            .expect("dry run should succeed");
        assert_eq!(dry_run.rewritten_files.len(), 2);
        assert!(vault_root.join("Projects/Alpha.md").exists());

        move_note(&paths, "Projects/Alpha.md", "Archive/Alpha.md", false)
            .expect("move should succeed");
        assert!(!vault_root.join("Projects/Alpha.md").exists());
        assert!(vault_root.join("Archive/Alpha.md").exists());
        let home_contents =
            fs::read_to_string(vault_root.join("Home.md")).expect("home should be readable");
        assert!(home_contents.contains("[[Archive/Alpha#Status]]"));
        assert!(home_contents.contains("reference: \"[Alpha doc](Archive/Alpha.md)\""));
        assert!(home_contents.contains("embed: \"![[Archive/Alpha]]\""));
        assert!(fs::read_to_string(vault_root.join("People/Bob.md"))
            .expect("bob should be readable")
            .contains("[[../Archive/Alpha|Project Alpha]]"));
        assert_eq!(
            doctor_vault(&paths)
                .expect("doctor should succeed")
                .summary
                .unresolved_links,
            0
        );

        move_note(&paths, "Archive/Alpha.md", "Projects/Alpha.md", false)
            .expect("roundtrip move should succeed");
        assert_eq!(
            fs::read_to_string(vault_root.join("Home.md")).expect("home should be readable"),
            fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../tests/fixtures/vaults/move-rewrite/Home.md")
            )
            .expect("fixture should be readable")
        );
    }

    #[test]
    fn concurrent_scan_and_move_produce_consistent_state() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("move-rewrite", &vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let barrier = Arc::new(Barrier::new(2));
        let move_paths = paths.clone();
        let scan_paths = paths.clone();
        let move_barrier = Arc::clone(&barrier);
        let scan_barrier = Arc::clone(&barrier);

        let move_thread = thread::spawn(move || {
            move_barrier.wait();
            move_note(&move_paths, "Projects/Alpha.md", "Archive/Alpha.md", false)
                .expect("move should succeed");
        });
        let scan_thread = thread::spawn(move || {
            scan_barrier.wait();
            scan_vault(&scan_paths, ScanMode::Incremental).expect("scan should succeed");
        });

        move_thread.join().expect("move thread should join");
        scan_thread.join().expect("scan thread should join");

        assert!(vault_root.join("Archive/Alpha.md").exists());
        assert_eq!(
            doctor_vault(&paths)
                .expect("doctor should succeed")
                .summary
                .unresolved_links,
            0
        );
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
