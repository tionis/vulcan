use super::{ExportLinkRecord, ExportedNoteDocument};
use crate::outline_markdown::{outline_document_url, rewrite_markdown_link_destinations};
use crate::AppError;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use vulcan_core::config::load_vault_config;
use vulcan_core::config::OutlineBlockReferencePolicyConfig;
use vulcan_core::config::OutlineExcludedTargetPolicyConfig;
use vulcan_core::folder_notes::FolderNotesConfig;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<OutlineBlockReferencePolicyConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_target_policy: Option<OutlineExcludedTargetPolicyConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<OutlineDiagnosticAction>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutlineDiagnosticAction {
    RenderedPlainText,
    RerunWithPlainText,
}

impl OutlineDiagnostic {
    #[must_use]
    pub fn is_warning(&self) -> bool {
        self.kind == OutlineDiagnosticKind::MissingFolderNote
            || self.action == Some(OutlineDiagnosticAction::RenderedPlainText)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutlinePublicationOptions {
    pub block_reference_policy: OutlineBlockReferencePolicyConfig,
    pub excluded_target_policy: OutlineExcludedTargetPolicyConfig,
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
        self.diagnostics.iter().all(OutlineDiagnostic::is_warning)
    }
}

#[must_use]
pub fn render_remote_document_content(
    document: &OutlinePlannedDocument,
    attachments: &[OutlinePlannedAttachment],
    remote_urls: &BTreeMap<String, String>,
) -> String {
    let mut content = document.content.clone();
    for attachment in attachments {
        let Some(remote_url) = remote_urls.get(&attachment.source_path) else {
            continue;
        };
        let relative = relative_archive_path(&document.archive_path, &attachment.archive_path);
        let href = encode_uri_path(&relative);
        content = content.replace(&format!("]({href})"), &format!("]({remote_url})"));
    }
    content
}

#[must_use]
pub fn render_remote_document_content_with_links(
    document: &OutlinePlannedDocument,
    documents: &[OutlinePlannedDocument],
    remote_document_ids: &BTreeMap<String, String>,
    attachments: &[OutlinePlannedAttachment],
    remote_attachment_urls: &BTreeMap<String, String>,
) -> String {
    let content = render_remote_document_content(document, attachments, remote_attachment_urls);
    let destinations = documents
        .iter()
        .filter_map(|target| {
            let remote_id = remote_document_ids.get(&target.source_path)?;
            let relative = relative_archive_path(&document.archive_path, &target.archive_path);
            Some((encode_uri_path(&relative), outline_document_url(remote_id)))
        })
        .collect::<BTreeMap<_, _>>();

    rewrite_markdown_link_destinations(&content, |destination| {
        let (path, fragment) = destination
            .split_once('#')
            .map_or((destination, None), |(path, fragment)| {
                (path, Some(fragment))
            });
        let remote = destinations.get(path)?;
        Some(fragment.map_or_else(|| remote.clone(), |fragment| format!("{remote}#{fragment}")))
    })
}

#[must_use]
pub fn planned_document_references_attachment(
    document: &OutlinePlannedDocument,
    attachment: &OutlinePlannedAttachment,
) -> bool {
    let relative = relative_archive_path(&document.archive_path, &attachment.archive_path);
    document
        .content
        .contains(&format!("]({})", encode_uri_path(&relative)))
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
    plan_outline_publication_with_options(
        paths,
        collection_title,
        notes,
        links,
        OutlinePublicationOptions::default(),
    )
}

pub fn plan_outline_publication_with_options(
    paths: &VaultPaths,
    collection_title: &str,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
    options: OutlinePublicationOptions,
) -> Result<OutlinePublicationPlan, AppError> {
    let collection_directory = serialize_outline_filename(collection_title.trim());
    let mut diagnostics = collection_path_diagnostics(collection_title, &collection_directory);

    let folder_notes_config = load_vault_config(paths).config.folder_notes;
    let mut folder_notes = identify_folder_notes(notes, &folder_notes_config, &mut diagnostics);
    let generated_folder_notes = complete_folder_note_hierarchy(
        notes,
        &mut folder_notes,
        &folder_notes_config,
        &mut diagnostics,
    );
    let document_paths = plan_document_paths(
        &collection_directory,
        notes,
        &folder_notes,
        &generated_folder_notes,
        &folder_notes_config,
        &mut diagnostics,
    );
    let selected_paths = document_paths
        .iter()
        .map(|document| document.source_path.clone())
        .collect::<BTreeSet<_>>();
    let document_contents = document_paths
        .iter()
        .map(|document| (document.source_path.as_str(), document.content.as_str()))
        .collect::<HashMap<_, _>>();
    let attachment_sources = validate_links_and_collect_attachments(
        paths,
        links,
        &selected_paths,
        &document_contents,
        options.block_reference_policy,
        options.excluded_target_policy,
        &mut diagnostics,
    );
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
                options.block_reference_policy,
                options.excluded_target_policy,
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

fn collection_path_diagnostics(
    collection_title: &str,
    collection_directory: &str,
) -> Vec<OutlineDiagnostic> {
    if collection_directory.is_empty() || !is_safe_relative_archive_path(collection_directory) {
        vec![OutlineDiagnostic {
            kind: OutlineDiagnosticKind::UnsafePath,
            source_path: None,
            target: Some(collection_title.to_string()),
            line: None,
            column: None,
            byte_offset: None,
            policy: None,
            excluded_target_policy: None,
            action: None,
            message: "collection title does not produce a safe Outline archive directory"
                .to_string(),
        }]
    } else {
        Vec::new()
    }
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
    config: &FolderNotesConfig,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> BTreeMap<String, String> {
    let mut candidates = BTreeMap::<String, Vec<String>>::new();
    for note in notes {
        if let Some(folder) = config.folder_for_note_path(&note.note.document_path) {
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
                line: None,
                column: None,
                byte_offset: None,
                policy: None,
                excluded_target_policy: None,
                action: None,
                message: format!(
                    "folder `{folder}` has multiple notes matching the configured folder-note convention"
                ),
            });
        } else if let Some(path) = paths.pop() {
            folder_notes.insert(folder, path);
        }
    }
    folder_notes
}

fn complete_folder_note_hierarchy(
    notes: &[ExportedNoteDocument],
    folder_notes: &mut BTreeMap<String, String>,
    config: &FolderNotesConfig,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> BTreeMap<String, String> {
    let mut required_folders = BTreeSet::new();
    for note in notes {
        let normalized = normalize_path(&note.note.document_path);
        let logical_parent = config
            .folder_for_note_path(&normalized)
            .map_or_else(|| parent_path(&normalized), |folder| parent_path(&folder));
        let mut folder = logical_parent;
        while !folder.is_empty() {
            required_folders.insert(folder.clone());
            folder = parent_path(&folder);
        }
    }

    let mut generated = BTreeMap::new();
    for folder in required_folders {
        if folder_notes.contains_key(&folder) {
            continue;
        }
        let Some(source_path) = config.note_path_for_folder(&folder) else {
            continue;
        };
        folder_notes.insert(folder.clone(), source_path.clone());
        generated.insert(folder.clone(), source_path.clone());
        diagnostics.push(OutlineDiagnostic {
            kind: OutlineDiagnosticKind::MissingFolderNote,
            source_path: Some(source_path),
            target: Some(folder.clone()),
            line: None,
            column: None,
            byte_offset: None,
            policy: None,
            excluded_target_policy: None,
            action: None,
            message: format!(
                "folder `{folder}` has no selected configured folder note; generated an export-only placeholder"
            ),
        });
    }
    generated
}

fn plan_document_paths(
    collection_directory: &str,
    notes: &[ExportedNoteDocument],
    folder_notes: &BTreeMap<String, String>,
    generated_folder_notes: &BTreeMap<String, String>,
    folder_notes_config: &FolderNotesConfig,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> Vec<DocumentPathPlan> {
    let mut planned = Vec::new();
    for (folder, source_path) in generated_folder_notes {
        let title = file_name(folder).to_string();
        let parent_folder = parent_path(folder);
        planned.push(DocumentPathPlan {
            source_path: source_path.clone(),
            source_document_id: format!("generated-outline-folder-note:{source_path}"),
            title: title.clone(),
            archive_path: format!("{collection_directory}/{}.md", serialize_path(folder)),
            parent_source_path: (!parent_folder.is_empty())
                .then(|| folder_notes.get(&parent_folder).cloned())
                .flatten(),
            content: format!("# {title}\n"),
        });
    }
    let mut sorted = notes.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.note.document_path.cmp(&right.note.document_path));
    for note in sorted {
        let normalized = normalize_path(&note.note.document_path);
        if !is_safe_relative_archive_path(&normalized) {
            diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::UnsafePath,
                source_path: Some(note.note.document_path.clone()),
                target: None,
                line: None,
                column: None,
                byte_offset: None,
                policy: None,
                excluded_target_policy: None,
                action: None,
                message: "note path is not a safe relative archive path".to_string(),
            });
            continue;
        }
        let folder = parent_path(&normalized);
        let folder_note = folder_notes_config.folder_for_note_path(&normalized);
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

        let parent_source_path = (!parent_folder.is_empty())
            .then(|| folder_notes.get(&parent_folder).cloned())
            .flatten();
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
    document_contents: &HashMap<&str, &str>,
    block_reference_policy: OutlineBlockReferencePolicyConfig,
    excluded_target_policy: OutlineExcludedTargetPolicyConfig,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) -> BTreeSet<String> {
    let mut attachments = BTreeSet::new();
    for link in links {
        if link.link_kind.eq_ignore_ascii_case("external") {
            continue;
        }
        if let Some(block) = link.target_block.as_deref() {
            let target = link
                .target_path_candidate
                .as_deref()
                .map_or_else(|| format!("#^{block}"), |path| format!("{path}#^{block}"));
            let offset = usize::try_from(link.byte_offset).ok();
            let (line, column) = offset
                .and_then(|offset| {
                    document_contents
                        .get(link.source_document_path.as_str())
                        .map(|content| line_column_for_offset(content, offset))
                })
                .map_or((None, None), |(line, column)| (Some(line), Some(column)));
            let (action, message) = match block_reference_policy {
                OutlineBlockReferencePolicyConfig::Error => (
                    OutlineDiagnosticAction::RerunWithPlainText,
                    "Outline cannot represent Obsidian block-reference targets".to_string(),
                ),
                OutlineBlockReferencePolicyConfig::PlainText => (
                    OutlineDiagnosticAction::RenderedPlainText,
                    "rendered Obsidian block-reference link as plain text for Outline".to_string(),
                ),
            };
            diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::UnsupportedLink,
                source_path: Some(link.source_document_path.clone()),
                target: Some(target),
                line,
                column,
                byte_offset: offset,
                policy: Some(block_reference_policy),
                excluded_target_policy: None,
                action: Some(action),
                message,
            });
            continue;
        }
        match (
            link.resolved_target_path.as_deref(),
            link.resolved_target_extension.as_deref(),
        ) {
            (Some(target), Some(extension)) if extension.eq_ignore_ascii_case("md") => {
                if !selected_paths.contains(target) {
                    push_excluded_target_diagnostic(
                        link,
                        target,
                        document_contents,
                        excluded_target_policy,
                        diagnostics,
                    );
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
                        line: None,
                        column: None,
                        byte_offset: None,
                        policy: None,
                        excluded_target_policy: None,
                        action: None,
                        message: "resolved attachment is missing from the vault".to_string(),
                    });
                }
            }
            _ => diagnostics.push(OutlineDiagnostic {
                kind: OutlineDiagnosticKind::UnresolvedLink,
                source_path: Some(link.source_document_path.clone()),
                target: link.target_path_candidate.clone(),
                line: None,
                column: None,
                byte_offset: None,
                policy: None,
                excluded_target_policy: None,
                action: None,
                message: "internal link could not be resolved".to_string(),
            }),
        }
    }
    attachments
}

fn push_excluded_target_diagnostic(
    link: &ExportLinkRecord,
    target: &str,
    document_contents: &HashMap<&str, &str>,
    policy: OutlineExcludedTargetPolicyConfig,
    diagnostics: &mut Vec<OutlineDiagnostic>,
) {
    let offset = usize::try_from(link.byte_offset).ok();
    let (source_line, source_column) = offset
        .and_then(|offset| {
            document_contents
                .get(link.source_document_path.as_str())
                .map(|content| line_column_for_offset(content, offset))
        })
        .map_or((None, None), |(line, column)| (Some(line), Some(column)));
    let (action, message) = match policy {
        OutlineExcludedTargetPolicyConfig::Error => (
            OutlineDiagnosticAction::RerunWithPlainText,
            "link resolves to a note excluded from the publication query".to_string(),
        ),
        OutlineExcludedTargetPolicyConfig::PlainText => (
            OutlineDiagnosticAction::RenderedPlainText,
            "rendered link to an excluded note as plain text for Outline".to_string(),
        ),
    };
    diagnostics.push(OutlineDiagnostic {
        kind: OutlineDiagnosticKind::ExcludedTarget,
        source_path: Some(link.source_document_path.clone()),
        target: Some(target.to_string()),
        line: source_line,
        column: source_column,
        byte_offset: offset,
        policy: None,
        excluded_target_policy: Some(policy),
        action: Some(action),
        message,
    });
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
                        line: None,
                        column: None,
                        byte_offset: None,
                        policy: None,
                        excluded_target_policy: None,
                        action: None,
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
                line: None,
                column: None,
                byte_offset: None,
                policy: None,
                excluded_target_policy: None,
                action: None,
                message: format!(
                    "case-insensitive archive collision with {existing_kind} `{existing_path}`"
                ),
            });
        } else {
            seen.insert(key, (source_path.clone(), kind.to_string()));
        }
    }
}

fn group_links_by_source(links: &[ExportLinkRecord]) -> HashMap<String, Vec<&ExportLinkRecord>> {
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
    block_reference_policy: OutlineBlockReferencePolicyConfig,
    excluded_target_policy: OutlineExcludedTargetPolicyConfig,
) -> String {
    let mut replacements = links
        .iter()
        .filter_map(|link| {
            if link.target_block.is_some()
                && block_reference_policy == OutlineBlockReferencePolicyConfig::PlainText
            {
                let start = usize::try_from(link.byte_offset).ok()?;
                let end = start.checked_add(link.raw_text.len())?;
                if end > document.content.len() || !document.content.is_char_boundary(start) {
                    return None;
                }
                return Some((start, end, link_plain_text(link)));
            }
            let target = link.resolved_target_path.as_ref()?;
            let archive_target = document_targets
                .get(target)
                .or_else(|| attachment_targets.get(target));
            let start = usize::try_from(link.byte_offset).ok()?;
            let end = start.checked_add(link.raw_text.len())?;
            if end > document.content.len() || !document.content.is_char_boundary(start) {
                return None;
            }
            if archive_target.is_none()
                && link
                    .resolved_target_extension
                    .as_deref()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                && excluded_target_policy == OutlineExcludedTargetPolicyConfig::PlainText
            {
                return Some((start, end, link_plain_text(link)));
            }
            let archive_target = archive_target?;
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

fn link_plain_text(link: &ExportLinkRecord) -> String {
    let label = if link.link_kind.eq_ignore_ascii_case("markdown") {
        link.display_text.as_deref().unwrap_or_default()
    } else {
        link.display_text
            .as_deref()
            .or(link.target_path_candidate.as_deref())
            .or(link.target_block.as_deref())
            .unwrap_or_default()
    };
    escape_markdown_plain_text(label)
}

fn escape_markdown_plain_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

fn line_column_for_offset(content: &str, byte_offset: usize) -> (usize, usize) {
    let offset = byte_offset.min(content.len());
    let line = content[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let line_start = content[..offset]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let column = content[line_start..offset].chars().count() + 1;
    (line, column)
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
                let _ = write!(encoded, "%{byte:02X}");
            }
        } else {
            encoded.push(character);
        }
    }
    let trailing_start = encoded.trim_end_matches(['.', ' ']).len();
    let trailing = encoded[trailing_start..].to_string();
    encoded.truncate(trailing_start);
    for byte in trailing.as_bytes() {
        let _ = write!(encoded, "%{byte:02X}");
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
            let _ = write!(encoded, "%{byte:02X}");
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
    use crate::export::{execute_export_query, prepare_outline_export_data};
    use crate::outline_markdown::OutlineMarkdownOptions;
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
        plan_all_with_policy(paths, OutlineBlockReferencePolicyConfig::Error)
    }

    fn plan_all_with_policy(
        paths: &VaultPaths,
        block_reference_policy: OutlineBlockReferencePolicyConfig,
    ) -> OutlinePublicationPlan {
        let report =
            execute_export_query(paths, Some("from notes"), None, None).expect("query all notes");
        let prepared = prepare_outline_export_data(
            paths,
            &report,
            None,
            None,
            OutlineMarkdownOptions::default(),
        )
        .expect("prepare publication");
        plan_outline_publication_with_options(
            paths,
            "Wiki",
            &prepared.notes,
            &prepared.links,
            OutlinePublicationOptions {
                block_reference_policy,
                excluded_target_policy: OutlineExcludedTargetPolicyConfig::Error,
            },
        )
        .expect("plan Outline publication")
    }

    fn configure_folder_notes(paths: &VaultPaths, placement: &str, name: &str) {
        fs::write(
            paths.config_file(),
            format!("[folder_notes]\nplacement = \"{placement}\"\nname = \"{name}\"\n"),
        )
        .expect("folder-note config");
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
    fn outline_preparation_strips_frontmatter_and_converts_callouts() {
        let (_temp, paths) = outline_vault(&[
            (
                "Home.md",
                b"---\ntags: [internal]\ncover: '[[secret.png]]'\n---\n# Home\n\n> [!WARNING] Careful\n> Published body\n",
            ),
            ("secret.png", b"not published"),
        ]);

        let plan = plan_all(&paths);
        let content = &plan.documents[0].content;
        assert!(!content.contains("tags:"));
        assert!(!content.contains("> [!WARNING]"));
        assert!(content.contains(":::warning\nCareful\nPublished body\n\n:::"));
        assert!(plan.attachments.is_empty());
    }

    #[test]
    fn block_reference_policy_preserves_strict_mode_and_can_render_labels_as_plain_text() {
        let source = b"# Home\n\n[label](#^block-9-0) and [again](#^block-9-0)\n\n^block-9-0\n\n## Section\n\n[heading](#Section) and [[Target|note]] and [[Target#^remote-block|remote label]]\n";
        let (_temp, paths) = outline_vault(&[
            ("Home.md", source),
            (
                "Target.md",
                b"# Target\n\n## Heading\n\nRemote text.\n\n^remote-block\n",
            ),
        ]);

        let strict = plan_all(&paths);
        assert!(!strict.is_valid());
        let strict_blocks = strict
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == OutlineDiagnosticKind::UnsupportedLink)
            .collect::<Vec<_>>();
        assert_eq!(strict_blocks.len(), 3);
        assert!(strict_blocks.iter().all(|diagnostic| {
            diagnostic.policy == Some(OutlineBlockReferencePolicyConfig::Error)
                && diagnostic.action == Some(OutlineDiagnosticAction::RerunWithPlainText)
                && diagnostic
                    .target
                    .as_deref()
                    .is_some_and(|target| !target.is_empty())
                && diagnostic.line.is_some()
                && diagnostic.column.is_some()
                && diagnostic.byte_offset.is_some()
        }));
        assert_eq!(strict_blocks[0].target.as_deref(), Some("#^block-9-0"));
        assert_eq!(
            strict_blocks[2].target.as_deref(),
            Some("Target#^remote-block")
        );

        let downgraded = plan_all_with_policy(&paths, OutlineBlockReferencePolicyConfig::PlainText);
        assert!(downgraded.is_valid(), "{:?}", downgraded.diagnostics);
        let home = downgraded
            .documents
            .iter()
            .find(|document| document.source_path == "Home.md")
            .expect("home document");
        assert!(home.content.contains("label and again"));
        assert!(home.content.contains("remote label"));
        assert!(!home.content.contains("#^block-9-0"));
        assert!(!home.content.contains("Target#^remote-block"));
        assert!(home.content.contains("[heading](Home.md#Section)"));
        assert!(home.content.contains("[note](Target.md)"));
        assert_eq!(
            fs::read(paths.vault_root().join("Home.md")).unwrap(),
            source
        );
    }

    #[test]
    fn plain_text_block_reference_policy_supports_dry_run_and_zip_creation() {
        let source =
            b"| Topic |\n| --- |\n| [Welcome](#^block-9-0) |\n\n^block-9-0\n\nWelcome text.\n";
        let (_temp, paths) = outline_vault(&[("Home.md", source)]);
        let output = paths.vault_root().join("exports/wiki.zip");

        let dry_run = write_outline_zip(
            &paths,
            &output,
            plan_all_with_policy(&paths, OutlineBlockReferencePolicyConfig::PlainText),
            true,
        )
        .expect("dry run");
        assert!(dry_run.plan.is_valid());
        assert!(!dry_run.wrote_archive);
        assert!(!output.exists());

        let written = write_outline_zip(
            &paths,
            &output,
            plan_all_with_policy(&paths, OutlineBlockReferencePolicyConfig::PlainText),
            false,
        )
        .expect("ZIP export");
        assert!(written.wrote_archive);
        let file = fs::File::open(&output).expect("ZIP exists");
        let mut archive = ZipArchive::new(file).expect("ZIP opens");
        let mut markdown = String::new();
        archive
            .by_name("Wiki/Home.md")
            .expect("exported note")
            .read_to_string(&mut markdown)
            .expect("read exported note");
        assert!(markdown.contains("| Welcome |"));
        assert!(!markdown.contains("#^block-9-0"));
        assert_eq!(
            fs::read(paths.vault_root().join("Home.md")).unwrap(),
            source
        );
    }

    #[test]
    fn plans_configured_same_name_folder_notes_as_outline_siblings() {
        let (_temp, paths) = outline_vault(&[
            ("Projects/Projects.md", b"# Projects\n"),
            ("Projects/Child.md", b"# Child\n"),
            ("Guides/Guides.md", b"# Guides\n"),
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
    fn configured_index_folder_notes_are_not_auto_detected() {
        let (_temp, paths) = outline_vault(&[
            ("Guides/index.md", b"# Guides\n"),
            ("Guides/Start.md", b"# Start\n"),
        ]);
        let unconfigured = plan_all(&paths);
        assert!(unconfigured.is_valid());
        assert_eq!(unconfigured.diagnostics.len(), 1);
        assert_eq!(
            unconfigured.diagnostics[0].kind,
            OutlineDiagnosticKind::MissingFolderNote
        );
        assert!(unconfigured
            .documents
            .iter()
            .any(|document| document.source_path == "Guides/Guides.md"));

        configure_folder_notes(&paths, "inside", "index");
        let configured = plan_all(&paths);
        assert!(
            configured.diagnostics.is_empty(),
            "{:?}",
            configured.diagnostics
        );
        assert_eq!(configured.documents[0].archive_path, "Wiki/Guides.md");
        assert_eq!(configured.documents[1].archive_path, "Wiki/Guides/Start.md");
    }

    #[test]
    fn generates_one_placeholder_and_warning_per_missing_folder() {
        let (_temp, paths) = outline_vault(&[
            ("Pantheons/Greek/Zeus.md", b"# Zeus\n"),
            ("Pantheons/Greek/Hera.md", b"# Hera\n"),
        ]);

        let plan = plan_all(&paths);

        assert!(plan.is_valid(), "{:?}", plan.diagnostics);
        assert_eq!(plan.diagnostics.len(), 2);
        assert!(plan.diagnostics.iter().all(|diagnostic| {
            diagnostic.kind == OutlineDiagnosticKind::MissingFolderNote
                && diagnostic.message.contains("export-only placeholder")
        }));
        let pantheons = plan
            .documents
            .iter()
            .find(|document| document.source_path == "Pantheons/Pantheons.md")
            .expect("generated top-level folder note");
        assert_eq!(pantheons.archive_path, "Wiki/Pantheons.md");
        assert_eq!(pantheons.parent_source_path, None);
        assert_eq!(pantheons.content, "# Pantheons\n");
        let greek = plan
            .documents
            .iter()
            .find(|document| document.source_path == "Pantheons/Greek/Greek.md")
            .expect("generated nested folder note");
        assert_eq!(greek.archive_path, "Wiki/Pantheons/Greek.md");
        assert_eq!(
            greek.parent_source_path.as_deref(),
            Some("Pantheons/Pantheons.md")
        );
        for child in ["Pantheons/Greek/Hera.md", "Pantheons/Greek/Zeus.md"] {
            let document = plan
                .documents
                .iter()
                .find(|document| document.source_path == child)
                .expect("selected child");
            assert_eq!(
                document.parent_source_path.as_deref(),
                Some("Pantheons/Greek/Greek.md")
            );
        }
    }

    #[test]
    fn plans_readme_and_outside_folder_note_conventions() {
        for (placement, name, folder_note) in [
            ("inside", "README", "Guides/README.md"),
            ("inside", "readme", "Guides/readme.md"),
            ("outside", "{{folder_name}}", "Guides.md"),
        ] {
            let (_temp, paths) = outline_vault(&[
                (folder_note, b"# Guides\n"),
                ("Guides/Start.md", b"# Start\n"),
            ]);
            configure_folder_notes(&paths, placement, name);

            let plan = plan_all(&paths);

            assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
            let parent = plan
                .documents
                .iter()
                .find(|document| document.source_path == folder_note)
                .expect("configured folder note should be the parent");
            assert_eq!(parent.archive_path, "Wiki/Guides.md");
            let child = plan
                .documents
                .iter()
                .find(|document| document.source_path == "Guides/Start.md")
                .expect("child should be present");
            assert_eq!(child.parent_source_path.as_deref(), Some(folder_note));
        }
    }

    #[test]
    fn plans_nested_folder_notes_links_embeds_and_deterministic_attachments() {
        let (_temp, paths) = outline_vault(&[
            (
                "Projects/Projects.md",
                b"# Projects\n\n[[Projects/Child]]\n",
            ),
            ("Projects/Child.md", b"# Child\n\n![[assets/logo.png]]\n"),
            ("Projects/Deep/Deep.md", b"# Deep\n"),
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
            Some("Projects/Deep/Deep.md")
        );
        assert!(leaf.content.contains("[../Child](../Child.md)"));
    }

    #[test]
    fn reports_case_insensitive_archive_collisions() {
        let documents = vec![
            DocumentPathPlan {
                source_path: "Projects/Readme.md".to_string(),
                source_document_id: "one".to_string(),
                title: "Readme".to_string(),
                archive_path: "Wiki/Projects/Readme.md".to_string(),
                parent_source_path: None,
                content: String::new(),
            },
            DocumentPathPlan {
                source_path: "Projects/README.md".to_string(),
                source_document_id: "two".to_string(),
                title: "README".to_string(),
                archive_path: "Wiki/Projects/README.md".to_string(),
                parent_source_path: None,
                content: String::new(),
            },
        ];
        let mut diagnostics = Vec::new();

        validate_archive_collisions(&documents, &[], &mut diagnostics);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, OutlineDiagnosticKind::Collision);
    }

    #[test]
    fn excluded_target_policy_preserves_strict_mode_and_can_render_labels_as_plain_text() {
        let (_temp, paths) = outline_vault(&[
            ("Projects/index.md", b"# Index\n"),
            ("Projects/Projects.md", b"# Projects\n"),
            (
                "Projects/Readme.md",
                b"Before [[Secret|hidden label]] and [details](../Secret.md#Details).\n",
            ),
            ("Secret.md", b"# Secret\n\n## Details\n"),
        ]);
        let source = fs::read(paths.vault_root().join("Projects/Readme.md")).expect("source");
        let report = execute_export_query(
            &paths,
            Some(r#"from notes where file.path matches "^Projects/""#),
            None,
            None,
        )
        .expect("query projects");
        let prepared = prepare_outline_export_data(
            &paths,
            &report,
            None,
            None,
            OutlineMarkdownOptions::default(),
        )
        .expect("prepare projects");
        let strict = plan_outline_publication(&paths, "Wiki", &prepared.notes, &prepared.links)
            .expect("plan projects strictly");
        assert!(!strict.is_valid());
        let excluded = strict
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == OutlineDiagnosticKind::ExcludedTarget)
            .collect::<Vec<_>>();
        assert_eq!(excluded.len(), 2);
        assert!(excluded.iter().all(|diagnostic| {
            diagnostic.excluded_target_policy == Some(OutlineExcludedTargetPolicyConfig::Error)
                && diagnostic.action == Some(OutlineDiagnosticAction::RerunWithPlainText)
                && diagnostic.target.as_deref() == Some("Secret.md")
                && diagnostic.line.is_some()
                && diagnostic.column.is_some()
                && diagnostic.byte_offset.is_some()
        }));

        let downgraded = plan_outline_publication_with_options(
            &paths,
            "Wiki",
            &prepared.notes,
            &prepared.links,
            OutlinePublicationOptions {
                block_reference_policy: OutlineBlockReferencePolicyConfig::Error,
                excluded_target_policy: OutlineExcludedTargetPolicyConfig::PlainText,
            },
        )
        .expect("plan projects with excluded links downgraded");
        assert!(downgraded.is_valid(), "{:?}", downgraded.diagnostics);
        let readme = downgraded
            .documents
            .iter()
            .find(|document| document.source_path == "Projects/Readme.md")
            .expect("readme document");
        assert!(readme.content.contains("Before hidden label and details."));
        assert!(!readme.content.contains("Secret"));
        assert_eq!(
            fs::read(paths.vault_root().join("Projects/Readme.md")).unwrap(),
            source
        );
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

    #[test]
    fn zip_export_writes_generated_folder_note() {
        let (_temp, paths) = outline_vault(&[("Pantheons/Zeus.md", b"# Zeus\n")]);
        let output = paths.vault_root().join("wiki.zip");

        let report = write_outline_zip(&paths, &output, plan_all(&paths), false)
            .expect("write export with placeholder");

        assert!(report.wrote_archive);
        assert_eq!(report.plan.diagnostics.len(), 1);
        let file = fs::File::open(&output).expect("open archive");
        let mut archive = ZipArchive::new(file).expect("read archive");
        let mut placeholder = String::new();
        archive
            .by_name("Wiki/Pantheons.md")
            .expect("placeholder entry")
            .read_to_string(&mut placeholder)
            .expect("read placeholder");
        assert_eq!(placeholder, "# Pantheons\n");
    }
}
