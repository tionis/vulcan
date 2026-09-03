use crate::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use vulcan_core::move_rewrite::rewrite_link_destination;
use vulcan_core::parser::{parse_document, LinkKind, RawLink};
use vulcan_core::paths::{secure_create, secure_read_to_string, secure_write};
use vulcan_core::{
    load_vault_config, plan_document_decomposition, resolve_note_reference,
    DecompositionDiagnostic, DecompositionOptions, DecompositionPlan, LinkChange, ScanMode,
    SourceByteSpan, VaultPaths,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingFragmentPolicy {
    Error,
    Preserve,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitNoteRequest {
    pub source: String,
    pub destination: Option<String>,
    pub from_level: u8,
    pub through_level: u8,
    pub keep_source: bool,
    pub missing_fragment_policy: MissingFragmentPolicy,
    pub navigation: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SplitNoteOutput {
    pub path: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_path: Option<String>,
    pub source_spans: Vec<SourceByteSpan>,
    pub children: Vec<String>,
    pub rewritten_links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SplitNoteRewrittenFile {
    pub path: String,
    pub changes: Vec<LinkChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SplitNoteReport {
    pub dry_run: bool,
    pub source_path: String,
    pub destination_root: String,
    pub root_path: String,
    pub source_retained: bool,
    pub notes: Vec<SplitNoteOutput>,
    pub rewritten_files: Vec<SplitNoteRewrittenFile>,
    pub diagnostics: Vec<DecompositionDiagnostic>,
    #[serde(skip_serializing)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct CachedLink {
    source_path: String,
    raw_text: String,
    byte_offset: usize,
    target_path_candidate: Option<String>,
    target_heading: Option<String>,
    target_block: Option<String>,
    resolved_target_id: Option<String>,
    resolved_target_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextEdit {
    start: usize,
    end: usize,
    replacement: String,
}

#[derive(Debug, Clone)]
struct ExternalRewritePlan {
    path: String,
    original: String,
    updated: String,
    changes: Vec<LinkChange>,
}

#[derive(Debug, Clone)]
struct RewriteTarget {
    path: Option<String>,
    heading: Option<String>,
    block: Option<String>,
}

pub fn split_note(
    paths: &VaultPaths,
    request: &SplitNoteRequest,
) -> Result<SplitNoteReport, AppError> {
    let _lock = vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    split_note_unlocked(paths, request)
}

#[allow(clippy::too_many_lines)]
fn split_note_unlocked(
    paths: &VaultPaths,
    request: &SplitNoteRequest,
) -> Result<SplitNoteReport, AppError> {
    let resolved = resolve_note_reference(paths, &request.source).map_err(AppError::operation)?;
    let source_path = resolved.path;
    let source = secure_read_to_string(paths.vault_root(), Path::new(&source_path))
        .map_err(AppError::operation)?;
    let connection = open_existing_cache(paths)?;
    ensure_cached_document_is_current(&connection, paths, &source_path, &source)?;
    let destination_root = request
        .destination
        .clone()
        .unwrap_or_else(|| default_destination_root(&source_path));
    let config = load_vault_config(paths).config;
    let mut plan = plan_document_decomposition(
        &source_path,
        &source,
        &config,
        &DecompositionOptions {
            from_level: request.from_level,
            through_level: request.through_level,
            destination_root: destination_root.clone(),
            navigation: request.navigation,
        },
    )
    .map_err(AppError::operation)?;

    if request.keep_source && plan.root_path == source_path {
        return Err(AppError::operation(format!(
            "--keep-source requires a destination whose folder note is not the source path `{source_path}`"
        )));
    }
    validate_output_collisions(paths, &connection, &plan, &source_path, request.keep_source)?;

    let links = load_related_links(&connection, &resolved.id)?;
    ensure_related_sources_are_current(&connection, paths, &source_path, &links)?;
    if request.missing_fragment_policy == MissingFragmentPolicy::Preserve {
        plan.diagnostics.extend(missing_fragment_diagnostics(
            &links,
            &plan,
            &resolved.id,
            &source_path,
            request.keep_source,
        ));
        plan.diagnostics.sort_by(|left, right| {
            left.code
                .cmp(&right.code)
                .then_with(|| left.heading.cmp(&right.heading))
                .then_with(|| left.fragment.cmp(&right.fragment))
        });
    }
    let parsed_source = parse_document(&source, &config);
    let mut document_paths = load_document_paths(&connection)?;
    if !request.keep_source {
        document_paths.retain(|path| path != &source_path);
    }
    document_paths.extend(plan.notes.iter().map(|note| note.path.clone()));
    document_paths.sort();
    document_paths.dedup();

    let cached_source_links = links
        .iter()
        .filter(|link| link.source_path == source_path)
        .map(|link| (link.byte_offset, link))
        .collect::<BTreeMap<_, _>>();
    let mut output_changes = BTreeMap::<String, Vec<LinkChange>>::new();
    let mut rendered_outputs = Vec::with_capacity(plan.notes.len());
    for note in &plan.notes {
        let mut edits = Vec::new();
        for placement in &note.link_placements {
            let cached = cached_source_links
                .get(&placement.source_byte_offset)
                .ok_or_else(|| stale_link_error(&source_path, placement.source_byte_offset))?;
            if cached.raw_text != placement.raw_text {
                return Err(stale_link_error(&source_path, placement.source_byte_offset));
            }
            let raw_link = parsed_source
                .links
                .iter()
                .find(|link| {
                    link.byte_offset == placement.source_byte_offset
                        && link.raw_text == placement.raw_text
                })
                .ok_or_else(|| stale_link_error(&source_path, placement.source_byte_offset))?;
            let Some(target) = rewrite_target(
                cached,
                &plan,
                &resolved.id,
                &source_path,
                request.keep_source,
                request.missing_fragment_policy,
            )?
            else {
                continue;
            };
            let local_path = if target.path.as_deref() == Some(note.path.as_str())
                && (target.heading.is_some() || target.block.is_some())
            {
                None
            } else {
                target.path.as_deref()
            };
            let replacement = rewrite_link_destination(
                raw_link,
                &note.path,
                local_path,
                target.heading.as_deref(),
                target.block.as_deref(),
                &document_paths,
                link_resolution_for_materialized_source(raw_link, config.link_resolution),
                config.link_style,
            );
            if replacement == placement.raw_text {
                continue;
            }
            edits.push(TextEdit {
                start: placement.output_byte_offset,
                end: placement.output_byte_offset + placement.raw_text.len(),
                replacement: replacement.clone(),
            });
            output_changes
                .entry(note.path.clone())
                .or_default()
                .push(LinkChange {
                    before: placement.raw_text.clone(),
                    after: replacement,
                });
        }
        rendered_outputs.push(apply_edits(&note.content, &edits, &note.path)?);
    }
    for (note, rendered) in plan.notes.iter_mut().zip(rendered_outputs) {
        note.content = rendered;
    }

    let external_rewrites = plan_external_inbound_rewrites(
        paths,
        &links,
        &source_path,
        &resolved.id,
        &plan,
        request.keep_source,
        request.missing_fragment_policy,
        &document_paths,
        &config,
    )?;
    let mut rewritten_files = output_changes
        .iter()
        .map(|(path, changes)| SplitNoteRewrittenFile {
            path: path.clone(),
            changes: changes.clone(),
        })
        .chain(
            external_rewrites
                .iter()
                .filter(|rewrite| !rewrite.changes.is_empty())
                .map(|rewrite| SplitNoteRewrittenFile {
                    path: rewrite.path.clone(),
                    changes: rewrite.changes.clone(),
                }),
        )
        .collect::<Vec<_>>();
    rewritten_files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut changed_paths = plan
        .notes
        .iter()
        .map(|note| note.path.clone())
        .chain(external_rewrites.iter().map(|rewrite| rewrite.path.clone()))
        .collect::<BTreeSet<_>>();
    if !request.keep_source {
        changed_paths.insert(source_path.clone());
    }

    if !request.dry_run {
        apply_plan_and_refresh_with_rollback(
            paths,
            &source_path,
            &source,
            request.keep_source,
            &plan,
            &external_rewrites,
        )?;
    }

    let notes = plan
        .notes
        .iter()
        .map(|note| SplitNoteOutput {
            path: note.path.clone(),
            title: note.title.clone(),
            parent_path: note.parent_path.clone(),
            source_spans: note.source_spans.clone(),
            children: note.children.clone(),
            rewritten_links: output_changes.get(&note.path).map_or(0, Vec::len),
        })
        .collect();
    Ok(SplitNoteReport {
        dry_run: request.dry_run,
        source_path,
        destination_root,
        root_path: plan.root_path,
        source_retained: request.keep_source,
        notes,
        rewritten_files,
        diagnostics: plan.diagnostics,
        changed_paths: changed_paths.into_iter().collect(),
    })
}

fn default_destination_root(source_path: &str) -> String {
    let path = Path::new(source_path);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Document");
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(
            || stem.to_string(),
            |parent| parent.join(stem).to_string_lossy().replace('\\', "/"),
        )
}

fn open_existing_cache(paths: &VaultPaths) -> Result<Connection, AppError> {
    if !paths.cache_db().exists() {
        return Err(AppError::operation(
            "cache is missing; run `vulcan scan` before splitting a note",
        ));
    }
    Connection::open(paths.cache_db()).map_err(AppError::operation)
}

fn ensure_cached_document_is_current(
    connection: &Connection,
    paths: &VaultPaths,
    path: &str,
    content: &str,
) -> Result<(), AppError> {
    let cached_hash = connection
        .query_row(
            "SELECT content_hash FROM documents WHERE path = ?1",
            params![path],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation(format!("source note `{path}` is not indexed")))?;
    let disk_hash = blake3::hash(content.as_bytes()).as_bytes().to_vec();
    if cached_hash != disk_hash {
        return Err(AppError::operation(format!(
            "cached metadata for `{path}` is stale; run `vulcan scan` and retry"
        )));
    }
    let metadata =
        fs::symlink_metadata(paths.vault_root().join(path)).map_err(AppError::operation)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::operation(format!(
            "source note `{path}` must be a regular non-symlink file"
        )));
    }
    Ok(())
}

fn validate_output_collisions(
    paths: &VaultPaths,
    connection: &Connection,
    plan: &DecompositionPlan,
    source_path: &str,
    keep_source: bool,
) -> Result<(), AppError> {
    let existing_paths = load_document_paths(connection)?
        .into_iter()
        .map(|path| (path.to_lowercase(), path))
        .collect::<BTreeMap<_, _>>();
    let mut planned = BTreeMap::<String, String>::new();
    for note in &plan.notes {
        let key = note.path.to_lowercase();
        if let Some(other) = planned.insert(key.clone(), note.path.clone()) {
            return Err(AppError::operation(format!(
                "generated path collision: `{other}` and `{}` differ only by case",
                note.path
            )));
        }
        let replaces_source = !keep_source && note.path == source_path;
        if let Some(existing) = existing_paths.get(&key) {
            if !replaces_source || existing != source_path {
                return Err(AppError::operation(format!(
                    "generated note `{}` collides with existing vault document `{existing}`",
                    note.path
                )));
            }
        }
        let absolute = paths.vault_root().join(&note.path);
        if absolute.exists() && !replaces_source {
            return Err(AppError::operation(format!(
                "generated note destination already exists: {}",
                note.path
            )));
        }
    }
    Ok(())
}

fn load_related_links(
    connection: &Connection,
    source_document_id: &str,
) -> Result<Vec<CachedLink>, AppError> {
    let mut statement = connection
        .prepare(
            "
            SELECT source.path,
                   links.raw_text,
                   links.byte_offset,
                   links.target_path_candidate,
                   links.target_heading,
                   links.target_block,
                   links.resolved_target_id,
                   target.path
            FROM links
            JOIN documents AS source ON source.id = links.source_document_id
            LEFT JOIN documents AS target ON target.id = links.resolved_target_id
            WHERE links.source_document_id = ?1 OR links.resolved_target_id = ?1
            ORDER BY source.path, links.byte_offset
            ",
        )
        .map_err(AppError::operation)?;
    let rows = statement
        .query_map(params![source_document_id], |row| {
            let byte_offset = row.get::<_, i64>(2)?;
            Ok(CachedLink {
                source_path: row.get(0)?,
                raw_text: row.get(1)?,
                byte_offset: usize::try_from(byte_offset)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, byte_offset))?,
                target_path_candidate: row.get(3)?,
                target_heading: row.get(4)?,
                target_block: row.get(5)?,
                resolved_target_id: row.get(6)?,
                resolved_target_path: row.get(7)?,
            })
        })
        .map_err(AppError::operation)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::operation)
}

fn load_document_paths(connection: &Connection) -> Result<Vec<String>, AppError> {
    let mut statement = connection
        .prepare("SELECT path FROM documents ORDER BY path")
        .map_err(AppError::operation)?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .map_err(AppError::operation)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::operation)
}

fn ensure_related_sources_are_current(
    connection: &Connection,
    paths: &VaultPaths,
    source_path: &str,
    links: &[CachedLink],
) -> Result<(), AppError> {
    let related = links
        .iter()
        .filter(|link| link.source_path != source_path)
        .map(|link| link.source_path.as_str())
        .collect::<BTreeSet<_>>();
    for path in related {
        let content = secure_read_to_string(paths.vault_root(), Path::new(path))
            .map_err(AppError::operation)?;
        ensure_cached_document_is_current(connection, paths, path, &content)?;
    }
    Ok(())
}

fn rewrite_target(
    link: &CachedLink,
    plan: &DecompositionPlan,
    source_document_id: &str,
    source_path: &str,
    keep_source: bool,
    missing_fragment_policy: MissingFragmentPolicy,
) -> Result<Option<RewriteTarget>, AppError> {
    if link_targets_source(link, source_document_id) {
        if let Some(fragment) = link.target_heading.as_deref() {
            if plan.fragment_target_count(fragment) > 1 {
                return Err(AppError::operation(format!(
                    "link `{}` targets duplicate heading or HTML anchor `{fragment}` in `{source_path}`; disambiguate the target before splitting",
                    link.raw_text
                )));
            }
            let Some(target) = plan.fragment_target(fragment) else {
                if missing_fragment_policy == MissingFragmentPolicy::Preserve {
                    return Ok(Some(RewriteTarget {
                        path: Some(if keep_source {
                            source_path.to_string()
                        } else {
                            plan.root_path.clone()
                        }),
                        heading: Some(fragment.to_string()),
                        block: None,
                    }));
                }
                return Err(AppError::operation(format!(
                    "link `{}` targets missing heading or HTML anchor `{fragment}` in `{source_path}`",
                    link.raw_text
                )));
            };
            return Ok(Some(RewriteTarget {
                path: Some(target.path.clone()),
                heading: target.fragment.clone(),
                block: None,
            }));
        }
        if let Some(block) = link.target_block.as_deref() {
            let target = plan.block_target(block).ok_or_else(|| {
                AppError::operation(format!(
                    "link `{}` targets missing block `^{block}` in `{source_path}`",
                    link.raw_text
                ))
            })?;
            return Ok(Some(RewriteTarget {
                path: Some(target.path.clone()),
                heading: None,
                block: target.fragment.clone(),
            }));
        }
        return Ok(Some(RewriteTarget {
            path: Some(if keep_source {
                source_path.to_string()
            } else {
                plan.root_path.clone()
            }),
            heading: None,
            block: None,
        }));
    }

    if let Some(target_path) = link.resolved_target_path.as_deref() {
        return Ok(Some(RewriteTarget {
            path: Some(target_path.to_string()),
            heading: link.target_heading.clone(),
            block: link.target_block.clone(),
        }));
    }
    if link.target_path_candidate.is_some() {
        return Err(AppError::operation(format!(
            "cannot safely split `{source_path}` while link `{}` at byte {} is unresolved; repair the link and rescan first",
            link.raw_text, link.byte_offset
        )));
    }
    Ok(None)
}

fn link_targets_source(link: &CachedLink, source_document_id: &str) -> bool {
    link.resolved_target_id.as_deref() == Some(source_document_id)
        || (link.resolved_target_id.is_none()
            && link.target_path_candidate.is_none()
            && (link.target_heading.is_some() || link.target_block.is_some()))
}

fn missing_fragment_diagnostics(
    links: &[CachedLink],
    plan: &DecompositionPlan,
    source_document_id: &str,
    source_path: &str,
    keep_source: bool,
) -> Vec<DecompositionDiagnostic> {
    let mut missing = BTreeMap::<String, (&str, usize, &str)>::new();
    for link in links {
        if !link_targets_source(link, source_document_id) {
            continue;
        }
        let Some(fragment) = link.target_heading.as_deref() else {
            continue;
        };
        if plan.fragment_target_count(fragment) == 0 {
            missing.entry(fragment.to_string()).or_insert((
                &link.source_path,
                link.byte_offset,
                &link.raw_text,
            ));
        }
    }
    missing
        .into_iter()
        .map(|(fragment, (link_source, byte_offset, raw_text))| DecompositionDiagnostic {
            code: "preserved_missing_fragment".to_string(),
            message: format!(
                "fragment `{fragment}` has no matching heading or HTML anchor; preserved link `{raw_text}` from `{link_source}` byte {byte_offset} against {} `{source_path}`",
                if keep_source { "retained source" } else { "generated root" }
            ),
            heading: None,
            fragment: Some(fragment),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn plan_external_inbound_rewrites(
    paths: &VaultPaths,
    links: &[CachedLink],
    source_path: &str,
    source_document_id: &str,
    plan: &DecompositionPlan,
    keep_source: bool,
    missing_fragment_policy: MissingFragmentPolicy,
    document_paths: &[String],
    config: &vulcan_core::VaultConfig,
) -> Result<Vec<ExternalRewritePlan>, AppError> {
    let mut by_file = BTreeMap::<String, Vec<&CachedLink>>::new();
    for link in links.iter().filter(|link| {
        link.source_path != source_path
            && link.resolved_target_id.as_deref() == Some(source_document_id)
    }) {
        by_file
            .entry(link.source_path.clone())
            .or_default()
            .push(link);
    }
    let mut rewrites = Vec::new();
    for (path, cached_links) in by_file {
        let original = secure_read_to_string(paths.vault_root(), Path::new(&path))
            .map_err(AppError::operation)?;
        let parsed = parse_document(&original, config);
        let mut edits = Vec::new();
        let mut changes = Vec::new();
        for cached in cached_links {
            let raw_link = parsed
                .links
                .iter()
                .find(|link| {
                    link.byte_offset == cached.byte_offset && link.raw_text == cached.raw_text
                })
                .ok_or_else(|| stale_link_error(&path, cached.byte_offset))?;
            let target = rewrite_target(
                cached,
                plan,
                source_document_id,
                source_path,
                keep_source,
                missing_fragment_policy,
            )?
            .expect("resolved inbound source link has a rewrite target");
            let replacement = rewrite_link_destination(
                raw_link,
                &path,
                target.path.as_deref(),
                target.heading.as_deref(),
                target.block.as_deref(),
                document_paths,
                link_resolution_for_materialized_source(raw_link, config.link_resolution),
                config.link_style,
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
        let updated = apply_edits(&original, &edits, &path)?;
        rewrites.push(ExternalRewritePlan {
            path,
            original,
            updated,
            changes,
        });
    }
    Ok(rewrites)
}

fn apply_edits(source: &str, edits: &[TextEdit], path: &str) -> Result<String, AppError> {
    let mut edits = edits.to_vec();
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.start));
    let mut previous_start = source.len();
    let mut updated = source.to_string();
    for edit in edits {
        if edit.start > edit.end || edit.end > previous_start || edit.end > updated.len() {
            return Err(AppError::operation(format!(
                "overlapping or invalid planned link edits in `{path}`"
            )));
        }
        if !updated.is_char_boundary(edit.start) || !updated.is_char_boundary(edit.end) {
            return Err(AppError::operation(format!(
                "planned link edit in `{path}` is not on UTF-8 boundaries"
            )));
        }
        updated.replace_range(edit.start..edit.end, &edit.replacement);
        previous_start = edit.start;
    }
    Ok(updated)
}

fn link_resolution_for_materialized_source(
    link: &RawLink,
    configured: vulcan_core::LinkResolutionMode,
) -> vulcan_core::LinkResolutionMode {
    if link.link_kind == LinkKind::Markdown
        || (link.link_kind == LinkKind::Embed && link.raw_text.starts_with("!["))
    {
        vulcan_core::LinkResolutionMode::Relative
    } else {
        configured
    }
}

fn apply_plan_and_refresh_with_rollback(
    paths: &VaultPaths,
    source_path: &str,
    source: &str,
    keep_source: bool,
    plan: &DecompositionPlan,
    external_rewrites: &[ExternalRewritePlan],
) -> Result<(), AppError> {
    let mut created_paths = Vec::<String>::new();
    let mut originals = external_rewrites
        .iter()
        .map(|rewrite| (rewrite.path.clone(), rewrite.original.clone()))
        .collect::<BTreeMap<_, _>>();
    originals.insert(source_path.to_string(), source.to_string());

    let apply_result = (|| -> Result<(), std::io::Error> {
        for note in &plan.notes {
            if note.path == source_path {
                continue;
            }
            secure_create(paths.vault_root(), Path::new(&note.path), &note.content)?;
            created_paths.push(note.path.clone());
        }
        for rewrite in external_rewrites {
            if rewrite.updated != rewrite.original {
                secure_write(
                    paths.vault_root(),
                    Path::new(&rewrite.path),
                    &rewrite.updated,
                )?;
            }
        }
        if let Some(root_note) = plan.notes.iter().find(|note| note.path == source_path) {
            secure_write(
                paths.vault_root(),
                Path::new(source_path),
                &root_note.content,
            )?;
        } else if !keep_source {
            fs::remove_file(paths.vault_root().join(source_path))?;
        }
        Ok(())
    })();

    if let Err(error) = apply_result {
        rollback_files(paths, &created_paths, &originals);
        return Err(AppError::operation(format!(
            "failed to apply split-note plan; restored original files: {error}"
        )));
    }
    if let Err(error) = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental) {
        rollback_files(paths, &created_paths, &originals);
        let _ = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental);
        return Err(AppError::operation(format!(
            "failed to refresh the cache after split-note; restored original files: {error}"
        )));
    }
    Ok(())
}

fn rollback_files(
    paths: &VaultPaths,
    created_paths: &[String],
    originals: &BTreeMap<String, String>,
) {
    for path in created_paths.iter().rev() {
        let _ = fs::remove_file(paths.vault_root().join(path));
    }
    for (path, content) in originals {
        let _ = secure_write(paths.vault_root(), Path::new(path), content);
    }
}

fn stale_link_error(path: &str, byte_offset: usize) -> AppError {
    AppError::operation(format!(
        "cached link span at byte {byte_offset} in `{path}` is stale; run `vulcan scan` and retry"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, scan_vault, ScanMode};

    fn setup_vault() -> (tempfile::TempDir, VaultPaths) {
        let temp = tempdir().expect("tempdir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("init");
        (temp, paths)
    }

    fn request() -> SplitNoteRequest {
        SplitNoteRequest {
            source: "Rulebook.md".to_string(),
            destination: None,
            from_level: 2,
            through_level: 3,
            keep_source: false,
            missing_fragment_policy: MissingFragmentPolicy::Error,
            navigation: true,
            dry_run: false,
        }
    }

    #[test]
    fn dry_run_plans_tree_and_leaves_vault_unchanged() {
        let (_temp, paths) = setup_vault();
        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# Rules\n\n## Combat\nText.\n\n### Damage\nHarm.\n",
        )
        .expect("source");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let dry_request = SplitNoteRequest {
            dry_run: true,
            ..request()
        };
        let report = split_note(&paths, &dry_request).expect("dry run");
        let repeated = split_note(&paths, &dry_request).expect("repeated dry run");

        assert!(report.dry_run);
        assert_eq!(report, repeated);
        assert_eq!(report.notes.len(), 3);
        assert!(paths.vault_root().join("Rulebook.md").exists());
        assert!(!paths.vault_root().join("Rulebook/Rulebook.md").exists());
    }

    #[test]
    fn applies_tree_and_rewrites_inbound_cross_section_and_asset_links() {
        let (_temp, paths) = setup_vault();
        fs::create_dir(paths.vault_root().join("assets")).expect("assets");
        fs::write(paths.vault_root().join("assets/map.png"), b"png").expect("asset");
        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# Rules\n\n## Combat\nSee [map](assets/map.png) and [[#Damage]].\n\n### Damage\nHarm.\n\n^harm\n",
        )
        .expect("source");
        fs::write(
            paths.vault_root().join("Index.md"),
            "See [[Rulebook#Combat]], [[Rulebook#Damage|damage]], and [[Rulebook#^harm|harm]].\n",
        )
        .expect("index");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        let report = split_note(&paths, &request()).expect("split");

        assert!(!paths.vault_root().join("Rulebook.md").exists());
        assert!(paths.vault_root().join("assets/map.png").exists());
        let combat = fs::read_to_string(paths.vault_root().join("Rulebook/Combat/Combat.md"))
            .expect("combat");
        assert!(
            combat.contains("[map](../../assets/map.png)"),
            "unexpected combat content: {combat}"
        );
        assert!(combat.contains("[[Damage]]"));
        let index = fs::read_to_string(paths.vault_root().join("Index.md")).expect("index");
        assert!(index.contains("[[Combat]]"), "unexpected index: {index}");
        assert!(
            index.contains("[[Damage|damage]]"),
            "unexpected index: {index}"
        );
        assert!(
            index.contains("[[Damage#^harm|harm]]"),
            "unexpected index: {index}"
        );
        assert!(report.changed_paths.contains(&"Index.md".to_string()));
        assert_eq!(report.rewritten_files.len(), 2);
        resolve_note_reference(&paths, "Rulebook/Combat/Damage").expect("reindexed output");
    }

    #[test]
    fn rewrites_pdf_converter_html_anchor_links_to_the_owning_note() {
        let (_temp, paths) = setup_vault();
        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# <span id=\"page-1-0\"></span>Rules\nSee [combat](#page-2-0).\n\n## <span id=\"page-2-0\"></span>Combat\nFight.\n",
        )
        .expect("source");
        fs::write(
            paths.vault_root().join("Index.md"),
            "See [[Rulebook#page-2-0|combat page]].\n",
        )
        .expect("index");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        split_note(&paths, &request()).expect("split");

        let root =
            fs::read_to_string(paths.vault_root().join("Rulebook/Rulebook.md")).expect("root");
        assert!(
            root.contains("[combat](Combat.md#page-2-0)"),
            "unexpected root: {root}"
        );
        let combat =
            fs::read_to_string(paths.vault_root().join("Rulebook/Combat.md")).expect("combat");
        assert!(combat.starts_with("# <span id=\"page-2-0\"></span>Combat"));
        let index = fs::read_to_string(paths.vault_root().join("Index.md")).expect("index");
        assert!(
            index.contains("[[Combat#page-2-0|combat page]]"),
            "unexpected index: {index}"
        );
    }

    #[test]
    fn missing_fragments_fail_closed_unless_preservation_is_explicit() {
        let (_temp, paths) = setup_vault();
        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# Rules\nSee [[#page-404-0]].\n\n## Combat\nFight.\n",
        )
        .expect("source");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        let error = split_note(&paths, &request()).expect_err("missing fragment");
        assert!(error.to_string().contains("missing heading or HTML anchor"));

        let preserve = SplitNoteRequest {
            missing_fragment_policy: MissingFragmentPolicy::Preserve,
            ..request()
        };
        let report = split_note(&paths, &preserve).expect("preserved split");
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].code, "preserved_missing_fragment");
        assert_eq!(
            report.diagnostics[0].fragment.as_deref(),
            Some("page-404-0")
        );
        let root =
            fs::read_to_string(paths.vault_root().join("Rulebook/Rulebook.md")).expect("root");
        assert!(root.contains("[[#page-404-0]]"), "unexpected root: {root}");
    }

    #[test]
    fn rejects_stale_cache_collisions_and_ambiguous_heading_links() {
        let (_temp, paths) = setup_vault();
        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# Rules\n## Topic\nOne\n## Topic\nTwo\n",
        )
        .expect("source");
        fs::write(paths.vault_root().join("Index.md"), "[[Rulebook#Topic]]\n").expect("index");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let error = split_note(&paths, &request()).expect_err("ambiguous link");
        assert!(error.to_string().contains("duplicate heading"));

        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# Rules changed\n## Topic\nOne\n",
        )
        .expect("change without scan");
        let error = split_note(&paths, &request()).expect_err("stale cache");
        assert!(error.to_string().contains("stale"));
    }

    #[test]
    fn keep_source_preserves_original_and_materializes_a_separate_tree() {
        let (_temp, paths) = setup_vault();
        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# Rules\n## Combat\nText\n",
        )
        .expect("source");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let keep = SplitNoteRequest {
            keep_source: true,
            ..request()
        };
        let report = split_note(&paths, &keep).expect("keep source split");
        assert!(report.source_retained);
        assert!(paths.vault_root().join("Rulebook.md").exists());
        assert!(paths.vault_root().join("Rulebook/Rulebook.md").exists());
    }

    #[test]
    fn rejects_existing_generated_destinations() {
        let (_temp, paths) = setup_vault();
        fs::create_dir_all(paths.vault_root().join("Rulebook")).expect("destination dir");
        fs::write(
            paths.vault_root().join("Rulebook.md"),
            "# Rules\n## Combat\nText\n",
        )
        .expect("source");
        fs::write(
            paths.vault_root().join("Rulebook/Combat.md"),
            "# Existing\n",
        )
        .expect("collision");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        let error = split_note(&paths, &request()).expect_err("collision");
        assert!(error.to_string().contains("collides"));
        assert!(paths.vault_root().join("Rulebook.md").exists());
    }

    #[test]
    fn apply_failure_removes_created_outputs_and_restores_source() {
        let (_temp, paths) = setup_vault();
        let source = "# Rules\n";
        fs::write(paths.vault_root().join("Rulebook.md"), source).expect("source");
        fs::write(paths.vault_root().join("blocked"), "not a directory").expect("blocker");
        let note = |path: &str| vulcan_core::DecompositionNotePlan {
            path: path.to_string(),
            title: path.to_string(),
            parent_path: None,
            source_spans: Vec::new(),
            children: Vec::new(),
            content: "# Generated\n".to_string(),
            link_placements: Vec::new(),
        };
        let plan = DecompositionPlan {
            source_path: "Rulebook.md".to_string(),
            destination_root: "Generated".to_string(),
            root_path: "Generated/Root.md".to_string(),
            notes: vec![note("Generated/Root.md"), note("blocked/Second.md")],
            diagnostics: Vec::new(),
            heading_targets: Vec::new(),
            anchor_targets: Vec::new(),
            block_targets: Vec::new(),
        };

        let error =
            apply_plan_and_refresh_with_rollback(&paths, "Rulebook.md", source, false, &plan, &[])
                .expect_err("apply should fail");

        assert!(error.to_string().contains("restored original files"));
        assert!(!paths.vault_root().join("Generated/Root.md").exists());
        assert_eq!(
            fs::read_to_string(paths.vault_root().join("Rulebook.md")).expect("source restored"),
            source
        );
    }
}
