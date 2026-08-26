use crate::templates::{
    parse_frontmatter_document, render_note_from_parts, YamlMapping, YamlValue,
};
use crate::AppError;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_core::artifact::{
    inspect_mdaf, MdafArtifact, MdafDiagnostic, MdafMemberRole, MdafOutline, MdafSelector,
    MdafSourceLocator as ArtifactSourceLocator, MdafSourceMap,
};
use vulcan_core::move_rewrite::rewrite_link_destination;
use vulcan_core::parser::{parse_document, RawLink};
use vulcan_core::paths::{
    normalize_relative_input_path, secure_create, secure_create_file, RelativePathOptions,
};
use vulcan_core::{
    load_vault_config, plan_document_decomposition,
    plan_document_decomposition_with_aligned_outline, AlignedOutlineHeading,
    DecompositionDiagnostic, DecompositionOptions, DecompositionPlan, LinkChange,
    LinkResolutionMode, ScanMode, SourceByteSpan, VaultPaths,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactHierarchyAuthority {
    Markdown,
    Outline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactImportRequest {
    pub artifact: PathBuf,
    pub destination: String,
    pub hierarchy: ArtifactHierarchyAuthority,
    pub from_level: u8,
    pub through_level: u8,
    pub navigation: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactImportNote {
    pub path: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_path: Option<String>,
    pub source_spans: Vec<SourceByteSpan>,
    pub children: Vec<String>,
    pub rewritten_links: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactImportAsset {
    pub member_path: String,
    pub vault_path: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactRewrittenFile {
    pub path: String,
    pub changes: Vec<LinkChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactImportReport {
    pub dry_run: bool,
    pub artifact_identity: String,
    pub destination_root: String,
    pub hierarchy: ArtifactHierarchyAuthority,
    pub root_path: String,
    pub notes: Vec<ArtifactImportNote>,
    pub assets: Vec<ArtifactImportAsset>,
    pub rewritten_files: Vec<ArtifactRewrittenFile>,
    pub diagnostics: Vec<ArtifactImportDiagnostic>,
    #[serde(skip_serializing)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactImportDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct NoteSourceProvenance {
    artifact: String,
    member: &'static str,
    spans: Vec<NoteSourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct NoteSourceSpan {
    start: usize,
    end: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    locators: Vec<NoteSourceLocator>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct NoteSourceLocator {
    source_id: String,
    selectors: Vec<MdafSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextEdit {
    start: usize,
    end: usize,
    replacement: String,
}

pub fn import_artifact(
    paths: &VaultPaths,
    request: &ArtifactImportRequest,
) -> Result<ArtifactImportReport, AppError> {
    let _lock = vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    import_artifact_unlocked(paths, request)
}

#[allow(clippy::too_many_lines)]
fn import_artifact_unlocked(
    paths: &VaultPaths,
    request: &ArtifactImportRequest,
) -> Result<ArtifactImportReport, AppError> {
    let destination = validate_destination(paths, &request.destination)?;
    let artifact = inspect_mdaf(&request.artifact).map_err(AppError::operation)?;
    if !artifact.valid {
        return Err(AppError::operation(format_invalid_artifact(
            &artifact.diagnostics,
        )));
    }
    let markdown = artifact
        .markdown
        .as_deref()
        .ok_or_else(|| AppError::operation("validated artifact has no primary Markdown"))?;
    let config = load_vault_config(paths).config;
    let options = DecompositionOptions {
        from_level: request.from_level,
        through_level: request.through_level,
        destination_root: destination.clone(),
        navigation: request.navigation,
    };
    let virtual_source = format!("{destination}/text.md");
    let mut plan = match request.hierarchy {
        ArtifactHierarchyAuthority::Markdown => {
            plan_document_decomposition(&virtual_source, markdown, &config, &options)
        }
        ArtifactHierarchyAuthority::Outline => {
            let outline = artifact
                .outline
                .as_ref()
                .ok_or_else(|| AppError::operation("--hierarchy outline requires outline.json"))?;
            let headings = aligned_outline_headings(outline)?;
            plan_document_decomposition_with_aligned_outline(
                &virtual_source,
                markdown,
                &config,
                &options,
                &headings,
            )
        }
    }
    .map_err(AppError::operation)?;
    ensure_plan_is_contained(&plan, &destination)?;

    let assets = artifact_assets(&artifact, &destination);
    let parsed = parse_document(markdown, &config);
    let mut rewrite_diagnostics = Vec::new();
    let mut rewritten_files = Vec::new();
    let routing_plan = plan.clone();
    for note in &mut plan.notes {
        let (content, changes) = rewrite_import_links(
            &note.content,
            &note.path,
            &note.link_placements,
            &parsed.links,
            &routing_plan,
            &assets,
            artifact.source_map.as_ref(),
            &config,
            &mut rewrite_diagnostics,
        )?;
        note.content = add_source_frontmatter(
            &content,
            &artifact.identity,
            &note.source_spans,
            artifact.source_map.as_ref(),
        )?;
        if !changes.is_empty() {
            rewritten_files.push(ArtifactRewrittenFile {
                path: note.path.clone(),
                changes,
            });
        }
    }
    rewritten_files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut diagnostics = plan
        .diagnostics
        .iter()
        .map(import_decomposition_diagnostic)
        .chain(rewrite_diagnostics)
        .collect::<Vec<_>>();
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    let mut changed_paths = plan
        .notes
        .iter()
        .map(|note| note.path.clone())
        .chain(assets.iter().map(|asset| asset.vault_path.clone()))
        .collect::<Vec<_>>();
    changed_paths.sort();

    if !request.dry_run {
        apply_import(paths, &artifact, &plan, &assets, &destination)?;
    }

    let notes = plan
        .notes
        .iter()
        .map(|note| ArtifactImportNote {
            path: note.path.clone(),
            title: note.title.clone(),
            parent_path: note.parent_path.clone(),
            source_spans: note.source_spans.clone(),
            children: note.children.clone(),
            rewritten_links: rewritten_files
                .iter()
                .find(|file| file.path == note.path)
                .map_or(0, |file| file.changes.len()),
        })
        .collect();
    Ok(ArtifactImportReport {
        dry_run: request.dry_run,
        artifact_identity: artifact.identity,
        destination_root: destination,
        hierarchy: request.hierarchy,
        root_path: plan.root_path,
        notes,
        assets,
        rewritten_files,
        diagnostics,
        changed_paths,
    })
}

fn validate_destination(paths: &VaultPaths, destination: &str) -> Result<String, AppError> {
    let normalized = normalize_relative_input_path(
        destination,
        RelativePathOptions {
            expected_extension: None,
            append_extension_if_missing: false,
        },
    )
    .map_err(AppError::operation)?;
    if normalized != destination || normalized == ".vulcan" || normalized.starts_with(".vulcan/") {
        return Err(AppError::operation(format!(
            "artifact destination must be a normalized, non-internal vault-relative folder; use `{normalized}`"
        )));
    }
    let components = Path::new(&normalized).components().collect::<Vec<_>>();
    let mut current = paths.vault_root().to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let name = component.as_os_str().to_string_lossy();
        let mut matched = None;
        if current.exists() {
            for entry in fs::read_dir(&current).map_err(AppError::operation)? {
                let entry = entry.map_err(AppError::operation)?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&name)
                {
                    matched = Some(entry);
                    break;
                }
            }
        }
        if let Some(entry) = matched {
            if index + 1 == components.len() {
                return Err(AppError::operation(format!(
                    "artifact destination collides with existing vault path: {}",
                    entry.path().display()
                )));
            }
            if entry.file_name().to_string_lossy() != name
                || entry.file_type().map_err(AppError::operation)?.is_symlink()
                || !entry.file_type().map_err(AppError::operation)?.is_dir()
            {
                return Err(AppError::operation(format!(
                    "artifact destination ancestor is not an exact regular directory: {}",
                    entry.path().display()
                )));
            }
            current = entry.path();
        } else {
            current.push(name.as_ref());
        }
    }
    Ok(normalized)
}

fn format_invalid_artifact(diagnostics: &[MdafDiagnostic]) -> String {
    let details = diagnostics
        .iter()
        .filter(|item| {
            matches!(
                item.severity,
                vulcan_core::artifact::MdafDiagnosticSeverity::Error
            )
        })
        .take(8)
        .map(|item| format!("{}: {}", item.code, item.message))
        .collect::<Vec<_>>()
        .join("; ");
    format!("artifact validation failed: {details}")
}

fn aligned_outline_headings(outline: &MdafOutline) -> Result<Vec<AlignedOutlineHeading>, AppError> {
    let mut stack = Vec::<(&str, u32)>::new();
    let mut headings = Vec::with_capacity(outline.nodes.len());
    for node in &outline.nodes {
        if node.heading.start != node.section.start {
            return Err(AppError::operation(format!(
                "outline node `{}` cannot be selected as hierarchy authority because its section starts before its heading",
                node.id
            )));
        }
        while stack.last().is_some_and(|(_, level)| *level >= node.level) {
            stack.pop();
        }
        if node.parent.as_deref() != stack.last().map(|(id, _)| *id) {
            return Err(AppError::operation(format!(
                "outline node `{}` has a parent relation that cannot be represented as a Markdown hierarchy",
                node.id
            )));
        }
        let level = u8::try_from(node.level).map_err(AppError::operation)?;
        headings.push(AlignedOutlineHeading {
            level,
            title: node.title.clone(),
            byte_offset: node.section.start,
        });
        stack.push((&node.id, node.level));
    }
    Ok(headings)
}

fn ensure_plan_is_contained(plan: &DecompositionPlan, destination: &str) -> Result<(), AppError> {
    let prefix = format!("{destination}/");
    if plan
        .notes
        .iter()
        .any(|note| !note.path.starts_with(&prefix))
    {
        return Err(AppError::operation(
            "decomposition plan escaped the artifact destination",
        ));
    }
    Ok(())
}

fn artifact_assets(artifact: &MdafArtifact, destination: &str) -> Vec<ArtifactImportAsset> {
    let mut assets = artifact
        .manifest
        .as_ref()
        .into_iter()
        .flat_map(|manifest| &manifest.members)
        .filter(|member| member.role == MdafMemberRole::Asset)
        .map(|member| ArtifactImportAsset {
            member_path: member.path.clone(),
            vault_path: format!("{destination}/{}", member.path),
            size: member.size,
            digest: member.digest.clone(),
        })
        .collect::<Vec<_>>();
    assets.sort_by(|left, right| left.member_path.cmp(&right.member_path));
    assets
}

#[allow(clippy::too_many_arguments)]
fn rewrite_import_links(
    content: &str,
    note_path: &str,
    placements: &[vulcan_core::DecompositionLinkPlacement],
    source_links: &[RawLink],
    plan: &DecompositionPlan,
    assets: &[ArtifactImportAsset],
    source_map: Option<&MdafSourceMap>,
    config: &vulcan_core::VaultConfig,
    diagnostics: &mut Vec<ArtifactImportDiagnostic>,
) -> Result<(String, Vec<LinkChange>), AppError> {
    let mut edits = Vec::new();
    let mut changes = Vec::new();
    let available_paths = plan
        .notes
        .iter()
        .map(|note| note.path.clone())
        .chain(assets.iter().map(|asset| asset.vault_path.clone()))
        .collect::<Vec<_>>();
    for placement in placements {
        let raw = source_links
            .iter()
            .find(|link| {
                link.byte_offset == placement.source_byte_offset
                    && link.raw_text == placement.raw_text
            })
            .ok_or_else(|| {
                AppError::operation(format!(
                    "artifact link at byte {} changed during planning",
                    placement.source_byte_offset
                ))
            })?;
        let target = import_link_target(raw, plan, assets, source_map, diagnostics);
        let Some((path, heading, block)) = target else {
            continue;
        };
        let local_path = if path == note_path && (heading.is_some() || block.is_some()) {
            None
        } else {
            Some(path.as_str())
        };
        let replacement = rewrite_link_destination(
            raw,
            note_path,
            local_path,
            heading.as_deref(),
            block.as_deref(),
            &available_paths,
            LinkResolutionMode::Relative,
            config.link_style,
        );
        if replacement != placement.raw_text {
            edits.push(TextEdit {
                start: placement.output_byte_offset,
                end: placement.output_byte_offset + placement.raw_text.len(),
                replacement: replacement.clone(),
            });
            changes.push(LinkChange {
                before: placement.raw_text.clone(),
                after: replacement,
            });
        }
    }
    Ok((apply_edits(content, &edits, note_path)?, changes))
}

fn import_link_target(
    raw: &RawLink,
    plan: &DecompositionPlan,
    assets: &[ArtifactImportAsset],
    source_map: Option<&MdafSourceMap>,
    diagnostics: &mut Vec<ArtifactImportDiagnostic>,
) -> Option<(String, Option<String>, Option<String>)> {
    if raw.target_path_candidate.is_none() {
        if let Some(heading) = raw.target_heading.as_deref() {
            if plan.fragment_target_count(heading) == 1 {
                let target = plan.fragment_target(heading)?;
                return Some((target.path.clone(), target.fragment.clone(), None));
            }
        }
        if let Some(block) = raw.target_block.as_deref() {
            let target = plan.block_target(block)?;
            return Some((target.path.clone(), None, target.fragment.clone()));
        }
    }
    if let Some(candidate) = raw.target_path_candidate.as_deref() {
        if let Some(asset) = assets.iter().find(|asset| asset.member_path == candidate) {
            return Some((asset.vault_path.clone(), None, None));
        }
    }
    let source_map = source_map?;
    let raw_end = raw.byte_offset + raw.raw_text.len();
    let references = source_map
        .references
        .iter()
        .filter(|reference| {
            reference.document.start < raw_end && raw.byte_offset < reference.document.end
        })
        .collect::<Vec<_>>();
    if references.len() != 1 {
        return None;
    }
    let reference = references[0];
    let targets = source_map
        .mappings
        .iter()
        .filter(|mapping| source_locators_overlap(&mapping.source, &reference.target))
        .filter_map(|mapping| plan.note_for_source_offset(mapping.document.start))
        .map(|note| note.path.clone())
        .collect::<BTreeSet<_>>();
    if targets.len() == 1 {
        return Some((targets.into_iter().next()?, None, None));
    }
    diagnostics.push(ArtifactImportDiagnostic {
        code: "source_reference_unresolved".to_string(),
        message: format!(
            "source reference at Markdown byte {} resolved to {} candidate notes and was preserved",
            raw.byte_offset,
            targets.len()
        ),
        path: None,
    });
    None
}

fn source_locators_overlap(left: &ArtifactSourceLocator, right: &ArtifactSourceLocator) -> bool {
    left.source_id == right.source_id
        && (right.selectors.is_empty()
            || right.selectors.iter().all(|selector| {
                left.selectors
                    .iter()
                    .any(|other| selectors_overlap(other, selector))
            }))
}

fn selectors_overlap(left: &MdafSelector, right: &MdafSelector) -> bool {
    match (left, right) {
        (MdafSelector::Interval { .. }, MdafSelector::Interval { .. }) => {
            interval_selectors_overlap(left, right)
        }
        (MdafSelector::Rectangle { .. }, MdafSelector::Rectangle { .. }) => {
            rectangle_selectors_overlap(left, right)
        }
        (MdafSelector::Polygon { .. }, MdafSelector::Polygon { .. })
        | (MdafSelector::Extension { .. }, MdafSelector::Extension { .. }) => left == right,
        (MdafSelector::Grid { .. }, MdafSelector::Grid { .. }) => {
            grid_selectors_overlap(left, right)
        }
        (MdafSelector::TextQuote { .. }, MdafSelector::TextQuote { .. }) => {
            text_quote_selectors_overlap(left, right)
        }
        (MdafSelector::Fragment { .. }, MdafSelector::Fragment { .. }) => {
            fragment_selectors_overlap(left, right)
        }
        _ => false,
    }
}

fn interval_selectors_overlap(left: &MdafSelector, right: &MdafSelector) -> bool {
    let (
        MdafSelector::Interval {
            unit: left_unit,
            start: left_start,
            end: left_end,
            ..
        },
        MdafSelector::Interval {
            unit: right_unit,
            start: right_start,
            end: right_end,
            ..
        },
    ) = (left, right)
    else {
        return false;
    };
    left_unit == right_unit && left_start < right_end && right_start < left_end
}

fn rectangle_selectors_overlap(left: &MdafSelector, right: &MdafSelector) -> bool {
    let (
        MdafSelector::Rectangle {
            unit: left_unit,
            x: left_x,
            y: left_y,
            width: left_width,
            height: left_height,
        },
        MdafSelector::Rectangle {
            unit: right_unit,
            x: right_x,
            y: right_y,
            width: right_width,
            height: right_height,
        },
    ) = (left, right)
    else {
        return false;
    };
    left_unit == right_unit
        && left_x < &(right_x + right_width)
        && right_x < &(left_x + left_width)
        && left_y < &(right_y + right_height)
        && right_y < &(left_y + left_height)
}

fn grid_selectors_overlap(left: &MdafSelector, right: &MdafSelector) -> bool {
    let (
        MdafSelector::Grid {
            sheet: left_sheet,
            row_start: left_row_start,
            row_end: left_row_end,
            column_start: left_column_start,
            column_end: left_column_end,
        },
        MdafSelector::Grid {
            sheet: right_sheet,
            row_start: right_row_start,
            row_end: right_row_end,
            column_start: right_column_start,
            column_end: right_column_end,
        },
    ) = (left, right)
    else {
        return false;
    };
    left_sheet == right_sheet
        && left_row_start < right_row_end
        && right_row_start < left_row_end
        && left_column_start < right_column_end
        && right_column_start < left_column_end
}

fn fragment_selectors_overlap(left: &MdafSelector, right: &MdafSelector) -> bool {
    let (
        MdafSelector::Fragment {
            value: left_value,
            conforms_to: left_conforms,
        },
        MdafSelector::Fragment {
            value: right_value,
            conforms_to: right_conforms,
        },
    ) = (left, right)
    else {
        return false;
    };
    left_value == right_value
        && (left_conforms == right_conforms || left_conforms.is_none() || right_conforms.is_none())
}

fn text_quote_selectors_overlap(left: &MdafSelector, right: &MdafSelector) -> bool {
    let (
        MdafSelector::TextQuote {
            exact: left_exact, ..
        },
        MdafSelector::TextQuote {
            exact: right_exact, ..
        },
    ) = (left, right)
    else {
        return false;
    };
    left_exact == right_exact
}

fn apply_edits(source: &str, edits: &[TextEdit], path: &str) -> Result<String, AppError> {
    let mut edits = edits.to_vec();
    edits.sort_by(|left, right| right.start.cmp(&left.start));
    let mut previous_start = source.len();
    let mut updated = source.to_string();
    for edit in edits {
        if edit.start > edit.end || edit.end > previous_start || edit.end > updated.len() {
            return Err(AppError::operation(format!(
                "overlapping or invalid artifact link edits in `{path}`"
            )));
        }
        if !updated.is_char_boundary(edit.start) || !updated.is_char_boundary(edit.end) {
            return Err(AppError::operation(format!(
                "artifact link edit in `{path}` is not on UTF-8 boundaries"
            )));
        }
        updated.replace_range(edit.start..edit.end, &edit.replacement);
        previous_start = edit.start;
    }
    Ok(updated)
}

fn add_source_frontmatter(
    content: &str,
    identity: &str,
    spans: &[SourceByteSpan],
    source_map: Option<&MdafSourceMap>,
) -> Result<String, AppError> {
    let (frontmatter, body) =
        parse_frontmatter_document(content, false).map_err(AppError::operation)?;
    let mut frontmatter = frontmatter.unwrap_or_default();
    let vulcan_key = YamlValue::String("vulcan".to_string());
    let source_key = YamlValue::String("source".to_string());
    let vulcan = frontmatter
        .entry(vulcan_key)
        .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
    let vulcan = vulcan.as_mapping_mut().ok_or_else(|| {
        AppError::operation("existing `vulcan` frontmatter must be a mapping for artifact import")
    })?;
    if vulcan.contains_key(&source_key) {
        return Err(AppError::operation(
            "existing `vulcan.source` frontmatter would be overwritten by artifact import",
        ));
    }
    let provenance = NoteSourceProvenance {
        artifact: identity.to_string(),
        member: "text.md",
        spans: spans
            .iter()
            .map(|span| NoteSourceSpan {
                start: span.start,
                end: span.end,
                locators: source_locators(span, source_map),
            })
            .collect(),
    };
    vulcan.insert(
        source_key,
        serde_yaml::to_value(provenance).map_err(AppError::operation)?,
    );
    render_note_from_parts(Some(&frontmatter), &body).map_err(AppError::operation)
}

fn source_locators(
    span: &SourceByteSpan,
    source_map: Option<&MdafSourceMap>,
) -> Vec<NoteSourceLocator> {
    let Some(source_map) = source_map else {
        return Vec::new();
    };
    source_map
        .mappings
        .iter()
        .filter(|mapping| mapping.document.start < span.end && span.start < mapping.document.end)
        .map(|mapping| NoteSourceLocator {
            source_id: mapping.source.source_id.clone(),
            selectors: mapping.source.selectors.clone(),
            confidence: mapping.confidence,
            method: mapping.method.clone(),
        })
        .collect()
}

fn import_decomposition_diagnostic(
    diagnostic: &DecompositionDiagnostic,
) -> ArtifactImportDiagnostic {
    ArtifactImportDiagnostic {
        code: diagnostic.code.clone(),
        message: diagnostic.message.clone(),
        path: None,
    }
}

fn apply_import(
    paths: &VaultPaths,
    artifact: &MdafArtifact,
    plan: &DecompositionPlan,
    assets: &[ArtifactImportAsset],
    destination: &str,
) -> Result<(), AppError> {
    let apply_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        for note in &plan.notes {
            secure_create(paths.vault_root(), Path::new(&note.path), &note.content)?;
        }
        for asset in assets {
            let mut file = secure_create_file(paths.vault_root(), Path::new(&asset.vault_path))?;
            artifact.copy_member_to(&asset.member_path, &mut file)?;
            file.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = apply_result {
        rollback_destination(paths, destination);
        return Err(AppError::operation(format!(
            "failed to apply artifact import; removed partial destination: {error}"
        )));
    }
    if let Err(error) = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental) {
        rollback_destination(paths, destination);
        let _ = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental);
        return Err(AppError::operation(format!(
            "failed to refresh the cache after artifact import; removed imported destination: {error}"
        )));
    }
    Ok(())
}

fn rollback_destination(paths: &VaultPaths, destination: &str) {
    let target = paths.vault_root().join(destination);
    if target.is_dir() {
        let _ = fs::remove_dir_all(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, scan_vault};

    fn digest(bytes: &[u8]) -> String {
        format!("blake3:{}", blake3::hash(bytes))
    }

    fn write_artifact(root: &Path, with_outline: bool) {
        fs::create_dir_all(root.join("assets")).expect("artifact dirs");
        let markdown = "# Rules\n\n## Combat\nSee [map](assets/map.png).\n\n### Damage\nHarm.\n";
        let asset = b"synthetic-png";
        fs::write(root.join("text.md"), markdown).expect("Markdown");
        fs::write(root.join("assets/map.png"), asset).expect("asset");
        let parameters = serde_json::json!({});
        let provenance = serde_json::json!({
            "version":1,
            "activities":[{
                "id":"extract","kind":"synthetic","tools":[{"name":"fixture","version":"1"}],
                "models":[],"inputs":["source:fixture"],
                "outputs":["text.md","assets/map.png","provenance.json"],"depends_on":[],
                "parameters":parameters,"parameters_digest":digest(b"{}")
            }],"redactions":[]
        });
        let provenance_bytes = serde_json::to_vec_pretty(&provenance).expect("provenance");
        fs::write(root.join("provenance.json"), &provenance_bytes).expect("provenance file");
        let mut members = vec![
            serde_json::json!({"path":"text.md","role":"primary","media_type":"text/markdown","size":markdown.len(),"digest":digest(markdown.as_bytes()),"created_by":"extract"}),
            serde_json::json!({"path":"assets/map.png","role":"asset","media_type":"image/png","size":asset.len(),"digest":digest(asset),"created_by":"extract"}),
            serde_json::json!({"path":"provenance.json","role":"provenance","media_type":"application/json","size":provenance_bytes.len(),"digest":digest(&provenance_bytes),"created_by":"extract"}),
        ];
        let mut capabilities = Vec::<&str>::new();
        if with_outline {
            let outline = serde_json::json!({
                "version":1,"document_digest":digest(markdown.as_bytes()),
                "nodes":[
                    {"id":"combat","parent":null,"level":2,"title":"Encounter","heading":{"start":9,"end":18},"section":{"start":9,"end":markdown.len()}},
                    {"id":"damage","parent":"combat","level":3,"title":"Harm","heading":{"start":markdown.find("### Damage").expect("damage heading"),"end":markdown.find("### Damage").expect("damage heading") + "### Damage".len()},"section":{"start":markdown.find("### Damage").expect("damage heading"),"end":markdown.len()}}
                ]
            });
            let bytes = serde_json::to_vec_pretty(&outline).expect("outline");
            fs::write(root.join("outline.json"), &bytes).expect("outline file");
            members.push(serde_json::json!({"path":"outline.json","role":"outline","media_type":"application/json","size":bytes.len(),"digest":digest(&bytes),"created_by":"extract"}));
            capabilities.push("outline");
            let mut value: serde_json::Value =
                serde_json::from_slice(&provenance_bytes).expect("parse provenance");
            value["activities"][0]["outputs"]
                .as_array_mut()
                .expect("outputs")
                .push(serde_json::json!("outline.json"));
            let updated = serde_json::to_vec_pretty(&value).expect("updated provenance");
            fs::write(root.join("provenance.json"), &updated).expect("updated provenance file");
            let member = members
                .iter_mut()
                .find(|member| member["path"] == "provenance.json")
                .expect("member");
            member["size"] = serde_json::json!(updated.len());
            member["digest"] = serde_json::json!(digest(&updated));
        }
        let info = serde_json::json!({
            "format":"mdaf","version":1,
            "markdown":{"path":"text.md","digest":digest(markdown.as_bytes()),"media_type":"text/markdown"},
            "producer":{"name":"synthetic","version":"1"},"members":members,
            "sources":[{"id":"fixture","media_type":"application/octet-stream","digest":digest(b"fixture")}],
            "capabilities":capabilities
        });
        fs::write(
            root.join("info.json"),
            serde_json::to_vec_pretty(&info).expect("info"),
        )
        .expect("info file");
    }

    #[test]
    fn dry_run_and_apply_import_tree_assets_frontmatter_and_cache() {
        let artifact_dir = tempdir().expect("artifact temp");
        write_artifact(artifact_dir.path(), false);
        let vault = tempdir().expect("vault temp");
        let paths = VaultPaths::new(vault.path());
        initialize_vulcan_dir(&paths).expect("init");
        scan_vault(&paths, ScanMode::Full).expect("scan");
        let mut request = ArtifactImportRequest {
            artifact: artifact_dir.path().to_path_buf(),
            destination: "Imported/Rules".to_string(),
            hierarchy: ArtifactHierarchyAuthority::Markdown,
            from_level: 2,
            through_level: 3,
            navigation: true,
            dry_run: true,
        };
        let preview = import_artifact(&paths, &request).expect("preview");
        assert!(!paths.vault_root().join("Imported").exists());
        assert_eq!(preview.assets.len(), 1);
        request.dry_run = false;
        let report = import_artifact(&paths, &request).expect("import");
        assert!(paths
            .vault_root()
            .join("Imported/Rules/assets/map.png")
            .exists());
        let combat = fs::read_to_string(paths.vault_root().join("Imported/Rules/Combat/Combat.md"))
            .expect("combat");
        assert!(combat.contains("artifact: blake3:"), "{combat}");
        assert!(report.assets[0].digest.starts_with("blake3:"));
        assert!(combat.contains("[map](../assets/map.png)"), "{combat}");
        assert_eq!(report.changed_paths.len(), report.notes.len() + 1);
        vulcan_core::resolve_note_reference(&paths, "Imported/Rules/Combat/Combat.md")
            .expect("reindexed");
    }

    #[test]
    fn outline_authority_is_explicit_and_changes_generated_titles() {
        let artifact_dir = tempdir().expect("artifact temp");
        write_artifact(artifact_dir.path(), true);
        let vault = tempdir().expect("vault temp");
        let paths = VaultPaths::new(vault.path());
        initialize_vulcan_dir(&paths).expect("init");
        let request = ArtifactImportRequest {
            artifact: artifact_dir.path().to_path_buf(),
            destination: "Outline".to_string(),
            hierarchy: ArtifactHierarchyAuthority::Outline,
            from_level: 2,
            through_level: 3,
            navigation: false,
            dry_run: true,
        };
        let report = import_artifact(&paths, &request).expect("outline preview");
        assert!(report.notes.iter().any(|note| note.title == "Encounter"));
        assert!(report.notes.iter().any(|note| note.title == "Harm"));
    }

    #[test]
    fn invalid_artifacts_and_destination_collisions_do_not_mutate_the_vault() {
        let artifact_dir = tempdir().expect("artifact temp");
        write_artifact(artifact_dir.path(), false);
        fs::write(artifact_dir.path().join("text.md"), "changed").expect("corrupt");
        let vault = tempdir().expect("vault temp");
        let paths = VaultPaths::new(vault.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::create_dir(paths.vault_root().join("Existing")).expect("existing");
        let request = ArtifactImportRequest {
            artifact: artifact_dir.path().to_path_buf(),
            destination: "New".to_string(),
            hierarchy: ArtifactHierarchyAuthority::Markdown,
            from_level: 2,
            through_level: 3,
            navigation: false,
            dry_run: false,
        };
        assert!(import_artifact(&paths, &request).is_err());
        assert!(!paths.vault_root().join("New").exists());
        let collision = ArtifactImportRequest {
            destination: "existing".to_string(),
            dry_run: true,
            ..request
        };
        assert!(import_artifact(&paths, &collision).is_err());
    }

    #[test]
    fn uniquely_resolvable_source_reference_targets_the_owning_note() {
        let markdown =
            "# Rules\n\nSee [later](page-ref).\n\n## Combat\nFight.\n\n## Magic\nCast.\n";
        let config = vulcan_core::VaultConfig::default();
        let plan = plan_document_decomposition(
            "Book/text.md",
            markdown,
            &config,
            &DecompositionOptions {
                from_level: 2,
                through_level: 2,
                destination_root: "Book".to_string(),
                navigation: false,
            },
        )
        .expect("plan");
        let parsed = parse_document(markdown, &config);
        let link = parsed.links.first().expect("source reference link");
        let magic_offset = markdown.find("## Magic").expect("magic offset");
        let source_map = MdafSourceMap {
            version: 1,
            document_digest: format!("blake3:{}", "0".repeat(64)),
            mappings: vec![vulcan_core::artifact::MdafSourceMapping {
                document: vulcan_core::artifact::MdafByteSpan {
                    start: magic_offset,
                    end: markdown.len(),
                },
                source: ArtifactSourceLocator {
                    source_id: "fixture".to_string(),
                    selectors: vec![MdafSelector::Interval {
                        unit: "page".to_string(),
                        start: 2.0,
                        end: 3.0,
                        origin: Some(1.0),
                        label_start: None,
                        label_end: None,
                    }],
                },
                confidence: Some(1.0),
                method: Some("fixture/alignment".to_string()),
            }],
            references: vec![vulcan_core::artifact::MdafSourceReference {
                document: vulcan_core::artifact::MdafByteSpan {
                    start: link.byte_offset,
                    end: link.byte_offset + link.raw_text.len(),
                },
                target: ArtifactSourceLocator {
                    source_id: "fixture".to_string(),
                    selectors: vec![MdafSelector::Interval {
                        unit: "page".to_string(),
                        start: 2.0,
                        end: 3.0,
                        origin: Some(1.0),
                        label_start: Some("2".to_string()),
                        label_end: None,
                    }],
                },
                kind: Some("page-reference".to_string()),
            }],
        };
        let mut diagnostics = Vec::new();
        let target = import_link_target(link, &plan, &[], Some(&source_map), &mut diagnostics)
            .expect("unique target");
        assert_eq!(target.0, "Book/Magic.md");
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn source_locator_matching_composes_temporal_spatial_and_grid_selectors() {
        let mapping = ArtifactSourceLocator {
            source_id: "video".to_string(),
            selectors: vec![
                MdafSelector::Interval {
                    unit: "millisecond".to_string(),
                    start: 1000.0,
                    end: 5000.0,
                    origin: Some(0.0),
                    label_start: None,
                    label_end: None,
                },
                MdafSelector::Rectangle {
                    unit: "normalized".to_string(),
                    x: 0.1,
                    y: 0.1,
                    width: 0.5,
                    height: 0.5,
                },
            ],
        };
        let target = ArtifactSourceLocator {
            source_id: "video".to_string(),
            selectors: vec![
                MdafSelector::Interval {
                    unit: "millisecond".to_string(),
                    start: 2000.0,
                    end: 3000.0,
                    origin: None,
                    label_start: None,
                    label_end: None,
                },
                MdafSelector::Rectangle {
                    unit: "normalized".to_string(),
                    x: 0.2,
                    y: 0.2,
                    width: 0.1,
                    height: 0.1,
                },
            ],
        };
        assert!(source_locators_overlap(&mapping, &target));

        let outside_time = ArtifactSourceLocator {
            selectors: vec![MdafSelector::Interval {
                unit: "millisecond".to_string(),
                start: 6000.0,
                end: 7000.0,
                origin: None,
                label_start: None,
                label_end: None,
            }],
            ..target.clone()
        };
        assert!(!source_locators_overlap(&mapping, &outside_time));

        let table = ArtifactSourceLocator {
            source_id: "sheet".to_string(),
            selectors: vec![MdafSelector::Grid {
                sheet: Some("Data".to_string()),
                row_start: 2,
                row_end: 8,
                column_start: 1,
                column_end: 4,
            }],
        };
        let cell = ArtifactSourceLocator {
            source_id: "sheet".to_string(),
            selectors: vec![MdafSelector::Grid {
                sheet: Some("Data".to_string()),
                row_start: 3,
                row_end: 4,
                column_start: 2,
                column_end: 3,
            }],
        };
        assert!(source_locators_overlap(&table, &cell));
    }

    #[test]
    fn member_copy_failure_rolls_back_the_new_destination() {
        let artifact_dir = tempdir().expect("artifact temp");
        write_artifact(artifact_dir.path(), false);
        let artifact = inspect_mdaf(artifact_dir.path()).expect("inspect artifact");
        assert!(artifact.valid);
        fs::remove_file(artifact_dir.path().join("assets/map.png")).expect("remove asset");

        let vault = tempdir().expect("vault temp");
        let paths = VaultPaths::new(vault.path());
        initialize_vulcan_dir(&paths).expect("init");
        let config = vulcan_core::VaultConfig::default();
        let plan = plan_document_decomposition(
            "Rollback/text.md",
            artifact.markdown.as_deref().expect("Markdown"),
            &config,
            &DecompositionOptions {
                from_level: 2,
                through_level: 3,
                destination_root: "Rollback".to_string(),
                navigation: false,
            },
        )
        .expect("plan");
        let assets = artifact_assets(&artifact, "Rollback");
        assert!(apply_import(&paths, &artifact, &plan, &assets, "Rollback").is_err());
        assert!(!paths.vault_root().join("Rollback").exists());
    }
}
