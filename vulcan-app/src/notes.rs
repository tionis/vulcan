use crate::mdbase::{
    apply_managed_mdbase_note_write, MdbaseManagedNoteWriteRequest, MdbaseManagedWriteMode,
    MdbaseWriteOperation,
};
use crate::plugins;
use crate::templates::{
    find_frontmatter_block, load_named_template, parse_frontmatter_document,
    render_creation_trigger_with_staged_creates, render_loaded_template_with_authority,
    render_loaded_template_with_staged_creates, render_note_from_parts,
    staged_template_create_snapshot, staged_template_creates, LoadedTemplateRenderRequest,
    StagedTemplateCreates, TemplateEngineKind, TemplateRunMode, TemplateTimestamp, YamlMapping,
    YamlValue,
};
use crate::AppError;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use vulcan_core::html::HtmlRenderOptions;
use vulcan_core::mdbase::is_mdbase_record_path;
use vulcan_core::paths::{
    normalize_relative_input_path, secure_create_atomic, secure_read_to_string, secure_replace,
    RelativePathOptions,
};
use vulcan_core::properties::{extract_indexed_properties, load_note_index};
use vulcan_core::Verbosity;
use vulcan_core::{
    expected_periodic_note_path, load_vault_config, match_periodic_note_path, parse_document,
    parse_dql_with_diagnostics, period_range_for_date, query_backlinks,
    query_backlinks_with_filter, query_links_with_filter, query_note_link_confidence_with_filter,
    render_note_fragment_html, render_note_html, render_vault_html, resolve_link,
    resolve_note_reference, resolve_note_reference_with_filter, resolve_permission_profile,
    BacklinkRecord, DoctorByteRange, DoctorDiagnosticIssue, GraphConfidenceBreakdown,
    GraphQueryError, LinkResolutionProblem, NoteLineSpan, NoteMatchKind, ParsedDocument,
    PeriodicConfig, PermissionFilter, PermissionGuard, PluginEvent, ProfilePermissionGuard,
    RefactorChange, ResolverDocument, ResolverLink, VaultConfig, VaultPaths,
};

#[derive(Debug, Clone)]
pub struct NoteCreateRequest {
    pub path: String,
    pub template: Option<String>,
    pub frontmatter: Option<YamlMapping>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteCreateReport {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
    #[serde(skip)]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteCreateCommandReport {
    pub path: String,
    pub created: bool,
    pub checked: bool,
    pub template: Option<String>,
    pub engine: Option<String>,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<DoctorDiagnosticIssue>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

pub fn finish_note_create_report(
    paths: &VaultPaths,
    report: NoteCreateReport,
    check: bool,
) -> Result<NoteCreateCommandReport, AppError> {
    let diagnostics = if check {
        diagnose_note_contents(paths, &report.path, &report.content)?
    } else {
        Vec::new()
    };
    Ok(NoteCreateCommandReport {
        path: report.path,
        created: true,
        checked: check,
        template: report.template,
        engine: report.engine,
        warnings: report.warnings,
        diagnostics,
        changed_paths: report.changed_paths,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteAppendMode {
    Append,
    Prepend,
    AfterHeading,
}

impl NoteAppendMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Prepend => "prepend",
            Self::AfterHeading => "after_heading",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NoteAppendRequest {
    pub note: Option<String>,
    pub text: String,
    pub mode: NoteAppendMode,
    pub heading: Option<String>,
    pub periodic: Option<String>,
    pub date: Option<String>,
    pub vars: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteAppendReport {
    pub path: String,
    pub mode: String,
    pub created: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_date: Option<String>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
    #[serde(skip)]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteAppendCommandReport {
    pub path: String,
    pub appended: bool,
    pub mode: String,
    pub checked: bool,
    pub created: bool,
    pub heading: Option<String>,
    pub period_type: Option<String>,
    pub reference_date: Option<String>,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<DoctorDiagnosticIssue>,
}

pub fn finish_note_append_report(
    paths: &VaultPaths,
    report: NoteAppendReport,
    check: bool,
) -> Result<NoteAppendCommandReport, AppError> {
    let diagnostics = if check {
        diagnose_note_contents(paths, &report.path, &report.content)?
    } else {
        Vec::new()
    };
    Ok(NoteAppendCommandReport {
        path: report.path,
        appended: true,
        mode: report.mode,
        checked: check,
        created: report.created,
        heading: report.heading,
        period_type: report.period_type,
        reference_date: report.reference_date,
        warnings: report.warnings,
        diagnostics,
    })
}

#[derive(Debug, Clone)]
pub struct NoteSetRequest {
    pub note: String,
    pub replacement: String,
    pub preserve_frontmatter: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteSetReport {
    pub path: String,
    pub preserved_frontmatter: bool,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
    #[serde(skip)]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteSetCommandReport {
    pub path: String,
    pub checked: bool,
    pub preserved_frontmatter: bool,
    pub diagnostics: Vec<DoctorDiagnosticIssue>,
}

pub fn finish_note_set_report(
    paths: &VaultPaths,
    report: NoteSetReport,
    check: bool,
) -> Result<NoteSetCommandReport, AppError> {
    let diagnostics = if check {
        diagnose_note_contents(paths, &report.path, &report.content)?
    } else {
        Vec::new()
    };
    Ok(NoteSetCommandReport {
        path: report.path,
        checked: check,
        preserved_frontmatter: report.preserved_frontmatter,
        diagnostics,
    })
}

#[derive(Debug, Clone)]
pub struct MarkdownTarget {
    pub display_path: String,
    pub absolute_path: PathBuf,
    pub vault_relative_path: Option<String>,
    pub config: VaultConfig,
}

impl MarkdownTarget {
    #[must_use]
    pub fn is_vault_managed(&self) -> bool {
        self.vault_relative_path.is_some()
    }

    pub fn read_source(&self) -> Result<String, AppError> {
        fs::read_to_string(&self.absolute_path).map_err(AppError::operation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteReadMode {
    Markdown,
    Html,
}

impl NoteReadMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NoteGetOptions<'a> {
    pub note: &'a str,
    pub mode: NoteReadMode,
    pub section_id: Option<&'a str>,
    pub heading: Option<&'a str>,
    pub block_ref: Option<&'a str>,
    pub lines: Option<&'a str>,
    pub match_pattern: Option<&'a str>,
    pub context: usize,
    pub no_frontmatter: bool,
    pub raw: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteGetReport {
    pub path: String,
    pub content: String,
    pub frontmatter: Option<JsonValue>,
    pub metadata: NoteGetMetadata,
    #[serde(skip)]
    pub display_lines: Vec<vulcan_core::NoteSelectedLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteInfoReport {
    pub path: String,
    pub matched_by: NoteMatchKind,
    pub word_count: usize,
    pub heading_count: usize,
    pub outgoing_link_count: usize,
    pub backlink_count: usize,
    pub alias_count: usize,
    pub tag_count: usize,
    pub file_size: i64,
    pub tags: Vec<String>,
    pub frontmatter_keys: Vec<String>,
    pub created_at_ms: Option<i64>,
    pub created_at: Option<String>,
    pub modified_at_ms: Option<i64>,
    pub modified_at: Option<String>,
    pub link_confidence: GraphConfidenceBreakdown,
}

pub fn build_note_info_report(
    paths: &VaultPaths,
    note: &str,
    read_filter: Option<&PermissionFilter>,
) -> Result<NoteInfoReport, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    let resolved = resolve_note_reference_with_filter(paths, note, read_filter)
        .map_err(AppError::operation)?;
    let absolute_path = paths.vault_root().join(&resolved.path);
    let source = fs::read_to_string(&absolute_path).map_err(AppError::operation)?;
    let metadata = fs::metadata(&absolute_path).map_err(AppError::operation)?;
    let config = load_vault_config(paths).config;
    let parsed = parse_document(&source, &config);
    let outgoing =
        query_links_with_filter(paths, &resolved.path, read_filter).map_err(AppError::operation)?;
    let backlinks = query_backlinks_with_filter(paths, &resolved.path, read_filter)
        .map_err(AppError::operation)?;

    let mut tags = parsed
        .tags
        .iter()
        .map(|tag| tag.tag_text.clone())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();

    let mut frontmatter_keys = parsed
        .frontmatter
        .as_ref()
        .and_then(|frontmatter| frontmatter.as_mapping())
        .map(|mapping| {
            mapping
                .keys()
                .filter_map(|value| value.as_str())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    frontmatter_keys.sort();

    let modified_at_ms = metadata
        .modified()
        .ok()
        .or_else(|| metadata.created().ok())
        .and_then(system_time_to_millis);
    let created_at_ms = metadata
        .created()
        .ok()
        .or_else(|| metadata.modified().ok())
        .and_then(system_time_to_millis)
        .or(modified_at_ms);

    Ok(NoteInfoReport {
        link_confidence: query_note_link_confidence_with_filter(paths, &resolved.path, read_filter)
            .map_err(AppError::operation)?,
        path: resolved.path,
        matched_by: resolved.matched_by,
        word_count: note_word_count(&source),
        heading_count: parsed.headings.len(),
        outgoing_link_count: outgoing.links.len(),
        backlink_count: backlinks.backlinks.len(),
        alias_count: parsed.aliases.len(),
        tag_count: tags.len(),
        file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        tags,
        frontmatter_keys,
        created_at_ms,
        created_at: created_at_ms.map(format_utc_timestamp_ms),
        modified_at_ms,
        modified_at: modified_at_ms.map(format_utc_timestamp_ms),
    })
}

fn note_word_count(source: &str) -> usize {
    let body = find_frontmatter_block(source).map_or(source, |(_, _, end)| &source[end..]);
    body.lines()
        .filter_map(normalize_note_word_line)
        .flat_map(str::split_whitespace)
        .count()
}

fn normalize_note_word_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_block_ref_only_line(trimmed) {
        return None;
    }
    let trimmed = if let Some(level) = markdown_heading_level(trimmed) {
        trimmed[level..].trim()
    } else {
        trimmed
    };
    let trimmed = strip_markdown_list_marker(trimmed).trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn is_block_ref_only_line(line: &str) -> bool {
    line.starts_with('^')
        && line.len() > 1
        && line[1..]
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn strip_markdown_list_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest;
        }
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = trimmed[digits..].strip_prefix(". ") {
            return rest;
        }
    }
    trimmed
}

fn system_time_to_millis(time: std::time::SystemTime) -> Option<i64> {
    let duration = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

fn format_utc_timestamp_ms(ms: i64) -> String {
    TemplateTimestamp::from_millis(ms)
        .default_strings()
        .datetime
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct NoteGetMetadata {
    pub mode: String,
    pub section_id: Option<String>,
    pub heading: Option<String>,
    pub block_ref: Option<String>,
    pub lines: Option<String>,
    pub match_pattern: Option<String>,
    pub context: usize,
    pub no_frontmatter: bool,
    pub raw: bool,
    pub match_count: usize,
    pub total_lines: usize,
    pub has_more_before: bool,
    pub has_more_after: bool,
    pub line_spans: Vec<vulcan_core::NoteLineSpan>,
}

pub fn read_note(
    paths: &VaultPaths,
    options: NoteGetOptions<'_>,
) -> Result<NoteGetReport, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    let NoteGetOptions {
        note,
        mode,
        section_id,
        heading,
        block_ref,
        lines,
        match_pattern,
        context,
        no_frontmatter,
        raw,
    } = options;
    let target = resolve_existing_markdown_target(paths, note)?;
    let source = target.read_source()?;
    let parsed = parse_document(&source, &target.config);
    let selection = vulcan_core::read_note(
        &source,
        &parsed,
        &vulcan_core::NoteReadOptions {
            heading: heading.map(ToOwned::to_owned),
            section_id: section_id.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: match_pattern.map(ToOwned::to_owned),
            context,
            no_frontmatter,
        },
    )
    .map_err(AppError::operation)?;
    let full_document = selection.selected_lines.len() == selection.total_lines
        && selection
            .selected_lines
            .iter()
            .enumerate()
            .all(|(expected, actual)| actual.line_number == expected + 1);
    let content = match mode {
        NoteReadMode::Markdown => selection.content.clone(),
        NoteReadMode::Html if full_document && !no_frontmatter => {
            target.vault_relative_path.as_deref().map_or_else(
                || {
                    render_vault_html(
                        paths,
                        &selection.content,
                        &HtmlRenderOptions {
                            full_document: true,
                            ..HtmlRenderOptions::default()
                        },
                    )
                    .html
                },
                |path| render_note_html(paths, path, &selection.content).html,
            )
        }
        NoteReadMode::Html => {
            render_note_fragment_html(
                paths,
                target.vault_relative_path.as_deref(),
                &selection.content,
            )
            .html
        }
    };
    let frontmatter = parsed
        .frontmatter
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(AppError::operation)?;
    Ok(NoteGetReport {
        path: target.display_path,
        content,
        frontmatter,
        metadata: NoteGetMetadata {
            mode: mode.as_str().to_string(),
            section_id: selection.section_id.clone(),
            heading: heading.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: match_pattern.map(ToOwned::to_owned),
            context,
            no_frontmatter,
            raw,
            match_count: selection.match_count,
            total_lines: selection.total_lines,
            has_more_before: selection.has_more_before,
            has_more_after: selection.has_more_after,
            line_spans: selection.line_spans.clone(),
        },
        display_lines: selection.selected_lines,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteOutlineReport {
    pub path: String,
    pub total_lines: usize,
    pub frontmatter_span: Option<vulcan_core::NoteLineSpan>,
    pub scope_section: Option<vulcan_core::NoteOutlineSection>,
    pub depth_limit: Option<usize>,
    pub sections: Vec<vulcan_core::NoteOutlineSection>,
    pub block_refs: Vec<vulcan_core::NoteOutlineBlockRef>,
}

pub fn read_note_outline(
    paths: &VaultPaths,
    note: &str,
    section_id: Option<&str>,
    depth: Option<usize>,
) -> Result<NoteOutlineReport, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    if matches!(depth, Some(0)) {
        return Err(AppError::operation(
            "`note outline --depth` must be at least 1",
        ));
    }
    let target = resolve_existing_markdown_target(paths, note)?;
    let source = target.read_source()?;
    let parsed = parse_document(&source, &target.config);
    let outline = vulcan_core::outline_note(&source, &parsed);
    let selection = vulcan_core::select_note_outline(
        &outline,
        &vulcan_core::NoteOutlineOptions {
            section_id: section_id.map(ToOwned::to_owned),
            depth,
        },
    )
    .map_err(AppError::operation)?;
    Ok(NoteOutlineReport {
        path: target.display_path,
        total_lines: selection.total_lines,
        frontmatter_span: selection.frontmatter_span,
        scope_section: selection.scope_section,
        depth_limit: depth,
        sections: selection.sections,
        block_refs: selection.block_refs,
    })
}

/// Resolve a note identifier or an explicit Markdown path for read workflows.
/// A direct file outside the vault uses default parsing configuration and carries no vault authority.
pub fn resolve_existing_markdown_target(
    paths: &VaultPaths,
    note: &str,
) -> Result<MarkdownTarget, AppError> {
    if let Ok(relative_path) = resolve_existing_note_path(paths, note) {
        let absolute_path = paths.vault_root().join(&relative_path);
        return Ok(MarkdownTarget {
            display_path: relative_path.clone(),
            absolute_path,
            vault_relative_path: Some(relative_path),
            config: load_vault_config(paths).config,
        });
    }

    if note_argument_looks_like_path(note) {
        return resolve_existing_direct_markdown_target(paths, note);
    }

    Err(AppError::operation(format!("note not found: {note}")))
}

/// Check the authority for a Markdown source, including explicit paths outside the vault.
pub fn check_read_markdown_source_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    note: &str,
) -> Result<(), AppError> {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let target = resolve_existing_markdown_target(paths, note)?;
    let Some(relative_path) = target.vault_relative_path.as_deref() else {
        return Err(AppError::operation(format!(
            "permission profiles cannot read markdown files outside the selected vault root: {}",
            target.display_path
        )));
    };
    guard
        .check_read_path(relative_path)
        .map_err(AppError::operation)
}

/// Check the authority for editing an existing Markdown source.
pub fn check_write_markdown_source_access(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    note: &str,
) -> Result<(), AppError> {
    if guard.write_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return Ok(());
    }
    let target = resolve_existing_markdown_target(paths, note)?;
    let Some(relative_path) = target.vault_relative_path.as_deref() else {
        return Err(AppError::operation(format!(
            "permission profiles cannot write markdown files outside the selected vault root: {}",
            target.display_path
        )));
    };
    guard
        .check_write_path(relative_path)
        .map_err(AppError::operation)
}

fn note_argument_looks_like_path(note: &str) -> bool {
    let path = Path::new(note);
    path.is_absolute()
        || path.extension().is_some()
        || note.starts_with('.')
        || path.components().count() > 1
}

fn resolve_existing_direct_markdown_target(
    paths: &VaultPaths,
    note: &str,
) -> Result<MarkdownTarget, AppError> {
    let current_dir = std::env::current_dir().map_err(AppError::operation)?;
    for candidate in direct_markdown_path_candidates(note) {
        if !has_markdown_extension(&candidate) {
            continue;
        }

        let absolute_candidate = if candidate.is_absolute() {
            candidate.clone()
        } else {
            current_dir.join(&candidate)
        };
        if !absolute_candidate.is_file() {
            continue;
        }

        let absolute_path = fs::canonicalize(&absolute_candidate).map_err(AppError::operation)?;
        let vault_relative_path = paths
            .relative_to_vault(&absolute_path)
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        let display_path = vault_relative_path.clone().unwrap_or_else(|| {
            if candidate.is_absolute() {
                absolute_candidate.to_string_lossy().into_owned()
            } else {
                candidate.to_string_lossy().into_owned()
            }
        });

        return Ok(MarkdownTarget {
            display_path,
            absolute_path,
            vault_relative_path: vault_relative_path.clone(),
            config: if vault_relative_path.is_some() {
                load_vault_config(paths).config
            } else {
                VaultConfig::default()
            },
        });
    }

    Err(AppError::operation(format!("note not found: {note}")))
}

fn direct_markdown_path_candidates(note: &str) -> Vec<PathBuf> {
    let path = PathBuf::from(note);
    let mut candidates = vec![path.clone()];
    if path.extension().is_none() {
        let mut with_extension = path;
        with_extension.set_extension("md");
        candidates.push(with_extension);
    }
    candidates
}

fn has_markdown_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

#[derive(Debug, Clone)]
pub struct NotePatchRequest {
    pub target: MarkdownTarget,
    pub section_id: Option<String>,
    pub heading: Option<String>,
    pub block_ref: Option<String>,
    pub lines: Option<String>,
    pub find: String,
    pub replace: String,
    pub replace_all: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotePatchReport {
    pub path: String,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    pub line_spans: Vec<NoteLineSpan>,
    pub regex: bool,
    pub match_count: usize,
    pub changes: Vec<RefactorChange>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
    #[serde(skip)]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotePatchCommandReport {
    pub path: String,
    pub dry_run: bool,
    pub checked: bool,
    pub section_id: Option<String>,
    pub heading: Option<String>,
    pub block_ref: Option<String>,
    pub lines: Option<String>,
    pub line_spans: Vec<NoteLineSpan>,
    pub pattern: String,
    pub regex: bool,
    pub replace: String,
    pub match_count: usize,
    pub changes: Vec<RefactorChange>,
    pub diagnostics: Vec<DoctorDiagnosticIssue>,
}

pub fn finish_note_patch_report(
    paths: &VaultPaths,
    request: &NotePatchRequest,
    report: NotePatchReport,
    check: bool,
) -> Result<NotePatchCommandReport, AppError> {
    let diagnostics = if check {
        match request.target.vault_relative_path.as_deref() {
            Some(relative_path) => diagnose_note_contents(paths, relative_path, &report.content)?,
            None => diagnose_external_markdown_contents(
                &request.target.display_path,
                &request.target.config,
                &report.content,
            )?,
        }
    } else {
        Vec::new()
    };
    Ok(NotePatchCommandReport {
        path: report.path,
        dry_run: report.dry_run,
        checked: check,
        section_id: report.section_id,
        heading: request.heading.clone(),
        block_ref: request.block_ref.clone(),
        lines: request.lines.clone(),
        line_spans: report.line_spans,
        pattern: request.find.clone(),
        regex: report.regex,
        replace: request.replace.clone(),
        match_count: report.match_count,
        changes: report.changes,
        diagnostics,
    })
}

#[derive(Debug, Clone)]
pub struct NoteDeleteRequest {
    pub note: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteDeleteReport {
    pub path: String,
    pub dry_run: bool,
    pub deleted: bool,
    pub backlink_count: usize,
    pub backlinks: Vec<BacklinkRecord>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
enum NotePatchMatcher {
    Literal(String),
    Regex(Regex),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotePatchApplication {
    updated_content: String,
    match_count: usize,
    changes: Vec<RefactorChange>,
    regex: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotePatchScopeSelection {
    section_id: Option<String>,
    line_spans: Vec<NoteLineSpan>,
    byte_start: usize,
    byte_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeriodicTarget {
    pub period_type: String,
    pub reference_date: String,
    pub start_date: String,
    pub end_date: String,
    pub path: String,
}

pub fn apply_note_create(
    paths: &VaultPaths,
    request: &NoteCreateRequest,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<NoteCreateReport, AppError> {
    let requested_path = normalize_note_path(&request.path)?;
    let config = load_vault_config(paths).config;
    let mutation_guard = note_create_mutation_guard(paths, permission_profile)?;
    let mut warnings = Vec::new();
    let mut frontmatter = request.frontmatter.clone();
    let mut body = request.body.clone();
    let mut final_path = requested_path.clone();
    let mut template = None;
    let mut engine = None;
    let mut changed_paths = Vec::new();
    let mut triggered_content = None;
    let staged_creates = staged_template_creates();

    if let Some(template_name) = request.template.as_deref() {
        let loaded = load_named_template(paths, &config, template_name)?;
        let vars = HashMap::new();
        let rendered = render_loaded_template_with_staged_creates(
            paths,
            &config,
            &loaded,
            &LoadedTemplateRenderRequest {
                target_path: &requested_path,
                target_contents: None,
                engine: TemplateEngineKind::Auto,
                vars: &vars,
                allow_mutations: true,
                run_mode: TemplateRunMode::Create,
                reference_date: None,
            },
            None,
            mutation_guard.as_ref(),
            Some(staged_creates.clone()),
        )?;
        let (template_frontmatter, template_body) =
            parse_frontmatter_document(&rendered.content, true).map_err(AppError::operation)?;
        frontmatter = merge_explicit_frontmatter(template_frontmatter, frontmatter);
        body = merge_note_create_bodies(&template_body, &body);
        final_path.clone_from(&rendered.target_path);
        warnings.extend(loaded.template.warning);
        warnings.extend(rendered.warnings.clone());
        warnings.extend(rendered.diagnostics);
        changed_paths.extend(rendered.changed_paths);
        template = Some(template_name.to_string());
        engine = Some(rendered.engine.as_str().to_string());
    } else {
        let initial_content =
            render_note_from_parts(frontmatter.as_ref(), &body).map_err(AppError::operation)?;
        if let Some(rendered) = render_creation_trigger_with_staged_creates(
            paths,
            &config,
            &requested_path,
            &initial_content,
            None,
            mutation_guard.as_ref(),
            Some(staged_creates.clone()),
        )? {
            final_path.clone_from(&rendered.target_path);
            template = rendered.template;
            engine = Some(rendered.engine.as_str().to_string());
            warnings.extend(rendered.warnings);
            warnings.extend(rendered.diagnostics);
            changed_paths.extend(rendered.changed_paths);
            triggered_content = Some(rendered.content);
        }
    }

    let absolute_path = paths.vault_root().join(&final_path);
    if let Some(guard) = mutation_guard.as_ref() {
        guard
            .check_write_path(&final_path)
            .map_err(AppError::operation)?;
    }
    if absolute_path.exists() {
        return Err(AppError::operation(format!(
            "destination note already exists: {final_path}"
        )));
    }

    let content = if let Some(content) = triggered_content {
        content
    } else {
        render_note_from_parts(frontmatter.as_ref(), &body).map_err(AppError::operation)?
    };
    let content = persist_note_create_with_template_effects(
        paths,
        &final_path,
        &content,
        &staged_creates,
        permission_profile,
        verbosity,
    )?;
    changed_paths.push(final_path.clone());
    changed_paths.sort();
    changed_paths.dedup();

    Ok(NoteCreateReport {
        path: final_path,
        template,
        engine,
        warnings,
        changed_paths,
        content,
    })
}

fn note_create_mutation_guard(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
) -> Result<Option<ProfilePermissionGuard>, AppError> {
    permission_profile
        .map(|profile| {
            resolve_permission_profile(paths, Some(profile))
                .map(|selection| ProfilePermissionGuard::new(paths, selection))
                .map_err(AppError::operation)
        })
        .transpose()
}

pub(crate) fn persist_note_create_with_template_effects(
    paths: &VaultPaths,
    path: &str,
    content: &str,
    staged_creates: &StagedTemplateCreates,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<String, AppError> {
    let staged = staged_template_create_snapshot(staged_creates)?;
    if staged.is_empty() {
        persist_note_create_content(paths, path, content, permission_profile, verbosity)
    } else {
        persist_note_create_with_staged_creates(
            paths,
            path,
            content,
            &staged,
            permission_profile,
            verbosity,
        )?;
        Ok(content.to_string())
    }
}

fn persist_note_create_content(
    paths: &VaultPaths,
    path: &str,
    content: &str,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<String, AppError> {
    if note_path_is_mdbase_managed(paths, path, permission_profile)? {
        return apply_mdbase_note_content_change(
            paths,
            &MdbaseManagedNoteWriteRequest {
                path,
                before: None,
                after: Some(content),
                operation: MdbaseWriteOperation::Create,
                mode: MdbaseManagedWriteMode::Validated,
                dry_run: false,
                permission_profile,
                verbosity,
            },
        );
    }
    dispatch_note_write_plugin_hooks(
        paths,
        permission_profile,
        path,
        "create",
        None,
        content,
        verbosity,
    )?;
    write_ordinary_note_if_unchanged_with_profile(
        paths,
        path,
        None,
        content,
        "create",
        permission_profile,
    )?;
    dispatch_note_create_plugin_hooks(paths, permission_profile, path, content, verbosity);
    Ok(content.to_string())
}

fn persist_note_create_with_staged_creates(
    paths: &VaultPaths,
    path: &str,
    content: &str,
    staged: &BTreeMap<String, String>,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<(), AppError> {
    if staged.contains_key(path) {
        return Err(AppError::operation(format!(
            "template side effect conflicts with final note path: {path}"
        )));
    }
    for side_path in staged.keys() {
        if note_path_is_mdbase_managed(paths, side_path, permission_profile)? {
            return Err(AppError::operation(format!(
                "tp.file.create_new cannot create an mdbase-managed note: {side_path}"
            )));
        }
    }
    if note_path_is_mdbase_managed(paths, path, permission_profile)? {
        return Err(AppError::operation(
            "template side effects cannot be combined with an mdbase-managed note create",
        ));
    }
    dispatch_note_write_plugin_hooks(
        paths,
        permission_profile,
        path,
        "create",
        None,
        content,
        verbosity,
    )?;
    let mut changes = staged
        .iter()
        .map(
            |(side_path, side_content)| vulcan_core::ordinary_write::OrdinaryWriteChange {
                path: side_path.clone(),
                before: None,
                after: Some(side_content.clone()),
            },
        )
        .collect::<Vec<_>>();
    changes.push(vulcan_core::ordinary_write::OrdinaryWriteChange {
        path: path.to_string(),
        before: None,
        after: Some(content.to_string()),
    });
    vulcan_core::ordinary_write::apply_ordinary_write_batch_with_preflight(paths, &changes, || {
        let guard = note_create_mutation_guard(paths, permission_profile)
            .map_err(|error| error.to_string())?;
        for change in &changes {
            if note_path_is_mdbase_managed(paths, &change.path, permission_profile)
                .map_err(|error| error.to_string())?
            {
                return Err(format!(
                    "template create target became an mdbase-managed note: {}",
                    change.path
                ));
            }
            if let Some(guard) = guard.as_ref() {
                guard
                    .check_write_path(&change.path)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    })
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    dispatch_note_create_plugin_hooks(paths, permission_profile, path, content, verbosity);
    Ok(())
}

pub fn parse_note_frontmatter_bindings(
    bindings: &[String],
) -> Result<Option<YamlMapping>, AppError> {
    if bindings.is_empty() {
        return Ok(None);
    }

    let mut mapping = YamlMapping::new();
    for binding in bindings {
        let Some((key, value)) = binding.split_once('=') else {
            return Err(AppError::operation(format!(
                "frontmatter bindings must use key=value syntax: {binding}"
            )));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(AppError::operation(format!(
                "frontmatter bindings need a non-empty key: {binding}"
            )));
        }
        let parsed =
            serde_yaml::from_str::<YamlValue>(value.trim()).map_err(AppError::operation)?;
        mapping.insert(YamlValue::String(key.to_string()), parsed);
    }

    Ok(Some(mapping))
}

pub fn json_properties_to_frontmatter(
    properties: &BTreeMap<String, JsonValue>,
) -> Result<Option<YamlMapping>, AppError> {
    if properties.is_empty() {
        return Ok(None);
    }

    let mut mapping = YamlMapping::new();
    for (key, value) in properties {
        mapping.insert(
            YamlValue::String(key.clone()),
            serde_yaml::to_value(value).map_err(AppError::operation)?,
        );
    }
    Ok(Some(mapping))
}

pub fn apply_note_append(
    paths: &VaultPaths,
    request: &NoteAppendRequest,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<NoteAppendReport, AppError> {
    if request.periodic.is_some() && request.note.is_some() {
        return Err(AppError::operation(
            "`note append` accepts either a note or a periodic target, not both",
        ));
    }

    let config = load_vault_config(paths).config;
    let target = load_note_append_target(paths, &config, request, permission_profile)?;
    let rendered =
        crate::templates::render_template_request(crate::templates::TemplateRenderRequest {
            paths,
            vault_config: &config,
            templates: &[],
            template_path: None,
            template_text: &request.text,
            target_path: &target.path,
            target_contents: Some(&target.existing),
            engine: TemplateEngineKind::Native,
            vars: &request.vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Append,
            reference_date: None,
        })?;

    let mut warnings = target.warnings;
    warnings.extend(rendered.warnings);
    warnings.extend(rendered.diagnostics);

    let mut content = match request.mode {
        NoteAppendMode::Append => append_entry_at_end(&target.existing, &rendered.content),
        NoteAppendMode::Prepend => {
            prepend_entry_after_frontmatter(&target.existing, &rendered.content)
        }
        NoteAppendMode::AfterHeading => append_entry_under_heading(
            &target.existing,
            request.heading.as_deref().unwrap_or_default(),
            &rendered.content,
        ),
    };

    if note_path_is_mdbase_managed(paths, &target.path, permission_profile)? {
        content = apply_mdbase_note_content_change(
            paths,
            &MdbaseManagedNoteWriteRequest {
                path: &target.path,
                before: (!target.created).then_some(target.existing.as_str()),
                after: Some(&content),
                operation: if target.created {
                    MdbaseWriteOperation::Create
                } else {
                    MdbaseWriteOperation::Update
                },
                mode: MdbaseManagedWriteMode::Validated,
                dry_run: false,
                permission_profile,
                verbosity,
            },
        )?;
    } else {
        dispatch_note_write_plugin_hooks(
            paths,
            permission_profile,
            &target.path,
            "append",
            Some(&target.existing),
            &content,
            verbosity,
        )?;
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &target.path,
            (!target.created).then_some(target.existing.as_str()),
            &content,
            "append",
            permission_profile,
        )?;
        if target.created {
            dispatch_note_create_plugin_hooks(
                paths,
                permission_profile,
                &target.path,
                &content,
                verbosity,
            );
        }
    }
    let path = target.path.clone();

    Ok(NoteAppendReport {
        path,
        mode: request.mode.as_str().to_string(),
        created: target.created,
        heading: request.heading.clone(),
        period_type: target.period_type,
        reference_date: target.reference_date,
        warnings,
        changed_paths: vec![target.path],
        content,
    })
}

pub fn apply_note_set(
    paths: &VaultPaths,
    request: &NoteSetRequest,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<NoteSetReport, AppError> {
    let path = resolve_existing_note_path(paths, &request.note)?;
    // Managed records take the vault lock inside their journaled transaction.
    let managed = note_path_is_mdbase_managed(paths, &path, permission_profile)?;
    let existing =
        secure_read_to_string(paths.vault_root(), Path::new(&path)).map_err(AppError::operation)?;
    let mut content = if request.preserve_frontmatter {
        preserve_existing_frontmatter(&existing, &request.replacement)
    } else {
        request.replacement.clone()
    };
    if managed {
        content = apply_mdbase_note_content_change(
            paths,
            &MdbaseManagedNoteWriteRequest {
                path: &path,
                before: Some(&existing),
                after: Some(&content),
                operation: MdbaseWriteOperation::Update,
                mode: MdbaseManagedWriteMode::Validated,
                dry_run: false,
                permission_profile,
                verbosity,
            },
        )?;
    } else {
        // Hooks may dispatch their own managed mutations, so they must not run
        // under the ordinary-note lock. Recheck the source after locking to
        // reject an intervening write instead of replacing it with stale data.
        dispatch_note_write_plugin_hooks(
            paths,
            permission_profile,
            &path,
            "set",
            Some(&existing),
            &content,
            verbosity,
        )?;
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &path,
            Some(&existing),
            &content,
            "set",
            permission_profile,
        )?;
    }

    Ok(NoteSetReport {
        path: path.clone(),
        preserved_frontmatter: request.preserve_frontmatter,
        changed_paths: vec![path],
        content,
    })
}

/// Writes a vault note's new content the way note commands do: through
/// mdbase validation when the note belongs to a collection, otherwise
/// atomically and only while the note still holds `before`. `None` creates a
/// note that must not exist yet. Returns the content written, which mdbase
/// lifecycle rules may have extended. Plugin hooks are left to the caller.
pub fn write_note_content(
    paths: &VaultPaths,
    path: &str,
    before: Option<&str>,
    after: &str,
    operation: &str,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<String, AppError> {
    if note_path_is_mdbase_managed(paths, path, permission_profile)? {
        return apply_mdbase_note_content_change(
            paths,
            &MdbaseManagedNoteWriteRequest {
                path,
                before,
                after: Some(after),
                operation: if before.is_some() {
                    MdbaseWriteOperation::Update
                } else {
                    MdbaseWriteOperation::Create
                },
                mode: MdbaseManagedWriteMode::Validated,
                dry_run: false,
                permission_profile,
                verbosity,
            },
        );
    }
    write_ordinary_note_if_unchanged_with_profile(
        paths,
        path,
        before,
        after,
        operation,
        permission_profile,
    )?;
    Ok(after.to_string())
}

/// Reads a vault note for a read-modify-write: `None` when it does not exist
/// yet. Any other failure is an error, never an empty note to overwrite.
pub fn read_note_for_update(paths: &VaultPaths, path: &str) -> Result<Option<String>, AppError> {
    match secure_read_to_string(paths.vault_root(), Path::new(path)) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::operation(format!("cannot read {path}: {error}"))),
    }
}

#[cfg(test)]
pub(crate) fn write_ordinary_note_if_unchanged(
    paths: &VaultPaths,
    path: &str,
    before: Option<&str>,
    after: &str,
    operation: &str,
) -> Result<(), AppError> {
    write_ordinary_note_if_unchanged_with_profile(paths, path, before, after, operation, None)
}

pub(crate) fn write_ordinary_note_if_unchanged_with_profile(
    paths: &VaultPaths,
    path: &str,
    before: Option<&str>,
    after: &str,
    operation: &str,
    permission_profile: Option<&str>,
) -> Result<(), AppError> {
    vulcan_core::initialize_vulcan_dir(paths).map_err(AppError::operation)?;
    let _write_lock =
        vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    vulcan_core::ordinary_write::ensure_no_pending_ordinary_write_batch(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    if note_path_is_mdbase_managed(paths, path, permission_profile)? {
        return Err(AppError::operation(format!(
            "mdbase collection changed during note {operation}; retry the operation"
        )));
    }
    if let Some(before) = before {
        let current = secure_read_to_string(paths.vault_root(), Path::new(path))
            .map_err(AppError::operation)?;
        if current != before {
            return Err(AppError::operation(format!(
                "note changed during note {operation}; reread it before retrying"
            )));
        }
        secure_replace(paths.vault_root(), Path::new(path), after).map_err(AppError::operation)
    } else {
        secure_create_atomic(paths.vault_root(), Path::new(path), after)
            .map_err(AppError::operation)
    }
}

fn delete_ordinary_note_if_unchanged(
    paths: &VaultPaths,
    path: &str,
    before: &str,
    permission_profile: Option<&str>,
) -> Result<(), AppError> {
    vulcan_core::initialize_vulcan_dir(paths).map_err(AppError::operation)?;
    let _write_lock =
        vulcan_core::write_lock::acquire_write_lock(paths).map_err(AppError::operation)?;
    vulcan_core::ordinary_write::ensure_no_pending_ordinary_write_batch(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    if note_path_is_mdbase_managed(paths, path, permission_profile)? {
        return Err(AppError::operation(
            "mdbase collection changed during note delete; retry the operation",
        ));
    }
    let current =
        secure_read_to_string(paths.vault_root(), Path::new(path)).map_err(AppError::operation)?;
    if current != before {
        return Err(AppError::operation(
            "note changed during note delete; reread it before retrying",
        ));
    }
    fs::remove_file(paths.vault_root().join(path)).map_err(AppError::operation)
}

pub(crate) fn note_path_is_mdbase_managed(
    paths: &VaultPaths,
    path: &str,
    permission_profile: Option<&str>,
) -> Result<bool, AppError> {
    let selection =
        resolve_permission_profile(paths, permission_profile).map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    crate::mdbase::load_mdbase_routing_collection(paths, &guard)?
        .map(|collection| is_mdbase_record_path(&collection, path))
        .transpose()
        .map_err(AppError::operation)
        .map(|managed| managed.unwrap_or(false))
}

pub fn apply_note_patch(
    paths: &VaultPaths,
    request: &NotePatchRequest,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<NotePatchReport, AppError> {
    let source = if let Some(relative_path) = request.target.vault_relative_path.as_deref() {
        secure_read_to_string(paths.vault_root(), Path::new(relative_path))
            .map_err(AppError::operation)?
    } else {
        request.target.read_source()?
    };
    let matcher = parse_note_patch_matcher(&request.find)?;
    let scope = resolve_note_patch_scope_selection(
        &source,
        &request.target.config,
        request.section_id.as_deref(),
        request.heading.as_deref(),
        request.block_ref.as_deref(),
        request.lines.as_deref(),
    )?;
    let application = if let Some(scope) = scope.as_ref() {
        let updated_scope = apply_note_patch_to_source(
            &source[scope.byte_start..scope.byte_end],
            &matcher,
            &request.replace,
            request.replace_all,
            "selected note scope",
        )?;
        let mut updated_content = String::with_capacity(
            source.len() - (scope.byte_end - scope.byte_start)
                + updated_scope.updated_content.len(),
        );
        updated_content.push_str(&source[..scope.byte_start]);
        updated_content.push_str(&updated_scope.updated_content);
        updated_content.push_str(&source[scope.byte_end..]);
        NotePatchApplication {
            updated_content,
            match_count: updated_scope.match_count,
            changes: updated_scope.changes,
            regex: updated_scope.regex,
        }
    } else {
        apply_note_patch_to_source(
            &source,
            &matcher,
            &request.replace,
            request.replace_all,
            "note",
        )?
    };

    let content = persist_note_patch_content(
        paths,
        request,
        &source,
        &application.updated_content,
        permission_profile,
        verbosity,
    )?;

    Ok(NotePatchReport {
        path: request.target.display_path.clone(),
        dry_run: request.dry_run,
        section_id: scope
            .as_ref()
            .and_then(|selection| selection.section_id.clone()),
        line_spans: scope
            .as_ref()
            .map_or_else(Vec::new, |selection| selection.line_spans.clone()),
        regex: application.regex,
        match_count: application.match_count,
        changes: application.changes,
        changed_paths: request
            .target
            .vault_relative_path
            .clone()
            .into_iter()
            .collect(),
        content,
    })
}

fn persist_note_patch_content(
    paths: &VaultPaths,
    request: &NotePatchRequest,
    source: &str,
    content: &str,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<String, AppError> {
    if let Some(relative_path) = request.target.vault_relative_path.as_deref() {
        if note_path_is_mdbase_managed(paths, relative_path, permission_profile)? {
            return apply_mdbase_note_content_change(
                paths,
                &MdbaseManagedNoteWriteRequest {
                    path: relative_path,
                    before: Some(source),
                    after: Some(content),
                    operation: MdbaseWriteOperation::Update,
                    mode: MdbaseManagedWriteMode::Validated,
                    dry_run: request.dry_run,
                    permission_profile,
                    verbosity,
                },
            );
        } else if !request.dry_run {
            dispatch_note_write_plugin_hooks(
                paths,
                permission_profile,
                relative_path,
                "patch",
                Some(source),
                content,
                verbosity,
            )?;
            write_ordinary_note_if_unchanged_with_profile(
                paths,
                relative_path,
                Some(source),
                content,
                "patch",
                permission_profile,
            )?;
        }
    } else if !request.dry_run {
        vulcan_core::paths::write_file_atomic(&request.target.absolute_path, content)
            .map_err(AppError::operation)?;
    }
    Ok(content.to_string())
}

pub fn apply_note_delete(
    paths: &VaultPaths,
    request: &NoteDeleteRequest,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<NoteDeleteReport, AppError> {
    let path = resolve_existing_note_path(paths, &request.note)?;
    let backlinks = match query_backlinks(paths, &path) {
        Ok(report) => report.backlinks,
        Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. }) => Vec::new(),
        Err(error) => return Err(AppError::operation(error)),
    };
    let source =
        secure_read_to_string(paths.vault_root(), Path::new(&path)).map_err(AppError::operation)?;

    if note_path_is_mdbase_managed(paths, &path, permission_profile)? {
        if !apply_mdbase_note_change(
            paths,
            &MdbaseManagedNoteWriteRequest {
                path: &path,
                before: Some(&source),
                after: None,
                operation: MdbaseWriteOperation::Delete,
                mode: MdbaseManagedWriteMode::Validated,
                dry_run: request.dry_run,
                permission_profile,
                verbosity,
            },
        )? {
            return Err(AppError::operation(
                "mdbase collection changed during note delete; retry the operation",
            ));
        }
    } else if !request.dry_run {
        delete_ordinary_note_if_unchanged(paths, &path, &source, permission_profile)?;
        dispatch_note_delete_plugin_hooks(paths, permission_profile, &path, verbosity);
    }

    Ok(NoteDeleteReport {
        path: path.clone(),
        dry_run: request.dry_run,
        deleted: !request.dry_run,
        backlink_count: backlinks.len(),
        backlinks,
        changed_paths: vec![path],
    })
}

fn apply_mdbase_note_change(
    paths: &VaultPaths,
    request: &MdbaseManagedNoteWriteRequest<'_>,
) -> Result<bool, AppError> {
    apply_managed_mdbase_note_write(paths, request).map(|report| report.is_some())
}

pub(crate) fn apply_mdbase_note_content_change(
    paths: &VaultPaths,
    request: &MdbaseManagedNoteWriteRequest<'_>,
) -> Result<String, AppError> {
    let report = apply_managed_mdbase_note_write(paths, request)?.ok_or_else(|| {
        AppError::operation("mdbase collection changed during note write; retry the operation")
    })?;
    report
        .plan
        .preview
        .changes
        .into_iter()
        .find(|change| change.path == request.path)
        .and_then(|change| change.after)
        .ok_or_else(|| AppError::operation("mdbase note write did not return authoritative source"))
}

pub fn diagnose_note_contents(
    paths: &VaultPaths,
    relative_path: &str,
    content: &str,
) -> Result<Vec<DoctorDiagnosticIssue>, AppError> {
    let config = load_vault_config(paths).config;
    let parsed = parse_document(content, &config);
    let mut diagnostics = collect_parse_diagnostics(relative_path, &config, &parsed)?;
    diagnostics.extend(link_resolution_diagnostics(
        paths,
        relative_path,
        &config,
        &parsed,
    )?);
    sort_and_dedup_diagnostics(&mut diagnostics);
    Ok(diagnostics)
}

pub fn diagnose_external_markdown_contents(
    display_path: &str,
    config: &VaultConfig,
    content: &str,
) -> Result<Vec<DoctorDiagnosticIssue>, AppError> {
    let parsed = parse_document(content, config);
    let mut diagnostics = collect_parse_diagnostics(display_path, config, &parsed)?;
    sort_and_dedup_diagnostics(&mut diagnostics);
    Ok(diagnostics)
}

pub fn resolve_periodic_target(
    config: &PeriodicConfig,
    period_type: &str,
    date: Option<&str>,
    require_enabled: bool,
) -> Result<PeriodicTarget, AppError> {
    let note = config
        .note(period_type)
        .ok_or_else(|| AppError::operation(format!("unknown periodic note type: {period_type}")))?;
    if require_enabled && !note.enabled {
        return Err(AppError::operation(format!(
            "periodic note type `{period_type}` is disabled in config"
        )));
    }

    let reference_date = normalize_date_argument(date)?;
    let (start_date, end_date) = period_range_for_date(config, period_type, &reference_date)
        .ok_or_else(|| {
            AppError::operation(format!(
                "failed to resolve period range for `{period_type}` and {reference_date}"
            ))
        })?;
    let path =
        expected_periodic_note_path(config, period_type, &reference_date).ok_or_else(|| {
            AppError::operation(format!(
                "failed to resolve note path for `{period_type}` and {reference_date}"
            ))
        })?;

    Ok(PeriodicTarget {
        period_type: period_type.to_string(),
        reference_date,
        start_date,
        end_date,
        path,
    })
}

pub fn render_periodic_note_contents(
    paths: &VaultPaths,
    period_type: &str,
    relative_path: &str,
    warnings: &mut Vec<String>,
    permission_profile: Option<&str>,
) -> Result<String, AppError> {
    let guard = permission_profile
        .map(|profile| {
            resolve_permission_profile(paths, Some(profile))
                .map(|selection| ProfilePermissionGuard::new(paths, selection))
                .map_err(AppError::operation)
        })
        .transpose()?;
    render_periodic_note_contents_with_guard(
        paths,
        period_type,
        relative_path,
        warnings,
        guard.as_ref(),
        false,
    )
}

pub(crate) fn render_periodic_note_contents_with_guard(
    paths: &VaultPaths,
    period_type: &str,
    relative_path: &str,
    warnings: &mut Vec<String>,
    guard: Option<&ProfilePermissionGuard>,
    dry_run: bool,
) -> Result<String, AppError> {
    let config = load_vault_config(paths).config;
    let template_name = config
        .periodic
        .note(period_type)
        .and_then(|note| note.template.as_deref());
    let Some(template_name) = template_name else {
        return Ok(String::new());
    };

    let loaded = if let Some(guard) = guard {
        match crate::templates::load_named_template_with_guard(paths, &config, template_name, guard)
        {
            Ok(loaded) => loaded,
            Err(error) if error.code() == Some("template_not_found") => {
                // Only an unrestricted, hook-free reader can distinguish true
                // absence from a template hidden by its authority ceiling.
                warnings.push(format!(
                    "failed to resolve periodic template `{template_name}` for `{period_type}`: {error}"
                ));
                return Ok(String::new());
            }
            Err(error) => return Err(error),
        }
    } else {
        match load_named_template(paths, &config, template_name) {
            Ok(loaded) => loaded,
            Err(error) => {
                warnings.push(format!(
                "failed to resolve periodic template `{template_name}` for `{period_type}`: {error}"
            ));
                return Ok(String::new());
            }
        }
    };
    let vars = HashMap::new();
    let read_filter = guard.map(PermissionGuard::read_filter);
    // A note for another day renders `{{date}}` as that day, like Obsidian's
    // Daily Notes plugin.
    let reference_date = match_periodic_note_path(&config.periodic, relative_path)
        .filter(|matched| matched.period_type == period_type)
        .map(|matched| matched.start_date);
    let rendered = render_loaded_template_with_authority(
        paths,
        &config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path: relative_path,
            target_contents: None,
            engine: TemplateEngineKind::Auto,
            vars: &vars,
            allow_mutations: !dry_run,
            run_mode: TemplateRunMode::Create,
            reference_date: reference_date.as_deref(),
        },
        read_filter.as_ref(),
        guard,
    )?;
    warnings.extend(loaded.template.warning);
    warnings.extend(rendered.warnings);
    warnings.extend(rendered.diagnostics);
    Ok(rendered.content)
}

pub(crate) fn normalize_note_path(path: &str) -> Result<String, AppError> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(AppError::operation)
}

pub(crate) fn normalize_date_argument(date: Option<&str>) -> Result<String, AppError> {
    crate::periodic::normalize_date_argument(date)
}

fn merge_note_create_bodies(template_body: &str, stdin_body: &str) -> String {
    match (
        template_body.trim().is_empty(),
        stdin_body.trim().is_empty(),
    ) {
        (true, true) => String::new(),
        (false, true) => template_body.to_string(),
        (true, false) => stdin_body.to_string(),
        (false, false) => {
            let first = template_body.trim_end_matches('\n');
            let second = stdin_body.trim_end_matches('\n');
            format!("{first}\n\n{second}\n")
        }
    }
}

fn merge_explicit_frontmatter(
    existing: Option<YamlMapping>,
    explicit: Option<YamlMapping>,
) -> Option<YamlMapping> {
    match (existing, explicit) {
        (None, None) => None,
        (Some(mapping), None) | (None, Some(mapping)) => Some(mapping),
        (Some(mut existing), Some(explicit)) => {
            for (key, value) in explicit {
                existing.insert(key, value);
            }
            Some(existing)
        }
    }
}

pub(crate) fn resolve_existing_note_path(
    paths: &VaultPaths,
    note: &str,
) -> Result<String, AppError> {
    match resolve_note_reference(paths, note) {
        Ok(resolved) => Ok(resolved.path),
        Err(GraphQueryError::AmbiguousIdentifier { .. }) => Err(AppError::operation(format!(
            "note identifier '{note}' is ambiguous"
        ))),
        Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. }) => {
            let normalized = normalize_note_path(note)?;
            if paths.vault_root().join(&normalized).is_file() {
                Ok(normalized)
            } else {
                Err(AppError::operation(format!("note not found: {note}")))
            }
        }
        Err(error) => Err(AppError::operation(error)),
    }
}

fn preserve_existing_frontmatter(existing: &str, body: &str) -> String {
    find_frontmatter_block(existing).map_or_else(
        || body.to_string(),
        |(_, _, body_start)| {
            let mut rendered = existing[..body_start].to_string();
            rendered.push_str(body);
            rendered
        },
    )
}

fn parse_note_patch_matcher(pattern: &str) -> Result<NotePatchMatcher, AppError> {
    if pattern.is_empty() {
        return Err(AppError::operation("`note patch --find` must not be empty"));
    }

    if let Some(regex_body) = pattern.strip_prefix('/') {
        let Some(regex_body) = regex_body.strip_suffix('/') else {
            return Err(AppError::operation(
                "regex patterns must use /.../ syntax, for example `/TODO \\d+/`",
            ));
        };
        if regex_body.is_empty() {
            return Err(AppError::operation("regex patterns must not be empty"));
        }
        return Regex::new(regex_body)
            .map(NotePatchMatcher::Regex)
            .map_err(AppError::operation);
    }

    Ok(NotePatchMatcher::Literal(pattern.to_string()))
}

fn apply_note_patch_to_source(
    source: &str,
    matcher: &NotePatchMatcher,
    replace: &str,
    all: bool,
    target: &str,
) -> Result<NotePatchApplication, AppError> {
    match matcher {
        NotePatchMatcher::Literal(find) => {
            let patch_matches = source
                .match_indices(find)
                .map(|(start, matched)| {
                    (
                        start,
                        start + matched.len(),
                        matched.to_string(),
                        replace.to_string(),
                    )
                })
                .collect::<Vec<_>>();
            build_note_patch_application(source, patch_matches, all, false, target)
        }
        NotePatchMatcher::Regex(regex) => {
            let patch_matches = regex
                .find_iter(source)
                .map(|matched| {
                    if matched.start() == matched.end() {
                        Err(AppError::operation(
                            "regex patterns for `note patch` must not match empty strings",
                        ))
                    } else {
                        Ok((
                            matched.start(),
                            matched.end(),
                            matched.as_str().to_string(),
                            regex.replace(matched.as_str(), replace).into_owned(),
                        ))
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            build_note_patch_application(source, patch_matches, all, true, target)
        }
    }
}

fn build_note_patch_application(
    source: &str,
    matches: Vec<(usize, usize, String, String)>,
    all: bool,
    regex: bool,
    target: &str,
) -> Result<NotePatchApplication, AppError> {
    match matches.len() {
        0 => Err(AppError::operation(format!(
            "pattern not found in {target}"
        ))),
        count if count > 1 && !all => Err(AppError::operation(format!(
            "pattern matched {count} times in {target}; rerun with --all to replace every match"
        ))),
        _ => {
            let mut updated = source.to_string();
            for (start, end, _, replacement) in matches.iter().rev() {
                updated.replace_range(*start..*end, replacement);
            }
            Ok(NotePatchApplication {
                updated_content: updated,
                match_count: matches.len(),
                changes: matches
                    .into_iter()
                    .map(|(_, _, before, after)| RefactorChange { before, after })
                    .collect(),
                regex,
            })
        }
    }
}

fn resolve_note_patch_scope_selection(
    source: &str,
    config: &VaultConfig,
    section_id: Option<&str>,
    heading: Option<&str>,
    block_ref: Option<&str>,
    lines: Option<&str>,
) -> Result<Option<NotePatchScopeSelection>, AppError> {
    if section_id.is_none() && heading.is_none() && block_ref.is_none() && lines.is_none() {
        return Ok(None);
    }

    let parsed = parse_document(source, config);
    let selection = vulcan_core::read_note(
        source,
        &parsed,
        &vulcan_core::NoteReadOptions {
            heading: heading.map(ToOwned::to_owned),
            section_id: section_id.map(ToOwned::to_owned),
            block_ref: block_ref.map(ToOwned::to_owned),
            lines: lines.map(ToOwned::to_owned),
            match_pattern: None,
            context: 0,
            no_frontmatter: false,
        },
    )
    .map_err(AppError::operation)?;

    let [line_span] = selection.line_spans.as_slice() else {
        return Err(AppError::operation(
            "selected note scope is empty or not contiguous",
        ));
    };
    let Some((byte_start, byte_end)) = vulcan_core::byte_range_for_line_span(source, line_span)
    else {
        return Err(AppError::operation(
            "selected note scope could not be mapped back to source bytes",
        ));
    };

    Ok(Some(NotePatchScopeSelection {
        section_id: selection.section_id,
        line_spans: selection.line_spans,
        byte_start,
        byte_end,
    }))
}

fn collect_parse_diagnostics(
    display_path: &str,
    config: &VaultConfig,
    parsed: &ParsedDocument,
) -> Result<Vec<DoctorDiagnosticIssue>, AppError> {
    let mut diagnostics = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| DoctorDiagnosticIssue {
            document_path: Some(display_path.to_string()),
            message: diagnostic.message.clone(),
            byte_range: diagnostic.byte_range.as_ref().map(|range| DoctorByteRange {
                start: range.start,
                end: range.end,
            }),
        })
        .collect::<Vec<_>>();

    if let Some(indexed) =
        extract_indexed_properties(parsed, config).map_err(AppError::operation)?
    {
        diagnostics.extend(indexed.diagnostics.into_iter().map(|diagnostic| {
            DoctorDiagnosticIssue {
                document_path: Some(display_path.to_string()),
                message: diagnostic.message,
                byte_range: None,
            }
        }));
    }

    diagnostics.extend(dataview_parse_diagnostics(display_path, parsed));
    Ok(diagnostics)
}

fn dataview_parse_diagnostics(
    display_path: &str,
    parsed: &ParsedDocument,
) -> Vec<DoctorDiagnosticIssue> {
    parsed
        .dataview_blocks
        .iter()
        .filter(|block| block.language == "dataview")
        .filter_map(|block| {
            let output = parse_dql_with_diagnostics(&block.text);
            output
                .diagnostics
                .first()
                .map(|diagnostic| DoctorDiagnosticIssue {
                    document_path: Some(display_path.to_string()),
                    message: format!(
                        "Dataview block {} at line {} failed to parse: {}",
                        block.block_index, block.line_number, diagnostic.message
                    ),
                    byte_range: Some(DoctorByteRange {
                        start: block.byte_range.start,
                        end: block.byte_range.end,
                    }),
                })
        })
        .collect()
}

fn link_diagnostic(
    relative_path: &str,
    byte_offset: usize,
    raw_text: &str,
    message: String,
) -> DoctorDiagnosticIssue {
    DoctorDiagnosticIssue {
        document_path: Some(relative_path.to_string()),
        message,
        byte_range: Some(DoctorByteRange {
            start: byte_offset,
            end: byte_offset + raw_text.len(),
        }),
    }
}

fn link_resolution_diagnostics(
    paths: &VaultPaths,
    relative_path: &str,
    config: &VaultConfig,
    parsed: &ParsedDocument,
) -> Result<Vec<DoctorDiagnosticIssue>, AppError> {
    let resolver_documents = build_resolver_documents(paths, relative_path, parsed, config)?;
    let mut target_documents = HashMap::new();
    let mut diagnostics = Vec::new();

    for link in &parsed.links {
        let resolution = resolve_link(
            &resolver_documents,
            &ResolverLink {
                source_document_id: relative_path.to_string(),
                source_path: relative_path.to_string(),
                target_path_candidate: link.target_path_candidate.clone(),
                link_kind: link.link_kind,
            },
            config.link_resolution,
        );
        match resolution.problem {
            Some(
                problem @ (LinkResolutionProblem::Unresolved | LinkResolutionProblem::OutsideVault),
            ) => diagnostics.push(link_diagnostic(
                relative_path,
                link.byte_offset,
                &link.raw_text,
                if problem == LinkResolutionProblem::OutsideVault {
                    format!("Link target `{}` leaves the vault root", link.raw_text)
                } else {
                    format!("Unresolved link target `{}`", link.raw_text)
                },
            )),
            Some(LinkResolutionProblem::Ambiguous(matches)) => {
                diagnostics.push(link_diagnostic(
                    relative_path,
                    link.byte_offset,
                    &link.raw_text,
                    format!(
                        "Ambiguous link target `{}` matched {}",
                        link.raw_text,
                        matches.join(", ")
                    ),
                ));
            }
            None => {
                let Some(target_path) = resolution.resolved_target_id else {
                    continue;
                };
                if let Some(target_heading) = link.target_heading.as_deref() {
                    let target = load_target_document(
                        paths,
                        relative_path,
                        parsed,
                        config,
                        &target_path,
                        &mut target_documents,
                    )?;
                    if !target
                        .headings
                        .iter()
                        .any(|heading| heading.text == target_heading)
                    {
                        diagnostics.push(link_diagnostic(relative_path, link.byte_offset, &link.raw_text, format!(
                                "Broken heading link `{}`: heading `{target_heading}` was not found in {target_path}",
                                link.raw_text
                            )));
                    }
                }
                if let Some(target_block) = link.target_block.as_deref() {
                    let target = load_target_document(
                        paths,
                        relative_path,
                        parsed,
                        config,
                        &target_path,
                        &mut target_documents,
                    )?;
                    if !target
                        .block_refs
                        .iter()
                        .any(|block_ref| block_ref.block_id_text == target_block)
                    {
                        diagnostics.push(link_diagnostic(relative_path, link.byte_offset, &link.raw_text, format!(
                                "Broken block link `{}`: block `^{target_block}` was not found in {target_path}",
                                link.raw_text
                            )));
                    }
                }
            }
        }
    }

    Ok(diagnostics)
}

fn build_resolver_documents(
    paths: &VaultPaths,
    relative_path: &str,
    parsed: &ParsedDocument,
    config: &VaultConfig,
) -> Result<Vec<ResolverDocument>, AppError> {
    if let Ok(note_index) = load_note_index(paths) {
        let mut documents = note_index
            .into_values()
            .map(|note| ResolverDocument {
                id: note.document_path.clone(),
                path: note.document_path,
                filename: note.file_name,
                aliases: note.aliases,
            })
            .collect::<Vec<_>>();
        if let Some(existing) = documents
            .iter_mut()
            .find(|document| document.path == relative_path)
        {
            existing.aliases.clone_from(&parsed.aliases);
        } else {
            documents.push(resolver_document_from_parsed(relative_path, parsed));
        }
        return Ok(documents);
    }

    let mut documents = Vec::new();
    for path in discover_markdown_note_paths(paths.vault_root()).map_err(AppError::operation)? {
        if path == relative_path {
            documents.push(resolver_document_from_parsed(relative_path, parsed));
            continue;
        }
        let source =
            fs::read_to_string(paths.vault_root().join(&path)).map_err(AppError::operation)?;
        let parsed_document = parse_document(&source, config);
        documents.push(resolver_document_from_parsed(&path, &parsed_document));
    }

    if !documents
        .iter()
        .any(|document| document.path == relative_path)
    {
        documents.push(resolver_document_from_parsed(relative_path, parsed));
    }
    Ok(documents)
}

fn resolver_document_from_parsed(relative_path: &str, parsed: &ParsedDocument) -> ResolverDocument {
    let filename = Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relative_path)
        .to_string();
    ResolverDocument {
        id: relative_path.to_string(),
        path: relative_path.to_string(),
        filename,
        aliases: parsed.aliases.clone(),
    }
}

fn load_target_document<'a>(
    paths: &VaultPaths,
    current_path: &str,
    current_parsed: &ParsedDocument,
    config: &VaultConfig,
    target_path: &str,
    cache: &'a mut HashMap<String, ParsedDocument>,
) -> Result<&'a ParsedDocument, AppError> {
    if target_path == current_path {
        cache
            .entry(target_path.to_string())
            .or_insert_with(|| current_parsed.clone());
    } else if !cache.contains_key(target_path) {
        let source = fs::read_to_string(paths.vault_root().join(target_path))
            .map_err(AppError::operation)?;
        cache.insert(target_path.to_string(), parse_document(&source, config));
    }

    cache
        .get(target_path)
        .ok_or_else(|| AppError::operation(format!("failed to load target note {target_path}")))
}

fn discover_markdown_note_paths(root: &Path) -> io::Result<Vec<String>> {
    fn walk(root: &Path, current: &Path, paths: &mut Vec<String>) -> io::Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            if file_name.to_string_lossy() == ".vulcan" {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, paths)?;
            } else if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                let relative = path
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                paths.push(relative);
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut paths)?;
    }
    paths.sort();
    Ok(paths)
}

fn sort_and_dedup_diagnostics(diagnostics: &mut Vec<DoctorDiagnosticIssue>) {
    diagnostics.sort_by(|left, right| {
        left.document_path
            .cmp(&right.document_path)
            .then(left.message.cmp(&right.message))
            .then_with(|| match (&left.byte_range, &right.byte_range) {
                (Some(left), Some(right)) => {
                    left.start.cmp(&right.start).then(left.end.cmp(&right.end))
                }
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
    });
    diagnostics.dedup();
}

fn append_entry_at_end(contents: &str, entry: &str) -> String {
    let mut prefix = contents.trim_end_matches('\n').to_string();
    if !prefix.is_empty() {
        prefix.push_str("\n\n");
    }
    let mut updated = prefix;
    updated.push_str(entry.trim_end());
    updated.push('\n');
    updated
}

fn append_entry_under_heading(contents: &str, heading: &str, entry: &str) -> String {
    let heading = heading.trim();
    if heading.is_empty() {
        return append_entry_at_end(contents, entry);
    }

    let heading_level = markdown_heading_level(heading);
    let mut offset = 0usize;
    let mut insert_at = None;
    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if insert_at.is_none() && trimmed == heading {
            insert_at = Some(offset + line.len());
        } else if insert_at.is_some()
            && markdown_heading_level(trimmed).is_some_and(|level| Some(level) <= heading_level)
        {
            insert_at = Some(offset);
            break;
        }
        offset += line.len();
    }

    if let Some(insert_at) = insert_at {
        let mut prefix = String::new();
        prefix.push_str(&contents[..insert_at]);
        if !prefix.ends_with('\n') {
            prefix.push('\n');
        }
        if !prefix.ends_with("\n\n") {
            prefix.push('\n');
        }
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        if insert_at < contents.len() && !contents[insert_at..].starts_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&contents[insert_at..]);
        updated
    } else {
        let mut prefix = contents.trim_end_matches('\n').to_string();
        if !prefix.is_empty() {
            prefix.push_str("\n\n");
        }
        prefix.push_str(heading);
        prefix.push_str("\n\n");
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        updated
    }
}

fn prepend_entry_after_frontmatter(contents: &str, entry: &str) -> String {
    let body_start = find_frontmatter_block(contents).map_or(0, |(_, _, start)| start);
    let prefix = &contents[..body_start];
    let body = contents[body_start..].trim_start_matches('\n');
    let mut updated = prefix.to_string();
    updated.push_str(entry.trim_end());
    updated.push('\n');
    if !body.is_empty() {
        updated.push('\n');
        updated.push_str(body.trim_end_matches('\n'));
        updated.push('\n');
    }
    updated
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    (hashes > 0 && hashes <= 6 && line.chars().nth(hashes).is_some_and(char::is_whitespace))
        .then_some(hashes)
}

fn dispatch_note_write_plugin_hooks(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    relative_path: &str,
    operation: &str,
    existing: Option<&str>,
    updated: &str,
    verbosity: Verbosity,
) -> Result<(), AppError> {
    plugins::dispatch_plugin_event(
        paths,
        permission_profile,
        PluginEvent::OnNoteWrite,
        &json!({
            "kind": PluginEvent::OnNoteWrite,
            "path": relative_path,
            "operation": operation,
            "existed_before": existing.is_some(),
            "previous_content": existing,
            "content": updated,
        }),
        verbosity,
    )
}

fn dispatch_note_create_plugin_hooks(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    relative_path: &str,
    content: &str,
    verbosity: Verbosity,
) {
    let _ = plugins::dispatch_plugin_event(
        paths,
        permission_profile,
        PluginEvent::OnNoteCreate,
        &json!({
            "kind": PluginEvent::OnNoteCreate,
            "path": relative_path,
            "content": content,
        }),
        verbosity,
    );
}

fn dispatch_note_delete_plugin_hooks(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    relative_path: &str,
    verbosity: Verbosity,
) {
    let _ = plugins::dispatch_plugin_event(
        paths,
        permission_profile,
        PluginEvent::OnNoteDelete,
        &json!({
            "kind": PluginEvent::OnNoteDelete,
            "path": relative_path,
        }),
        verbosity,
    );
}

struct LoadedAppendTarget {
    path: String,
    existing: String,
    created: bool,
    period_type: Option<String>,
    reference_date: Option<String>,
    warnings: Vec<String>,
}

fn load_note_append_target(
    paths: &VaultPaths,
    config: &vulcan_core::VaultConfig,
    request: &NoteAppendRequest,
    permission_profile: Option<&str>,
) -> Result<LoadedAppendTarget, AppError> {
    if let Some(period_type) = request.periodic.as_deref() {
        let target =
            resolve_periodic_target(&config.periodic, period_type, request.date.as_deref(), true)?;
        let absolute_path = paths.vault_root().join(&target.path);
        let mut warnings = Vec::new();
        let (existing, created) = if absolute_path.is_file() {
            (
                fs::read_to_string(&absolute_path).map_err(AppError::operation)?,
                false,
            )
        } else if absolute_path.exists() {
            return Err(AppError::operation(format!(
                "path exists but is not a note file: {}",
                target.path
            )));
        } else {
            (
                render_periodic_note_contents(
                    paths,
                    period_type,
                    &target.path,
                    &mut warnings,
                    permission_profile,
                )?,
                true,
            )
        };

        return Ok(LoadedAppendTarget {
            path: target.path,
            existing,
            created,
            period_type: Some(target.period_type),
            reference_date: Some(target.reference_date),
            warnings,
        });
    }

    let note = request
        .note
        .as_deref()
        .ok_or_else(|| AppError::operation("`note append` requires a note or periodic target"))?;
    let path = resolve_existing_note_path(paths, note)?;
    let existing =
        fs::read_to_string(paths.vault_root().join(&path)).map_err(AppError::operation)?;
    Ok(LoadedAppendTarget {
        path,
        existing,
        created: false,
        period_type: None,
        reference_date: None,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        apply_note_append, apply_note_create, apply_note_delete, apply_note_patch, apply_note_set,
        build_note_info_report, check_read_markdown_source_access,
        check_write_markdown_source_access, diagnose_note_contents, finish_note_append_report,
        finish_note_create_report, finish_note_patch_report, finish_note_set_report,
        json_properties_to_frontmatter, parse_note_frontmatter_bindings, read_note,
        read_note_outline, resolve_existing_markdown_target, MarkdownTarget, NoteAppendMode,
        NoteAppendRequest, NoteCreateRequest, NoteDeleteRequest, NoteGetOptions, NotePatchRequest,
        NoteReadMode, NoteSetRequest,
    };
    use crate::templates::{YamlMapping, YamlValue};
    use serde::Serialize;
    use serde_json::Value as JsonValue;
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;
    use vulcan_core::Verbosity;
    use vulcan_core::{
        initialize_vulcan_dir, resolve_permission_profile, scan_vault_with_progress,
        ProfilePermissionGuard, ScanMode, VaultPaths,
    };

    #[test]
    fn note_writes_are_atomic_checked_and_never_overwrite_unreadable_notes() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        assert_eq!(
            super::read_note_for_update(&paths, "New.md").expect("missing"),
            None
        );
        super::write_note_content(
            &paths,
            "New.md",
            None,
            "first\n",
            "create",
            None,
            Verbosity::Quiet,
        )
        .expect("create");
        assert!(
            super::write_note_content(
                &paths,
                "New.md",
                None,
                "again\n",
                "create",
                None,
                Verbosity::Quiet
            )
            .is_err(),
            "create must not replace an existing note"
        );
        assert!(
            super::write_note_content(
                &paths,
                "New.md",
                Some("stale\n"),
                "lost update\n",
                "append",
                None,
                Verbosity::Quiet
            )
            .is_err(),
            "a note that changed since it was read is not overwritten"
        );
        let written = super::write_note_content(
            &paths,
            "New.md",
            Some("first\n"),
            "second\n",
            "append",
            None,
            Verbosity::Quiet,
        )
        .expect("update");
        assert_eq!(written, "second\n");
        assert_eq!(
            super::read_note_for_update(&paths, "New.md").expect("read"),
            Some("second\n".to_string())
        );

        fs::write(temp_dir.path().join("Binary.md"), b"\xff\xfe").expect("binary note");
        assert!(super::read_note_for_update(&paths, "Binary.md").is_err());
    }

    #[test]
    fn note_info_report_preserves_metadata_and_word_count() {
        let temporary = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::write(
            temporary.path().join("Home.md"),
            "---\ntitle: Home\n---\n# Home\n- One task\n^block-ref\n[[Other]]\n",
        )
        .expect("home note");
        fs::write(temporary.path().join("Other.md"), "[[Home]]\n").expect("other note");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

        let report = build_note_info_report(&paths, "Home.md", None).expect("info");
        assert_eq!(report.path, "Home.md");
        assert_eq!(report.heading_count, 1);
        assert_eq!(report.outgoing_link_count, 1);
        assert_eq!(report.backlink_count, 1);
        assert_eq!(report.frontmatter_keys, vec!["title"]);
        assert_eq!(report.word_count, 4);
        assert!(report.modified_at_ms.is_some());
    }

    #[test]
    fn existing_markdown_target_distinguishes_vault_and_external_files() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join("Projects")).unwrap();
        fs::write(vault.join("Projects/Alpha.md"), "# Alpha\n").unwrap();
        let external = temporary.path().join("External.md");
        fs::write(&external, "# External\n").unwrap();
        let paths = VaultPaths::new(&vault);

        let internal = resolve_existing_markdown_target(&paths, "Projects/Alpha.md").unwrap();
        assert_eq!(internal.display_path, "Projects/Alpha.md");
        assert_eq!(
            internal.vault_relative_path.as_deref(),
            Some("Projects/Alpha.md")
        );
        assert_eq!(internal.read_source().unwrap(), "# Alpha\n");

        let external_target =
            resolve_existing_markdown_target(&paths, external.to_str().unwrap()).unwrap();
        assert_eq!(external_target.display_path, external.display().to_string());
        assert!(external_target.vault_relative_path.is_none());
        assert_eq!(external_target.read_source().unwrap(), "# External\n");
        assert!(resolve_existing_markdown_target(&paths, "Missing.md").is_err());
        let non_markdown = temporary.path().join("Other.txt");
        fs::write(&non_markdown, "not markdown").unwrap();
        assert!(resolve_existing_markdown_target(&paths, non_markdown.to_str().unwrap()).is_err());
    }

    #[test]
    fn markdown_source_guards_allow_unrestricted_external_but_reject_scoped_external() {
        let temporary = tempdir().expect("temporary vault");
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join(".vulcan")).expect("config directory");
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .expect("permission config");
        fs::write(vault.join("Home.md"), "# Home\n").expect("vault note");
        let external = temporary.path().join("External.md");
        fs::write(&external, "# External\n").expect("external note");
        let paths = VaultPaths::new(&vault);
        let unrestricted = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).expect("readonly profile"),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).expect("blind profile"),
        );
        let write_unrestricted = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).expect("unrestricted profile"),
        );
        let external_path = external.to_str().expect("utf-8 path");
        assert!(check_read_markdown_source_access(&paths, &unrestricted, external_path).is_ok());
        assert!(check_read_markdown_source_access(&paths, &blind, "Home.md").is_err());
        assert!(
            check_read_markdown_source_access(&paths, &blind, external_path)
                .expect_err("scoped external read")
                .to_string()
                .contains("outside the selected vault root")
        );
        assert!(
            check_write_markdown_source_access(&paths, &write_unrestricted, external_path).is_ok()
        );
        assert!(check_write_markdown_source_access(&paths, &unrestricted, "Home.md").is_err());
        assert!(
            check_write_markdown_source_access(&paths, &unrestricted, external_path)
                .expect_err("scoped external write")
                .to_string()
                .contains("outside the selected vault root")
        );
    }

    #[test]
    fn shared_note_outline_preserves_scope_depth_and_external_path() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        let external = temporary.path().join("Outline.md");
        fs::write(&external, "# Root\n## Child\n### Grandchild\n").unwrap();
        let paths = VaultPaths::new(&vault);
        let report =
            read_note_outline(&paths, external.to_str().unwrap(), Some("root@1"), Some(1)).unwrap();
        assert_eq!(report.path, external.display().to_string());
        assert_eq!(report.scope_section.unwrap().id, "root@1");
        assert_eq!(report.sections.len(), 1);
        assert_eq!(report.sections[0].id, "root/child@2");
        assert_eq!(report.depth_limit, Some(1));
        assert!(read_note_outline(&paths, external.to_str().unwrap(), None, Some(0)).is_err());
    }

    #[test]
    fn shared_note_get_preserves_selection_metadata_and_html_shape() {
        let temporary = tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        fs::write(
            vault.join("Read.md"),
            "---\ntitle: Read\n---\n# First\nText\n## Second\nMore\n",
        )
        .unwrap();
        let paths = VaultPaths::new(&vault);
        let markdown = read_note(
            &paths,
            NoteGetOptions {
                note: "Read.md",
                mode: NoteReadMode::Markdown,
                section_id: Some("first/second@6"),
                heading: None,
                block_ref: None,
                lines: None,
                match_pattern: None,
                context: 0,
                no_frontmatter: false,
                raw: false,
            },
        )
        .unwrap();
        assert_eq!(markdown.path, "Read.md");
        assert_eq!(markdown.metadata.mode, "markdown");
        assert_eq!(
            markdown.metadata.section_id.as_deref(),
            Some("first/second@6")
        );
        assert_eq!(markdown.metadata.total_lines, 7);
        assert_eq!(markdown.content, "## Second\nMore\n");
        let html = read_note(
            &paths,
            NoteGetOptions {
                note: "Read.md",
                mode: NoteReadMode::Html,
                section_id: None,
                heading: None,
                block_ref: None,
                lines: None,
                match_pattern: None,
                context: 0,
                no_frontmatter: false,
                raw: false,
            },
        )
        .unwrap();
        assert_eq!(html.metadata.mode, "html");
        assert!(html.content.contains("<h1 id=\"first\">First</h1>"));
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One pending journal is exercised across reads, writes, and recovery.
    fn direct_note_reads_and_writes_refuse_pending_ordinary_write_journal() {
        #[derive(Serialize)]
        struct JournalFixture<'a> {
            version: u32,
            transaction_id: &'a str,
            changes: Vec<vulcan_core::ordinary_write::OrdinaryWriteChange>,
            digest: String,
        }

        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        fs::write(temporary.path().join("Read.md"), "# Read\nOriginal\n").expect("note");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan note");
        let directory = paths
            .operational_state_dir()
            .expect("operational state")
            .join("ordinary-write");
        fs::create_dir_all(&directory).expect("journal directory");
        let mut journal = JournalFixture {
            version: 1,
            transaction_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            changes: vec![vulcan_core::ordinary_write::OrdinaryWriteChange {
                path: "Read.md".to_string(),
                before: Some("# Read\nOriginal\n".to_string()),
                after: Some("# Read\nUpdated\n".to_string()),
            }],
            digest: String::new(),
        };
        journal.digest = blake3::hash(&serde_json::to_vec(&journal).expect("journal bytes"))
            .to_hex()
            .to_string();
        let journal_path = directory.join("journal.json");
        fs::write(
            &journal_path,
            serde_json::to_vec(&journal).expect("sealed journal"),
        )
        .expect("pending journal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&journal_path, fs::Permissions::from_mode(0o600))
                .expect("owner-only journal");
        }

        let get = || {
            read_note(
                &paths,
                NoteGetOptions {
                    note: "Read.md",
                    mode: NoteReadMode::Markdown,
                    section_id: None,
                    heading: None,
                    block_ref: None,
                    lines: None,
                    match_pattern: None,
                    context: 0,
                    no_frontmatter: false,
                    raw: false,
                },
            )
        };
        assert_eq!(
            get().expect_err("get must fail closed").code(),
            Some("ordinary_write_pending")
        );
        assert_eq!(
            read_note_outline(&paths, "Read.md", None, None)
                .expect_err("outline must fail closed")
                .code(),
            Some("ordinary_write_pending")
        );
        assert_eq!(
            build_note_info_report(&paths, "Read.md", None)
                .expect_err("info must fail closed")
                .code(),
            Some("ordinary_write_pending")
        );
        assert_eq!(
            super::write_ordinary_note_if_unchanged(
                &paths,
                "Other.md",
                None,
                "unrelated\n",
                "create",
            )
            .expect_err("create must fail closed")
            .code(),
            Some("ordinary_write_pending")
        );
        assert!(!temporary.path().join("Other.md").exists());
        assert_eq!(
            super::write_ordinary_note_if_unchanged(
                &paths,
                "Read.md",
                Some("# Read\nOriginal\n"),
                "replacement\n",
                "set",
            )
            .expect_err("replacement must fail closed")
            .code(),
            Some("ordinary_write_pending")
        );
        assert_eq!(
            super::delete_ordinary_note_if_unchanged(&paths, "Read.md", "# Read\nOriginal\n", None)
                .expect_err("delete must fail closed")
                .code(),
            Some("ordinary_write_pending")
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("Read.md")).expect("untouched note"),
            "# Read\nOriginal\n"
        );
        vulcan_core::ordinary_write::recover_ordinary_write_batch(&paths)
            .expect("recover pending batch")
            .expect("pending batch");
        assert_eq!(
            get().expect("get after recovery").content,
            "# Read\nUpdated\n"
        );
        assert_eq!(
            read_note_outline(&paths, "Read.md", None, None)
                .expect("outline after recovery")
                .sections
                .len(),
            1
        );
        assert_eq!(
            build_note_info_report(&paths, "Read.md", None)
                .expect("info after recovery")
                .heading_count,
            1
        );
        super::write_ordinary_note_if_unchanged(&paths, "Other.md", None, "created\n", "create")
            .expect("create after recovery");
    }

    #[test]
    fn parse_note_frontmatter_bindings_parses_yaml_scalars_and_lists() {
        let bindings = vec!["status=done".to_string(), "tags=[alpha, beta]".to_string()];

        let parsed = parse_note_frontmatter_bindings(&bindings)
            .expect("bindings should parse")
            .expect("bindings should produce frontmatter");

        assert_eq!(parsed["status"], YamlValue::String("done".to_string()));
        assert_eq!(
            parsed["tags"],
            serde_yaml::from_str::<YamlValue>(
                "- alpha
- beta
"
            )
            .expect("tag yaml")
        );
    }

    #[test]
    fn json_properties_to_frontmatter_converts_json_values() {
        let properties = BTreeMap::from([
            ("done".to_string(), JsonValue::Bool(true)),
            ("owners".to_string(), serde_json::json!(["alice", "bob"])),
        ]);

        let frontmatter = json_properties_to_frontmatter(&properties)
            .expect("properties should convert")
            .expect("properties should produce frontmatter");

        assert_eq!(frontmatter["done"], YamlValue::Bool(true));
        assert_eq!(
            frontmatter["owners"],
            serde_yaml::from_str::<YamlValue>(
                "- alice
- bob
"
            )
            .expect("owners yaml")
        );
    }

    #[test]
    fn apply_note_create_renders_template_and_writes_note() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/templates/brief.md"),
            "---\nstatus: draft\n---\n# {{title}}\n\nTemplate body\n",
        )
        .expect("template");

        let mut frontmatter = YamlMapping::new();
        frontmatter.insert(
            YamlValue::String("reviewed".to_string()),
            YamlValue::Bool(true),
        );

        let report = apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Inbox/Idea".to_string(),
                template: Some("brief".to_string()),
                frontmatter: Some(frontmatter),
                body: "Extra details\n".to_string(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("create report");

        assert_eq!(report.path, "Inbox/Idea.md");
        assert_eq!(report.template.as_deref(), Some("brief"));
        assert_eq!(report.engine.as_deref(), Some("native"));
        assert_eq!(report.changed_paths, vec!["Inbox/Idea.md".to_string()]);

        let rendered = fs::read_to_string(root.join("Inbox/Idea.md"))
            .expect("created note")
            .replace("\r\n", "\n");
        assert!(rendered.contains("status: draft"));
        assert!(rendered.contains("reviewed: true"));
        assert!(rendered.contains("# Idea"));
        assert!(rendered.contains("Template body\n\nExtra details\n"));
    }

    #[test]
    fn native_template_create_new_is_committed_with_final_note() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/templates/native-side.md"),
            "<% tp.file.create_new('Side body', 'Side') %>Main body",
        )
        .expect("template");

        let report = apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Main".to_string(),
                template: Some("native-side".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("native template create");

        assert_eq!(report.changed_paths, vec!["Main.md", "Side.md"]);
        assert_eq!(
            fs::read_to_string(root.join("Side.md")).expect("side note"),
            "Side body"
        );
        assert!(root.join("Main.md").is_file());
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn scoped_note_create_rejects_template_side_effect_outside_the_grant() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/config.toml"),
            "[permissions.profiles.agent]\nread = \"all\"\nwrite = { allow = [\"folder:Allowed/**\"] }\n",
        )
        .expect("config");
        fs::write(
            root.join(".vulcan/templates/side.md"),
            "<%* await tp.file.create_new('side', 'Denied/Leak'); %>Main body",
        )
        .expect("template");

        let error = apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Allowed/Main".to_string(),
                template: Some("side".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            Some("agent"),
            Verbosity::Quiet,
        )
        .expect_err("template side effect must be denied");
        assert!(!error.to_string().is_empty());
        assert!(!root.join("Denied/Leak.md").exists());
        assert!(!root.join("Allowed/Main.md").exists());

        fs::write(
            root.join(".vulcan/templates/side.md"),
            "<%* await tp.file.create_new('side', 'Allowed/Child'); %>Main body",
        )
        .expect("allowed template");
        apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Allowed/Main".to_string(),
                template: Some("side".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            Some("agent"),
            Verbosity::Quiet,
        )
        .expect("allowed side effect");
        assert!(root.join("Allowed/Child.md").exists());
        assert!(root.join("Allowed/Main.md").exists());
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn note_create_does_not_publish_template_side_effect_when_final_path_collides() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/templates/side.md"),
            "<%* await tp.file.create_new('Side body', 'Side'); %>Main body",
        )
        .expect("template");
        fs::write(root.join("Main.md"), "Existing main\n").expect("existing target");

        apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Main".to_string(),
                template: Some("side".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect_err("final-path collision must fail the whole template create");

        assert_eq!(
            fs::read_to_string(root.join("Main.md")).expect("original main"),
            "Existing main\n"
        );
        assert!(!root.join("Side.md").exists());
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn note_create_publishes_staged_template_side_effect_with_final_note() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/templates/side.md"),
            "<%* await tp.file.create_new('Side body', 'Side'); tR += tp.file.exists('Side') ? 'found:' : 'missing:'; tR += tp.file.include('Side'); %>",
        )
        .expect("template");

        let report = apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Main".to_string(),
                template: Some("side".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("journaled template create");

        assert_eq!(report.changed_paths, vec!["Main.md", "Side.md"]);
        assert_eq!(
            fs::read_to_string(root.join("Side.md")).expect("side effect"),
            "Side body"
        );
        assert_eq!(
            fs::read_to_string(root.join("Main.md")).expect("main note"),
            "found:Side body"
        );
        assert!(
            vulcan_core::ordinary_write::inspect_ordinary_write_batch(&VaultPaths::new(root))
                .expect("journal inspection")
                .is_none()
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn scoped_note_create_rejects_template_target_move_outside_the_grant() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/config.toml"),
            "[permissions.profiles.agent]\nread = \"all\"\nwrite = { allow = [\"folder:Allowed/**\"] }\n",
        )
        .expect("config");
        fs::write(
            root.join(".vulcan/templates/move.md"),
            "<%* await tp.file.move('Denied/Moved'); %>Main body",
        )
        .expect("template");

        apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Allowed/Main".to_string(),
                template: Some("move".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            Some("agent"),
            Verbosity::Quiet,
        )
        .expect_err("template target move must be denied");
        assert!(!root.join("Denied/Moved.md").exists());
        assert!(!root.join("Allowed/Main.md").exists());
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn scoped_note_create_rejects_creation_trigger_side_effect_outside_the_grant() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/config.toml"),
            "[permissions.profiles.agent]\nread = \"all\"\nwrite = { allow = [\"folder:Allowed/**\"] }\n[templates]\ntrigger_on_file_creation = true\ntrigger_on_file_creation_mode = \"folder\"\nfolder_templates = [{ folder = \"Allowed\", template = \"side\" }]\n",
        )
        .expect("config");
        fs::write(
            root.join(".vulcan/templates/side.md"),
            "<%* await tp.file.create_new('side', 'Denied/Leak'); %>Main body",
        )
        .expect("template");

        apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Allowed/Main".to_string(),
                template: None,
                frontmatter: None,
                body: String::new(),
            },
            Some("agent"),
            Verbosity::Quiet,
        )
        .expect_err("creation trigger side effect must be denied");
        assert!(!root.join("Denied/Leak.md").exists());
        assert!(!root.join("Allowed/Main.md").exists());
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn creation_trigger_stages_side_effect_until_final_note_can_publish() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/config.toml"),
            "[templates]\ntrigger_on_file_creation = true\ntrigger_on_file_creation_mode = \"folder\"\nfolder_templates = [{ folder = \"Allowed\", template = \"side\" }]\n",
        )
        .expect("config");
        fs::write(
            root.join(".vulcan/templates/side.md"),
            "<%* await tp.file.create_new('Side body', 'Child', false, 'Allowed'); %>Main body",
        )
        .expect("template");
        fs::create_dir_all(root.join("Allowed")).expect("target dir");
        fs::write(root.join("Allowed/Main.md"), "Existing\n").expect("existing target");
        let request = NoteCreateRequest {
            path: "Allowed/Main".to_string(),
            template: None,
            frontmatter: None,
            body: String::new(),
        };

        apply_note_create(&VaultPaths::new(root), &request, None, Verbosity::Quiet)
            .expect_err("target collision must leave side effect unpublished");
        assert!(!root.join("Allowed/Child.md").exists());
        assert_eq!(
            fs::read_to_string(root.join("Allowed/Main.md")).expect("original target"),
            "Existing\n"
        );

        fs::remove_file(root.join("Allowed/Main.md")).expect("remove test collision");
        let report = apply_note_create(&VaultPaths::new(root), &request, None, Verbosity::Quiet)
            .expect("triggered create");
        assert_eq!(
            report.changed_paths,
            vec!["Allowed/Child.md", "Allowed/Main.md"]
        );
        assert_eq!(
            fs::read_to_string(root.join("Allowed/Child.md")).expect("side note"),
            "Side body"
        );
        assert_eq!(
            fs::read_to_string(root.join("Allowed/Main.md")).expect("main note"),
            "Main body"
        );
    }

    #[test]
    fn apply_note_create_uses_inherited_folder_creation_template() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan/templates")).expect("template dir");
        fs::write(
            root.join(".vulcan/config.toml"),
            r#"[templates]
trigger_on_file_creation = true
trigger_on_file_creation_mode = "folder"
folder_templates = [{ folder = "Projects", template = "project" }]
"#,
        )
        .expect("config");
        fs::write(
            root.join(".vulcan/templates/project.md"),
            "---\nstatus: active\n---\n# {{title}}\n",
        )
        .expect("template");
        fs::write(
            root.join(".vulcan/templates/manual.md"),
            "# Manual {{title}}\n",
        )
        .expect("manual template");

        let report = apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Projects/Active/Idea".to_string(),
                template: None,
                frontmatter: None,
                body: String::new(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("create report");

        assert_eq!(report.template.as_deref(), Some("project"));
        assert_eq!(report.engine.as_deref(), Some("native"));
        assert_eq!(
            fs::read_to_string(root.join("Projects/Active/Idea.md")).expect("created note"),
            "---\nstatus: active\n---\n# Idea\n"
        );

        let explicit = apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Projects/Active/Explicit".to_string(),
                template: Some("manual".to_string()),
                frontmatter: None,
                body: String::new(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("explicit create report");
        assert_eq!(explicit.template.as_deref(), Some("manual"));
        assert_eq!(
            fs::read_to_string(root.join("Projects/Active/Explicit.md")).expect("explicit note"),
            "# Manual Explicit\n"
        );
    }

    #[test]
    fn apply_note_create_renders_inline_commands_when_creation_trigger_is_enabled() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        fs::create_dir_all(root.join(".vulcan")).expect("config dir");
        fs::write(
            root.join(".vulcan/config.toml"),
            "[templates]\ntrigger_on_file_creation = true\ntrigger_on_file_creation_mode = \"none\"\n",
        )
        .expect("config");

        let report = apply_note_create(
            &VaultPaths::new(root),
            &NoteCreateRequest {
                path: "Inbox/Idea".to_string(),
                template: None,
                frontmatter: None,
                body: "# {{title}}\n".to_string(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("create report");

        assert_eq!(report.template, None);
        assert_eq!(report.engine.as_deref(), Some("native"));
        assert_eq!(
            fs::read_to_string(root.join("Inbox/Idea.md")).expect("created note"),
            "# Idea\n"
        );
    }

    #[test]
    fn apply_note_append_creates_missing_periodic_note_and_renders_vars() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        initialize_vulcan_dir(&paths).expect("init should succeed");

        let report = apply_note_append(
            &paths,
            &NoteAppendRequest {
                note: None,
                text: "- {{VALUE:title|case:slug}} due {{VDATE:due,YYYY-MM-DD}}".to_string(),
                mode: NoteAppendMode::Append,
                heading: None,
                periodic: Some("daily".to_string()),
                date: Some("2026-04-03".to_string()),
                vars: HashMap::from([
                    ("title".to_string(), "Release Planning".to_string()),
                    ("due".to_string(), "2026-04-05".to_string()),
                ]),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("append report");

        assert_eq!(report.path, "Journal/Daily/2026-04-03.md");
        assert_eq!(report.mode, "append");
        assert!(report.created);
        assert_eq!(report.period_type.as_deref(), Some("daily"));
        assert_eq!(report.reference_date.as_deref(), Some("2026-04-03"));

        let rendered = fs::read_to_string(root.join("Journal/Daily/2026-04-03.md"))
            .expect("daily note")
            .replace("\r\n", "\n");
        assert!(rendered.contains("- release-planning due 2026-04-05\n"));
    }

    #[test]
    fn apply_note_set_preserves_frontmatter() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::create_dir_all(root.join("Inbox")).expect("note dir");
        fs::write(
            root.join("Inbox/Idea.md"),
            "---\nstatus: draft\n---\nOriginal body\n",
        )
        .expect("seed note");

        let report = apply_note_set(
            &paths,
            &NoteSetRequest {
                note: "Inbox/Idea".to_string(),
                replacement: "Updated body\n".to_string(),
                preserve_frontmatter: true,
            },
            None,
            Verbosity::Quiet,
        )
        .expect("set report");

        assert_eq!(report.path, "Inbox/Idea.md");
        assert!(report.preserved_frontmatter);
        assert_eq!(report.changed_paths, vec!["Inbox/Idea.md".to_string()]);

        let rendered = fs::read_to_string(root.join("Inbox/Idea.md"))
            .expect("updated note")
            .replace("\r\n", "\n");
        assert_eq!(rendered, "---\nstatus: draft\n---\nUpdated body\n");
    }

    #[test]
    fn ordinary_note_set_waits_for_vault_write_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::write(temp_dir.path().join("note.md"), "original\n").expect("seed note");
        let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("vault lock");
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).expect("start notification");
            let result = apply_note_set(
                &paths,
                &NoteSetRequest {
                    note: "note.md".to_string(),
                    replacement: "updated\n".to_string(),
                    preserve_frontmatter: false,
                },
                None,
                Verbosity::Quiet,
            );
            done_tx.send(result).expect("completion notification");
        });
        started_rx.recv().expect("worker started");
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("note.md")).expect("note before release"),
            "original\n"
        );
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker completion")
            .expect("note set");
        worker.join().expect("worker join");
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("note.md")).expect("note after release"),
            "updated\n"
        );
    }

    #[test]
    fn ordinary_note_set_rejects_stale_source() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        let note = temp_dir.path().join("note.md");
        fs::write(&note, "newer\n").expect("concurrent write");
        let error = super::write_ordinary_note_if_unchanged(
            &paths,
            "note.md",
            Some("original\n"),
            "stale replacement\n",
            "set",
        )
        .expect_err("stale replacement should fail");
        assert!(error.to_string().contains("note changed during note set"));
        assert_eq!(fs::read_to_string(note).expect("current note"), "newer\n");
    }

    #[test]
    fn ordinary_note_routing_and_locked_rechecks_preserve_explicit_profile() {
        let directory = tempdir().unwrap();
        let paths = VaultPaths::new(directory.path());
        initialize_vulcan_dir(&paths).unwrap();
        fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"note:note.md\"] }\nwrite = { allow = [\"note:note.md\"] }\n").unwrap();
        let note = directory.path().join("note.md");
        fs::write(&note, "original\n").unwrap();
        for config in [None, Some("hidden: [malformed")] {
            if let Some(config) = config {
                fs::write(directory.path().join("mdbase.yaml"), config).unwrap();
            }
            let errors = [
                super::write_note_content(
                    &paths,
                    "note.md",
                    Some("original\n"),
                    "updated\n",
                    "set",
                    Some("scoped"),
                    Verbosity::Quiet,
                )
                .unwrap_err(),
                super::write_ordinary_note_if_unchanged_with_profile(
                    &paths,
                    "note.md",
                    Some("original\n"),
                    "updated\n",
                    "set",
                    Some("scoped"),
                )
                .unwrap_err(),
                super::delete_ordinary_note_if_unchanged(
                    &paths,
                    "note.md",
                    "original\n",
                    Some("scoped"),
                )
                .unwrap_err(),
            ];
            for error in errors {
                assert_eq!(error.code(), Some("permission_denied"));
                assert_eq!(
                    error.message(),
                    "permission denied for required mdbase controls"
                );
            }
            assert_eq!(fs::read_to_string(&note).unwrap(), "original\n");
        }
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_note_set_publishes_a_complete_replacement_file() {
        use std::io::Read;

        let temporary = tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let note = temporary.path().join("note.md");
        fs::write(&note, "original\n").expect("original note");
        let mut old_handle = fs::File::open(&note).expect("open original note");

        super::write_ordinary_note_if_unchanged(
            &paths,
            "note.md",
            Some("original\n"),
            "updated\n",
            "set",
        )
        .expect("atomic note set");

        let mut old_contents = String::new();
        old_handle
            .read_to_string(&mut old_contents)
            .expect("read old file handle");
        assert_eq!(old_contents, "original\n");
        assert_eq!(
            fs::read_to_string(note).expect("published note"),
            "updated\n"
        );
    }

    #[test]
    fn ordinary_note_append_create_does_not_replace_a_new_file() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        let note = temp_dir.path().join("periodic.md");
        fs::write(&note, "created elsewhere\n").expect("concurrent create");
        super::write_ordinary_note_if_unchanged(
            &paths,
            "periodic.md",
            None,
            "stale periodic content\n",
            "append",
        )
        .expect_err("create collision should fail");
        assert_eq!(
            fs::read_to_string(note).expect("current note"),
            "created elsewhere\n"
        );
    }

    #[test]
    fn ordinary_note_create_waits_for_vault_write_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        let note = temp_dir.path().join("new.md");
        let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("vault lock");
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(apply_note_create(
                    &paths,
                    &NoteCreateRequest {
                        path: "new.md".to_string(),
                        template: None,
                        frontmatter: None,
                        body: "new content\n".to_string(),
                    },
                    None,
                    Verbosity::Quiet,
                ))
                .expect("completion notification");
        });
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(!note.exists());
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker completion")
            .expect("note create");
        worker.join().expect("worker join");
        assert!(fs::read_to_string(note)
            .expect("created note")
            .contains("new content"));
    }

    #[test]
    fn ordinary_note_delete_rejects_stale_source() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        let note = temp_dir.path().join("note.md");
        fs::write(&note, "newer\n").expect("concurrent write");
        let error = super::delete_ordinary_note_if_unchanged(&paths, "note.md", "original\n", None)
            .expect_err("stale delete should fail");
        assert!(error
            .to_string()
            .contains("note changed during note delete"));
        assert_eq!(fs::read_to_string(note).expect("current note"), "newer\n");
    }

    #[test]
    fn ordinary_note_delete_waits_for_vault_write_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        let note = temp_dir.path().join("note.md");
        fs::write(&note, "original\n").expect("seed note");
        let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("vault lock");
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(apply_note_delete(
                    &paths,
                    &NoteDeleteRequest {
                        note: "note.md".to_string(),
                        dry_run: false,
                    },
                    None,
                    Verbosity::Quiet,
                ))
                .expect("completion notification");
        });
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(
            fs::read_to_string(&note).expect("note before release"),
            "original\n"
        );
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker completion")
            .expect("note delete");
        worker.join().expect("worker join");
        assert!(!note.exists());
    }

    #[test]
    fn ordinary_note_append_waits_for_vault_write_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        let note = temp_dir.path().join("note.md");
        fs::write(&note, "original\n").expect("seed note");
        let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("vault lock");
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = apply_note_append(
                &paths,
                &NoteAppendRequest {
                    note: Some("note.md".to_string()),
                    text: "new entry".to_string(),
                    mode: NoteAppendMode::Append,
                    heading: None,
                    periodic: None,
                    date: None,
                    vars: HashMap::new(),
                },
                None,
                Verbosity::Quiet,
            );
            done_tx.send(result).expect("completion notification");
        });
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(
            fs::read_to_string(&note).expect("note before release"),
            "original\n"
        );
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker completion")
            .expect("note append");
        worker.join().expect("worker join");
        assert!(fs::read_to_string(note)
            .expect("note after release")
            .contains("new entry"));
    }

    #[test]
    fn ordinary_note_patch_waits_for_vault_write_lock() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        let note = temp_dir.path().join("note.md");
        fs::write(&note, "TODO\n").expect("seed note");
        let request = NotePatchRequest {
            target: resolve_existing_markdown_target(&paths, "note.md").expect("target"),
            section_id: None,
            heading: None,
            block_ref: None,
            lines: None,
            find: "TODO".to_string(),
            replace: "DONE".to_string(),
            replace_all: false,
            dry_run: false,
        };
        let held = vulcan_core::write_lock::acquire_write_lock(&paths).expect("vault lock");
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done_tx
                .send(apply_note_patch(&paths, &request, None, Verbosity::Quiet))
                .expect("completion notification");
        });
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(
            fs::read_to_string(&note).expect("note before release"),
            "TODO\n"
        );
        drop(held);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("worker completion")
            .expect("note patch");
        worker.join().expect("worker join");
        assert_eq!(
            fs::read_to_string(note).expect("note after release"),
            "DONE\n"
        );
    }

    #[test]
    fn note_set_command_report_runs_diagnostics_only_when_requested() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::write(temp_dir.path().join("Home.md"), "original\n").expect("seed note");

        let applied = apply_note_set(
            &paths,
            &NoteSetRequest {
                note: "Home.md".to_string(),
                replacement: "[[Missing]]\n".to_string(),
                preserve_frontmatter: true,
            },
            None,
            Verbosity::Quiet,
        )
        .expect("set note");
        let unchecked = finish_note_set_report(&paths, applied.clone(), false).expect("unchecked");
        assert!(!unchecked.checked);
        assert!(unchecked.diagnostics.is_empty());

        let checked = finish_note_set_report(&paths, applied, true).expect("checked");
        assert_eq!(checked.path, "Home.md");
        assert!(checked.checked);
        assert!(checked.preserved_frontmatter);
        assert!(checked
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("Missing") }));
    }

    #[test]
    fn create_and_append_command_reports_preserve_checked_diagnostics() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::write(temp_dir.path().join("Home.md"), "# Home\n").expect("seed note");

        let created = apply_note_create(
            &paths,
            &NoteCreateRequest {
                path: "New.md".to_string(),
                template: None,
                frontmatter: None,
                body: "[[Missing]]\n".to_string(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("create");
        let created = finish_note_create_report(&paths, created, true).expect("create report");
        assert!(created.created);
        assert!(created.checked);
        assert_eq!(created.changed_paths, vec!["New.md"]);
        assert!(created
            .diagnostics
            .iter()
            .any(|item| item.message.contains("Missing")));

        let appended = apply_note_append(
            &paths,
            &NoteAppendRequest {
                note: Some("Home.md".to_string()),
                text: "[[Missing]]".to_string(),
                mode: NoteAppendMode::Append,
                heading: None,
                periodic: None,
                date: None,
                vars: HashMap::new(),
            },
            None,
            Verbosity::Quiet,
        )
        .expect("append");
        let appended = finish_note_append_report(&paths, appended, true).expect("append report");
        assert!(appended.appended);
        assert!(appended.checked);
        assert!(!appended.created);
        assert!(appended
            .diagnostics
            .iter()
            .any(|item| item.message.contains("Missing")));
    }

    #[test]
    fn diagnose_note_contents_reports_unresolved_links() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::create_dir_all(root.join("Inbox")).expect("note dir");

        let diagnostics = diagnose_note_contents(
            &paths,
            "Inbox/Idea.md",
            "# Idea\n\nMissing [[Ghost Note]]\n",
        )
        .expect("diagnostics");

        assert!(diagnostics.iter().any(|issue| issue
            .message
            .contains("Unresolved link target `[[Ghost Note]]`")));
    }

    #[test]
    fn apply_note_patch_updates_selected_heading_scope() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        let note_path = root.join("scope.md");
        fs::write(&note_path, "# Title\n\n## Status\nTODO\n\n## Notes\nTODO\n").expect("seed note");

        let report = apply_note_patch(
            &VaultPaths::new(root),
            &NotePatchRequest {
                target: MarkdownTarget {
                    display_path: path_to_slash_string(&note_path),
                    absolute_path: note_path.clone(),
                    vault_relative_path: None,
                    config: vulcan_core::VaultConfig::default(),
                },
                section_id: None,
                heading: Some("Status".to_string()),
                block_ref: None,
                lines: None,
                find: "TODO".to_string(),
                replace: "DONE".to_string(),
                replace_all: false,
                dry_run: false,
            },
            None,
            Verbosity::Quiet,
        )
        .expect("patch report");

        assert_eq!(report.path, path_to_slash_string(&note_path));
        assert_eq!(report.match_count, 1);
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.line_spans.len(), 1);

        let updated = fs::read_to_string(&note_path)
            .expect("patched note")
            .replace("\r\n", "\n");
        assert_eq!(updated, "# Title\n\n## Status\nDONE\n\n## Notes\nTODO\n");
    }

    #[test]
    fn patch_command_report_checks_dry_run_content_without_writing() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init");
        fs::write(temp_dir.path().join("Home.md"), "# Home\nTODO\n").expect("seed note");
        let request = NotePatchRequest {
            target: resolve_existing_markdown_target(&paths, "Home.md").expect("target"),
            section_id: None,
            heading: Some("Home".to_string()),
            block_ref: None,
            lines: None,
            find: "TODO".to_string(),
            replace: "[[Missing]]".to_string(),
            replace_all: false,
            dry_run: true,
        };

        let applied =
            apply_note_patch(&paths, &request, None, Verbosity::Quiet).expect("patch preview");
        let report = finish_note_patch_report(&paths, &request, applied, true).expect("report");
        assert!(report.dry_run);
        assert!(report.checked);
        assert_eq!(report.path, "Home.md");
        assert_eq!(report.heading.as_deref(), Some("Home"));
        assert_eq!(report.pattern, "TODO");
        assert_eq!(report.match_count, 1);
        assert!(report
            .diagnostics
            .iter()
            .any(|item| item.message.contains("Missing")));
        assert_eq!(
            fs::read_to_string(temp_dir.path().join("Home.md")).expect("unchanged source"),
            "# Home\nTODO\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_note_patch_rejects_vault_symlink_without_modifying_target() {
        use std::os::unix::fs::symlink;

        let vault = tempdir().expect("vault temp dir");
        let outside = tempdir().expect("outside temp dir");
        let outside_note = outside.path().join("outside.md");
        fs::write(&outside_note, "secret TODO\n").expect("outside note");
        symlink(&outside_note, vault.path().join("linked.md")).expect("note symlink");
        let paths = VaultPaths::new(vault.path());

        let error = apply_note_patch(
            &paths,
            &NotePatchRequest {
                target: MarkdownTarget {
                    display_path: "linked.md".to_string(),
                    absolute_path: vault.path().join("linked.md"),
                    vault_relative_path: Some("linked.md".to_string()),
                    config: vulcan_core::VaultConfig::default(),
                },
                section_id: None,
                heading: None,
                block_ref: None,
                lines: None,
                find: "TODO".to_string(),
                replace: "DONE".to_string(),
                replace_all: false,
                dry_run: false,
            },
            None,
            Verbosity::Quiet,
        )
        .expect_err("symlinked vault note should be rejected");

        assert!(!error.to_string().is_empty());
        assert_eq!(
            fs::read_to_string(outside_note).expect("outside note should remain readable"),
            "secret TODO\n"
        );
    }

    #[test]
    fn apply_note_delete_reports_backlinks_and_removes_note() {
        let temp_dir = tempdir().expect("temp dir");
        let root = temp_dir.path();
        let paths = VaultPaths::new(root);
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::create_dir_all(root.join("Projects")).expect("projects dir");
        fs::write(root.join("Home.md"), "Links to [[Projects/Alpha]].\n").expect("home");
        fs::write(root.join("Projects/Alpha.md"), "# Alpha\n").expect("alpha");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

        let report = apply_note_delete(
            &paths,
            &NoteDeleteRequest {
                note: "Projects/Alpha".to_string(),
                dry_run: false,
            },
            None,
            Verbosity::Quiet,
        )
        .expect("delete report");

        assert_eq!(report.path, "Projects/Alpha.md");
        assert!(report.deleted);
        assert_eq!(report.backlink_count, 1);
        assert_eq!(report.changed_paths, vec!["Projects/Alpha.md".to_string()]);
        assert_eq!(report.backlinks[0].source_path, "Home.md");
        assert!(!root.join("Projects/Alpha.md").exists());
    }

    fn path_to_slash_string(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }
}
