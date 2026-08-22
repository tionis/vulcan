use super::{ExportLinkRecord, ExportedNoteDocument};
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use vulcan_core::VaultPaths;
use zip::write::FileOptions;

pub const SUPPORTED_OUTLINE_VERSION: &str = "1.9.x";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineDiagnosticKind {
    UnsafePath,
    Collision,
    DuplicateFolderNote,
    MissingFolderNote,
    MissingAsset,
    UnresolvedLink,
    ExcludedTarget,
    UnsupportedLink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineDiagnostic {
    pub kind: OutlineDiagnosticKind,
    pub source_path: Option<String>,
    pub target: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePlannedDocument {
    pub source_path: String,
    pub source_document_id: String,
    pub title: String,
    pub archive_path: String,
    pub parent_source_path: Option<String>,
    pub content_hash: String,
    #[serde(skip)]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePlannedAttachment {
    pub source_path: String,
    pub archive_path: String,
    pub content_hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlinePublicationPlan {
    pub collection_title: String,
    pub collection_directory: String,
    pub outline_version: String,
    pub documents: Vec<OutlinePlannedDocument>,
    pub attachments: Vec<OutlinePlannedAttachment>,
    pub diagnostics: Vec<OutlineDiagnostic>,
}

impl OutlinePublicationPlan {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineZipExportReport {
    pub path: String,
    pub dry_run: bool,
    pub wrote_archive: bool,
    #[serde(flatten)]
    pub plan: OutlinePublicationPlan,
}

#[derive(Debug, Clone)]
struct DocumentPathPlan {
    source_path: String,
    source_document_id: String,
    title: String,
    archive_path: String,
    parent_source_path: Option<String>,
    content: String,
}

pub fn plan_outline_publication(
    paths: &VaultPaths,
    collection_title: &str,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
) -> Result<OutlinePublicationPlan, AppError> {
    let collection_directory = serialize_outline_filename(collection_title.trim());
    let mut diagnostics = Vec::new();
    if collection_directory.is_empty() || !is_safe_relative_archive_path(&collection_directory) {
        diagnostics.push(OutlineDiagnostic {
            kind: OutlineDiagnosticKind::UnsafePath,
            source_path: None,
            target: Some(collection_title.to_string()),
            message: "collection title does not produce a safe Outline archive directory"
                .to_string(),
        });
    }

    let folder_notes = identify_folder_notes(notes, &mut diagnostics);
    let document_paths = plan_document_paths(
        &collection_directory,
        notes,
        &folder_notes,
        &mut diagnostics,
    );
    let selected_paths = document_paths
        .iter()
        .map(|document| document.source_path.clone())
        .collect::<BTreeSet<_>>();
    let attachment_sources =
        validate_links_and_collect_attachments(paths, links, &selected_paths, &mut diagnostics);
    let attachments = plan_attachments(
        paths,
        &collection_directory,
        &attachment_sources,
        &mut diagnostics,
    );
    validate_archive_collisions(&document_paths, &attachments, &mut diagnostics);

    let document_target_paths = document_paths
        .iter()
        .map(|document| (document.source_path.clone(), document.archive_path.clone()))
        .collect::<HashMap<_, _>>();
    let attachment_target_paths = attachments
        .iter()
        .map(|attachment| {
            (
                attachment.source_path.clone(),
                attachment.archive_path.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let links_by_source = group_links_by_source(links);

    let documents = document_paths
        .into_iter()
        .map(|document| {
            let rewritten = rewrite_document_links(
                &document,
                links_by_source
                    .get(&document.source_path)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
                &document_target_paths,
                &attachment_target_paths,
            );
            OutlinePlannedDocument {
                source_path: document.source_path,
                source_document_id: document.source_document_id,
                title: document.title,
                archive_path: document.archive_path,
                parent_source_path: document.parent_source_path,
                content_hash: blake3::hash(rewritten.as_bytes()).to_hex().to_string(),
                content: rewritten,
            }
        })
        .collect();

    diagnostics.sort_by(|left, right| {
        left.source_path
            .cmp(&right.source_path)
            .then(left.target.cmp(&right.target))
            .then(left.message.cmp(&right.message))
    });

    Ok(OutlinePublicationPlan {
        collection_title: collection_title.trim().to_string(),
        collection_directory,
        outline_version: SUPPORTED_OUTLINE_VERSION.to_string(),
        documents,
        attachments,
        diagnostics,
    })
}

pub fn write_outline_zip(
    paths: &VaultPaths,
    output_path: &Path,
    plan: OutlinePublicationPlan,
    dry_run: bool,
) -> Result<OutlineZipExportReport, AppError> {
    if !plan.is_valid() {
        return Ok(OutlineZipExportReport {
            path: output_path.display().to_string(),
            dry_run,
            wrote_archive: false,
            plan,
        });
    }
    if dry_run {
        return Ok(OutlineZipExportReport {
            path: output_path.display().to_string(),
            dry_run: true,
            wrote_archive: false,
            plan,
        });
    }
    if output_path.exists() {
        return Err(AppError::operation(format!(
            "refusing to overwrite existing Outline archive {}",
            output_path.display()
        )));
    }
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let temporary_path = temporary_archive_path(output_path);
    let result = write_outline_zip_file(paths, &temporary_path, &plan);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    fs::rename(&temporary_path, output_path).map_err(AppError::operation)?;

    Ok(OutlineZipExportReport {
        path: output_path.display().to_string(),
        dry_run: false,
        wrote_archive: true,
        plan,
    })
}

fn write_outline_zip_file(
    paths: &VaultPaths,
    output_path: &Path,
    plan: &OutlinePublicationPlan,
) -> Result<(), AppError> {
    let file = fs::File::create(output_path).map_err(AppError::operation)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut entries = BTreeMap::<String, Vec<u8>>::new();
    for document in &plan.documents {
        entries.insert(
            document.archive_path.clone(),
            document.content.as_bytes().to_vec(),
        );
    }
    for attachment in &plan.attachments {
        let bytes = fs::read(paths.vault_root().join(&attachment.source_path))
            .map_err(AppError::operation)?;
        entries.insert(attachment.archive_path.clone(), bytes);
    }
    for (archive_path, bytes) in entries {
        writer
            .start_file(archive_path, options)
            .map_err(AppError::operation)?;
        writer.write_all(&bytes).map_err(AppError::operation)?;
    }
    writer.finish().map_err(AppError::operation)?;
    Ok(())
}

fn identify_folder_notes(
    notes: &[ExportedNoteDocument],
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> BTreeMap<String, String> {
    let mut candidates = BTreeMap::<String, Vec<String>>::new();
    for note in notes {
        if let Some(folder) = folder_note_folder(&note.note.document_path) {
            candidates
                .entry(folder)
                .or_default()
                .push(note.note.document_path.clone());
        }
    }
    let mut folder_notes = BTreeMap::new();
    for (folder, mut paths) in candidates {
        paths.sort();
        if paths.len() > 1 {
            diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::DuplicateFolderNote,
                source_path: Some(paths.join(", ")),
                target: Some(folder.clone()),
                message: format!(
                    "folder `{folder}` has multiple folder notes; keep either index.md or the same-name note"
                ),
            });
        } else if let Some(path) = paths.pop() {
            folder_notes.insert(folder, path);
        }
    }
    folder_notes
}

fn plan_document_paths(
    collection_directory: &str,
    notes: &[ExportedNoteDocument],
    folder_notes: &BTreeMap<String, String>,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> Vec<DocumentPathPlan> {
    let mut planned = Vec::new();
    let mut sorted = notes.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.note.document_path.cmp(&right.note.document_path));
    for note in sorted {
        let normalized = normalize_path(&note.note.document_path);
        if !is_safe_relative_archive_path(&normalized) {
            diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::UnsafePath,
                source_path: Some(note.note.document_path.clone()),
                target: None,
                message: "note path is not a safe relative archive path".to_string(),
            });
            continue;
        }
        let folder = parent_path(&normalized);
        let folder_note = folder_note_folder(&normalized);
        let (logical_path, title, parent_folder) = if let Some(folder_note) = folder_note {
            let title = file_name(&folder_note).to_string();
            let parent = parent_path(&folder_note);
            (
                format!("{}.md", serialize_path(&folder_note)),
                title,
                parent,
            )
        } else {
            let stem = markdown_stem(file_name(&normalized));
            let logical_path = if folder.is_empty() {
                format!("{}.md", serialize_outline_filename(stem))
            } else {
                format!(
                    "{}/{}.md",
                    serialize_path(&folder),
                    serialize_outline_filename(stem)
                )
            };
            (logical_path, stem.to_string(), folder.clone())
        };

        let parent_source_path = if parent_folder.is_empty() {
            None
        } else if let Some(parent) = folder_notes.get(&parent_folder) {
            Some(parent.clone())
        } else {
            diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::MissingFolderNote,
                source_path: Some(note.note.document_path.clone()),
                target: Some(parent_folder.clone()),
                message: format!(
                    "folder `{parent_folder}` needs an included index.md or same-name folder note to preserve Outline hierarchy"
                ),
            });
            None
        };
        let archive_path = format!("{collection_directory}/{logical_path}");
        planned.push(DocumentPathPlan {
            source_path: note.note.document_path.clone(),
            source_document_id: note.note.document_id.clone(),
            title,
            archive_path,
            parent_source_path,
            content: note.content.clone(),
        });
    }
    planned.sort_by(|left, right| {
        left.archive_path
            .matches('/')
            .count()
            .cmp(&right.archive_path.matches('/').count())
            .then(left.archive_path.cmp(&right.archive_path))
    });
    planned
}

fn validate_links_and_collect_attachments(
    paths: &VaultPaths,
    links: &[ExportLinkRecord],
    selected_paths: &BTreeSet<String>,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> BTreeSet<String> {
    let mut attachments = BTreeSet::new();
    for link in links {
        if link.link_kind.eq_ignore_ascii_case("external") {
            continue;
        }
        if link.target_block.is_some() {
            diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::UnsupportedLink,
                source_path: Some(link.source_document_path.clone()),
                target: link.target_path_candidate.clone(),
                message: "Outline publication does not support Obsidian block-reference targets"
                    .to_string(),
            });
        }
        match (
            link.resolved_target_path.as_deref(),
            link.resolved_target_extension.as_deref(),
        ) {
            (Some(target), Some(extension)) if extension.eq_ignore_ascii_case("md") => {
                if !selected_paths.contains(target) {
                    diagnostics.push(OutlineDiagnostic {
                        kind: OutlineDiagnosticKind::ExcludedTarget,
                        source_path: Some(link.source_document_path.clone()),
                        target: Some(target.to_string()),
                        message: "link resolves to a note excluded from the publication query"
                            .to_string(),
                    });
                }
            }
            (Some(target), Some(_)) => {
                if paths.vault_root().join(target).is_file() {
                    attachments.insert(target.to_string());
                } else {
                    diagnostics.push(OutlineDiagnostic {
                        kind: OutlineDiagnosticKind::MissingAsset,
                        source_path: Some(link.source_document_path.clone()),
                        target: Some(target.to_string()),
                        message: "resolved attachment is missing from the vault".to_string(),
                    });
                }
            }
            _ => diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::UnresolvedLink,
                source_path: Some(link.source_document_path.clone()),
                target: link.target_path_candidate.clone(),
                message: "internal link could not be resolved".to_string(),
            }),
        }
    }
    attachments
}

fn plan_attachments(
    paths: &VaultPaths,
    collection_directory: &str,
    sources: &BTreeSet<String>,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> Vec<OutlinePlannedAttachment> {
    sources
        .iter()
        .filter_map(|source_path| {
            let absolute = paths.vault_root().join(source_path);
            let bytes = match fs::read(&absolute) {
                Ok(bytes) => bytes,
                Err(error) => {
                    diagnostics.push(OutlineDiagnostic {
                        kind: OutlineDiagnosticKind::MissingAsset,
                        source_path: None,
                        target: Some(source_path.clone()),
                        message: format!("failed to read attachment: {error}"),
                    });
                    return None;
                }
            };
            let name = serialize_outline_filename(file_name(source_path));
            let identity_hash = blake3::hash(source_path.as_bytes()).to_hex().to_string();
            Some(OutlinePlannedAttachment {
                source_path: source_path.clone(),
                archive_path: format!(
                    "{collection_directory}/uploads/{}/{name}",
                    &identity_hash[..16]
                ),
                content_hash: blake3::hash(&bytes).to_hex().to_string(),
                size: bytes.len() as u64,
            })
        })
        .collect()
}

fn validate_archive_collisions(
    documents: &[DocumentPathPlan],
    attachments: &[OutlinePlannedAttachment],
    diagnostics: &mut Vec<OutlineDiagnostic>,
) {
    let mut seen = BTreeMap::<String, (String, String)>::new();
    for (archive_path, source_path, kind) in documents
        .iter()
        .map(|entry| (&entry.archive_path, &entry.source_path, "document"))
        .chain(
            attachments
                .iter()
                .map(|entry| (&entry.archive_path, &entry.source_path, "attachment")),
        )
    {
        let key = archive_path.to_lowercase();
        if let Some((existing_path, existing_kind)) = seen.get(&key) {
            diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::Collision,
                source_path: Some(source_path.clone()),
                target: Some(archive_path.clone()),
                message: format!(
                    "case-insensitive archive collision with {existing_kind} `{existing_path}`"
                ),
            });
        } else {
            seen.insert(key, (source_path.clone(), kind.to_string()));
        }
    }
}

fn group_links_by_source<'a>(
    links: &'a [ExportLinkRecord],
) -> HashMap<String, Vec<&'a ExportLinkRecord>> {
    let mut grouped = HashMap::<String, Vec<&ExportLinkRecord>>::new();
    for link in links {
        grouped
            .entry(link.source_document_path.clone())
            .or_default()
            .push(link);
    }
    for values in grouped.values_mut() {
        values.sort_by_key(|link| link.byte_offset);
    }
    grouped
}

fn rewrite_document_links(
    document: &DocumentPathPlan,
    links: &[&ExportLinkRecord],
    document_targets: &HashMap<String, String>,
    attachment_targets: &HashMap<String, String>,
) -> String {
    let mut replacements = links
        .iter()
        .filter_map(|link| {
            let target = link.resolved_target_path.as_ref()?;
            let archive_target = document_targets
                .get(target)
                .or_else(|| attachment_targets.get(target))?;
            let start = usize::try_from(link.byte_offset).ok()?;
            let end = start.checked_add(link.raw_text.len())?;
            if end > document.content.len() || !document.content.is_char_boundary(start) {
                return None;
            }
            let relative = relative_archive_path(&document.archive_path, archive_target);
            let mut href = encode_uri_path(&relative);
            if let Some(heading) = link.target_heading.as_deref() {
                href.push('#');
                href.push_str(&encode_uri_fragment(heading));
            }
            let label = link
                .display_text
                .as_deref()
                .or(link.target_path_candidate.as_deref())
                .unwrap_or(target);
            let is_attachment = attachment_targets.contains_key(target);
            let replacement = if link.link_kind.eq_ignore_ascii_case("embed") && is_attachment {
                format!("![{}]({href})", escape_markdown_label(label))
            } else {
                format!("[{}]({href})", escape_markdown_label(label))
            };
            Some((start, end, replacement))
        })
        .collect::<Vec<_>>();
    replacements.sort_by_key(|(start, _, _)| *start);
    replacements.reverse();
    let mut content = document.content.clone();
    for (start, end, replacement) in replacements {
        content.replace_range(start..end, &replacement);
    }
    content
}

fn folder_note_folder(path: &str) -> Option<String> {
    let normalized = normalize_path(path);
    let folder = parent_path(&normalized);
    if folder.is_empty() {
        return None;
    }
    let name = file_name(&normalized);
    if name.eq_ignore_ascii_case("index.md") {
        return Some(folder);
    }
    let folder_name = file_name(&folder);
    markdown_stem(name)
        .eq_ignore_ascii_case(folder_name)
        .then_some(folder)
}

fn serialize_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(serialize_outline_filename)
        .collect::<Vec<_>>()
        .join("/")
}

#[must_use]
pub fn serialize_outline_filename(value: &str) -> String {
    let mut encoded = String::new();
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
        ) {
            for byte in character.to_string().as_bytes() {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        } else {
            encoded.push(character);
        }
    }
    let trailing_start = encoded.trim_end_matches(['.', ' ']).len();
    let trailing = encoded[trailing_start..].to_string();
    encoded.truncate(trailing_start);
    for byte in trailing.as_bytes() {
        encoded.push_str(&format!("%{byte:02X}"));
    }
    encoded
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| parent.to_string())
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn markdown_stem(name: &str) -> &str {
    name.strip_suffix(".md").unwrap_or(name)
}

fn is_safe_relative_archive_path(path: &str) -> bool {
    !path.is_empty()
        && !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn relative_archive_path(from_file: &str, to_file: &str) -> String {
    let from_parent = parent_path(from_file);
    let from = from_parent
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let to = to_file
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = vec![".."; from.len().saturating_sub(common)];
    result.extend(to[common..].iter().copied());
    if result.is_empty() {
        ".".to_string()
    } else {
        result.join("/")
    }
}

fn encode_uri_path(value: &str) -> String {
    percent_encode(value, true)
}

fn encode_uri_fragment(value: &str) -> String {
    percent_encode(value, false)
}

fn percent_encode(value: &str, allow_slash: bool) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        let allowed = byte.is_ascii_alphanumeric()
            || matches!(*byte, b'-' | b'_' | b'.' | b'~')
            || (allow_slash && *byte == b'/');
        if allowed {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn escape_markdown_label(value: &str) -> String {
    value.replace('\\', "\\\\").replace(']', "\\]")
}

fn temporary_archive_path(output_path: &Path) -> PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("outline.zip");
    output_path.with_file_name(format!(".{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{execute_export_query, prepare_export_data};
    use std::io::Read;
    use tempfile::{tempdir, TempDir};
    use vulcan_core::{scan_vault, ScanMode};
    use zip::ZipArchive;

    fn outline_vault(files: &[(&str, &[u8])]) -> (TempDir, VaultPaths) {
        let temp = tempdir().expect("temporary directory");
        let root = temp.path().join("vault");
        fs::create_dir_all(root.join(".vulcan")).expect("vulcan directory");
        for (path, content) in files {
            let absolute = root.join(path);
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).expect("fixture parent");
            }
            fs::write(absolute, content).expect("fixture file");
        }
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).expect("fixture scan");
        (temp, paths)
    }

    fn plan_all(paths: &VaultPaths) -> OutlinePublicationPlan {
        let report =
            execute_export_query(paths, Some("from notes"), None, None).expect("query all notes");
        let prepared =
            prepare_export_data(paths, &report, None, None).expect("prepare publication");
        plan_outline_publication(paths, "Wiki", &prepared.notes, &prepared.links)
            .expect("plan Outline publication")
    }

    #[test]
    fn outline_filename_serialization_matches_upstream_windows_rules() {
        assert_eq!(serialize_outline_filename("A:B? "), "A%3AB%3F%20");
        assert_eq!(serialize_outline_filename("Guide.md"), "Guide.md");
    }

    #[test]
    fn relative_paths_are_calculated_from_document_directory() {
        assert_eq!(
            relative_archive_path("Wiki/Projects/Child.md", "Wiki/Projects.md"),
            "../Projects.md"
        );
        assert_eq!(
            relative_archive_path("Wiki/Projects.md", "Wiki/Projects/Child.md"),
            "Projects/Child.md"
        );
    }

    #[test]
    fn plans_same_name_and_index_folder_notes_as_outline_siblings() {
        let (_temp, paths) = outline_vault(&[
            ("Projects/Projects.md", b"# Projects\n"),
            ("Projects/Child.md", b"# Child\n"),
            ("Guides/index.md", b"# Guides\n"),
            ("Guides/Start.md", b"# Start\n"),
        ]);
        let plan = plan_all(&paths);
        assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
        let paths = plan
            .documents
            .iter()
            .map(|document| document.archive_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "Wiki/Guides.md",
                "Wiki/Projects.md",
                "Wiki/Guides/Start.md",
                "Wiki/Projects/Child.md",
            ]
        );
        let child = plan
            .documents
            .iter()
            .find(|document| document.source_path == "Projects/Child.md")
            .expect("child document");
        assert_eq!(
            child.parent_source_path.as_deref(),
            Some("Projects/Projects.md")
        );
    }

    #[test]
    fn plans_nested_folder_notes_links_embeds_and_deterministic_attachments() {
        let (_temp, paths) = outline_vault(&[
            (
                "Projects/Projects.md",
                b"# Projects\n\n[[Projects/Child]]\n",
            ),
            ("Projects/Child.md", b"# Child\n\n![[assets/logo.png]]\n"),
            ("Projects/Deep/index.md", b"# Deep\n"),
            ("Projects/Deep/Leaf.md", b"# Leaf\n\n[[../Child]]\n"),
            ("assets/logo.png", b"png bytes"),
        ]);
        let first = plan_all(&paths);
        let second = plan_all(&paths);
        assert_eq!(first, second);
        assert!(first.diagnostics.is_empty(), "{:?}", first.diagnostics);
        assert_eq!(first.attachments.len(), 1);
        assert!(first.attachments[0]
            .archive_path
            .starts_with("Wiki/uploads/"));
        let projects = first
            .documents
            .iter()
            .find(|document| document.source_path == "Projects/Projects.md")
            .expect("projects document");
        assert!(projects
            .content
            .contains("[Projects/Child](Projects/Child.md)"));
        let child = first
            .documents
            .iter()
            .find(|document| document.source_path == "Projects/Child.md")
            .expect("child document");
        assert!(child.content.contains("![assets/logo.png](../uploads/"));
        let leaf = first
            .documents
            .iter()
            .find(|document| document.source_path == "Projects/Deep/Leaf.md")
            .expect("leaf document");
        assert_eq!(
            leaf.parent_source_path.as_deref(),
            Some("Projects/Deep/index.md")
        );
        assert!(leaf.content.contains("[../Child](../Child.md)"));
    }

    #[test]
    fn reports_duplicate_folder_notes_case_collisions_and_excluded_targets() {
        let (_temp, paths) = outline_vault(&[
            ("Projects/index.md", b"# Index\n"),
            ("Projects/Projects.md", b"# Projects\n"),
            ("Projects/Readme.md", b"[[Secret]]\n"),
            ("Projects/README.md", b"# duplicate\n"),
            ("Secret.md", b"# Secret\n"),
        ]);
        let report = execute_export_query(
            &paths,
            Some(r#"from notes where file.path matches "^Projects/""#),
            None,
            None,
        )
        .expect("query projects");
        let prepared = prepare_export_data(&paths, &report, None, None).expect("prepare projects");
        let plan = plan_outline_publication(&paths, "Wiki", &prepared.notes, &prepared.links)
            .expect("plan projects");
        let kinds = plan
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.kind.clone())
            .collect::<Vec<_>>();
        assert!(kinds.contains(&OutlineDiagnosticKind::DuplicateFolderNote));
        assert!(kinds.contains(&OutlineDiagnosticKind::Collision));
        assert!(kinds.contains(&OutlineDiagnosticKind::ExcludedTarget));
    }

    #[test]
    fn reports_missing_and_unresolved_assets() {
        let (_temp, paths) = outline_vault(&[
            ("Home.md", b"![[assets/gone.png]]\n![[assets/never.png]]\n"),
            ("assets/gone.png", b"gone"),
        ]);
        fs::remove_file(paths.vault_root().join("assets/gone.png")).expect("remove cached asset");
        let plan = plan_all(&paths);
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == OutlineDiagnosticKind::MissingAsset));
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == OutlineDiagnosticKind::UnresolvedLink));
    }

    #[test]
    fn dry_run_is_mutation_free_and_zip_layout_matches_plan() {
        let (_temp, paths) = outline_vault(&[
            ("Projects/Projects.md", b"# Projects\n"),
            ("Projects/Child.md", b"# Child\n"),
        ]);
        let output = paths.vault_root().join("exports/wiki.zip");
        let dry_run =
            write_outline_zip(&paths, &output, plan_all(&paths), true).expect("dry-run export");
        assert!(!dry_run.wrote_archive);
        assert!(!output.exists());

        let report =
            write_outline_zip(&paths, &output, plan_all(&paths), false).expect("write export");
        assert!(report.wrote_archive);
        let file = fs::File::open(&output).expect("open archive");
        let mut archive = ZipArchive::new(file).expect("read archive");
        let mut names = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .expect("archive entry")
                    .name()
                    .to_string()
            })
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec!["Wiki/Projects.md", "Wiki/Projects/Child.md"]);
        let mut child = String::new();
        archive
            .by_name("Wiki/Projects/Child.md")
            .expect("child entry")
            .read_to_string(&mut child)
            .expect("read child");
        assert_eq!(child, "# Child\n");
    }
}
