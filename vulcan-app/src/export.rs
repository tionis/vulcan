use crate::config as app_config;
use crate::templates::{
    find_frontmatter_block, format_frontmatter_block, parse_frontmatter_document,
    TemplateTimestamp, YamlMapping,
};
use crate::AppError;
use pulldown_cmark::{
    html, CowStr, Event as MarkdownEvent, Options as MarkdownOptions, Parser as MarkdownParser,
    Tag as MarkdownTag,
};
use regex::Regex;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use toml::Value as TomlValue;
use vulcan_core::config::{
    ExportEpubTocStyleConfig, ExportGraphFormatConfig, ExportProfileConfig, ExportProfileFormat,
    LinkResolutionMode, VaultConfig,
};
use vulcan_core::content_transforms::apply_content_transforms;
use vulcan_core::content_transforms::{
    ContentReplacementRuleConfig, ContentTransformConfig, ContentTransformRuleConfig,
};
use vulcan_core::parser::{LinkKind, OriginContext};
use vulcan_core::permissions::PermissionFilter;
use vulcan_core::properties::load_note_index;
use vulcan_core::properties::{evaluate_note_inline_expressions, extract_indexed_properties};
use vulcan_core::resolver::{ResolverDocument, ResolverIndex, ResolverLink};
use vulcan_core::{
    ensure_vulcan_dir, execute_query_report_with_filter, load_vault_config, parse_document,
    validate_vulcan_overrides_toml, ConfigDiagnostic, EvaluatedInlineExpression, NoteRecord,
    ParsedDocument, QueryAst, QueryReport, VaultPaths,
};
use zip::write::FileOptions;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportProfileListEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportProfileShowReport {
    pub name: String,
    pub profile: Value,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip)]
    pub rendered_toml: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportProfileCreateRequest {
    pub format: ExportProfileFormat,
    pub query: Option<String>,
    pub query_json: Option<String>,
    pub path: PathBuf,
    pub title: Option<String>,
    pub author: Option<String>,
    pub toc: Option<ExportEpubTocStyleConfig>,
    pub backlinks: bool,
    pub frontmatter: bool,
    pub pretty: bool,
    pub graph_format: Option<ExportGraphFormatConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueUpdate<T> {
    Keep,
    Set(T),
    Clear,
}

impl<T> ConfigValueUpdate<T> {
    #[must_use]
    pub fn has_change(&self) -> bool {
        !matches!(self, Self::Keep)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolConfigUpdate {
    Keep,
    SetTrue,
    Clear,
}

impl BoolConfigUpdate {
    #[must_use]
    pub fn has_change(self) -> bool {
        !matches!(self, Self::Keep)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportProfileSetRequest {
    pub format: Option<ExportProfileFormat>,
    pub query: Option<String>,
    pub query_json: Option<String>,
    pub clear_query: bool,
    pub path: ConfigValueUpdate<PathBuf>,
    pub title: ConfigValueUpdate<String>,
    pub author: ConfigValueUpdate<String>,
    pub toc: ConfigValueUpdate<ExportEpubTocStyleConfig>,
    pub backlinks: BoolConfigUpdate,
    pub frontmatter: BoolConfigUpdate,
    pub pretty: BoolConfigUpdate,
    pub graph_format: ConfigValueUpdate<ExportGraphFormatConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportProfileRuleRequest {
    pub query: Option<String>,
    pub query_json: Option<String>,
    pub exclude_callouts: Vec<String>,
    pub exclude_headings: Vec<String>,
    pub exclude_frontmatter_keys: Vec<String>,
    pub exclude_inline_fields: Vec<String>,
    pub replacement_rules: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportProfileRuleMoveRequest {
    pub index: usize,
    pub before: Option<usize>,
    pub after: Option<usize>,
    pub last: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportProfileWriteAction {
    Created,
    Replaced,
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportProfileWriteReport {
    pub name: String,
    pub profile: Value,
    pub config_path: PathBuf,
    pub action: ExportProfileWriteAction,
    pub created_config: bool,
    pub dry_run: bool,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
    #[serde(skip)]
    pub rendered_toml: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportProfileDeleteReport {
    pub name: String,
    pub config_path: PathBuf,
    pub deleted: bool,
    pub dry_run: bool,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportProfileRuleWriteAction {
    Added,
    Updated,
    Moved,
    Deleted,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportProfileRuleListEntry {
    pub index: usize,
    pub rule: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportProfileRuleWriteReport {
    pub name: String,
    pub profile: Value,
    pub config_path: PathBuf,
    pub action: ExportProfileRuleWriteAction,
    pub rule_index: Option<usize>,
    pub previous_rule_index: Option<usize>,
    pub rule: Option<Value>,
    pub dry_run: bool,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
    #[serde(skip)]
    pub rendered_toml: String,
}

#[derive(Debug, Clone)]
struct ExportProfilePersistOutcome {
    created_config: bool,
    existing_profile: bool,
    updated: bool,
    diagnostics: Vec<ConfigDiagnostic>,
    changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedExportData {
    pub notes: Vec<ExportedNoteDocument>,
    pub links: Vec<ExportLinkRecord>,
}

#[derive(Debug, Clone)]
struct ParsedExportedNoteDocument {
    note: NoteRecord,
    content: String,
    parsed: ParsedDocument,
}

#[derive(Debug, Clone)]
struct ExportResolvedDocument {
    path: String,
    extension: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonNoteExportDocument {
    pub document_path: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_mtime: i64,
    pub file_size: i64,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub inlinks: Vec<String>,
    pub aliases: Vec<String>,
    pub frontmatter: Value,
    pub properties: Value,
    pub inline_expressions: Vec<EvaluatedInlineExpression>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonNotesExportReport {
    pub query: QueryAst,
    pub result_count: usize,
    pub notes: Vec<JsonNoteExportDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MarkdownExportSummary {
    pub path: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CsvExportSummary {
    pub path: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsonExportSummary {
    pub path: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZipExportSummary {
    pub path: String,
    pub result_count: usize,
    pub attachment_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EpubExportSummary {
    pub path: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpubExportOptions<'a> {
    pub title: Option<&'a str>,
    pub author: Option<&'a str>,
    pub backlinks: bool,
    pub frontmatter: bool,
    pub toc_style: ExportEpubTocStyleConfig,
}

#[derive(Clone, Copy)]
pub struct EpubRenderCallbacks<'a> {
    pub render_dataview_block: &'a dyn Fn(&VaultPaths, &str, &str, &str) -> String,
    pub render_base_embed: &'a dyn Fn(&VaultPaths, &str, Option<&str>) -> String,
    pub render_inline_value: &'a dyn Fn(&Value) -> String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ZipExportManifest {
    pub query: QueryAst,
    pub result_count: usize,
    pub notes: Vec<String>,
    pub attachments: Vec<String>,
}

fn resolve_export_query_ast(
    query: Option<&str>,
    query_json: Option<&str>,
) -> Result<QueryAst, AppError> {
    match (query, query_json) {
        (Some(_), Some(_)) => Err(AppError::operation(
            "provide either a note query DSL argument or --query-json, not both",
        )),
        (Some(query), None) => QueryAst::from_dsl(query).map_err(AppError::operation),
        (None, Some(query_json)) => QueryAst::from_json(query_json).map_err(AppError::operation),
        (None, None) => Err(AppError::operation(
            "provide a note query DSL argument or --query-json payload",
        )),
    }
}

pub fn execute_export_query(
    paths: &VaultPaths,
    query: Option<&str>,
    query_json: Option<&str>,
    filter: Option<&PermissionFilter>,
) -> Result<QueryReport, AppError> {
    let ast = resolve_export_query_ast(query, query_json)?;
    execute_query_report_with_filter(paths, ast, filter).map_err(AppError::operation)
}

pub fn load_exported_notes(
    paths: &VaultPaths,
    report: &QueryReport,
) -> Result<Vec<ExportedNoteDocument>, AppError> {
    report
        .notes
        .iter()
        .map(|note| {
            let content = fs::read_to_string(paths.vault_root().join(&note.document_path))
                .map_err(AppError::operation)?;
            Ok(ExportedNoteDocument {
                note: note.clone(),
                content,
            })
        })
        .collect()
}

fn synthetic_export_file_link(path: &str, extension: &str) -> String {
    if extension.eq_ignore_ascii_case("md") {
        format!("[[{}]]", path.strip_suffix(".md").unwrap_or(path))
    } else {
        format!("[[{path}]]")
    }
}

fn render_export_link_kind(kind: LinkKind) -> String {
    match kind {
        LinkKind::Wikilink => "wikilink",
        LinkKind::Markdown => "markdown",
        LinkKind::Embed => "embed",
        LinkKind::External => "external",
    }
    .to_string()
}

fn render_export_origin_context(origin: OriginContext) -> String {
    match origin {
        OriginContext::Body => "body",
        OriginContext::Frontmatter => "frontmatter",
        OriginContext::Property => "property",
    }
    .to_string()
}

fn load_export_resolution_documents(
    paths: &VaultPaths,
) -> Result<(ResolverIndex, HashMap<String, ExportResolvedDocument>), AppError> {
    let connection = Connection::open(paths.cache_db()).map_err(AppError::operation)?;

    let mut alias_statement = connection
        .prepare("SELECT document_id, alias_text FROM aliases ORDER BY document_id, alias_text")
        .map_err(AppError::operation)?;
    let alias_rows = alias_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(AppError::operation)?;
    let mut aliases_by_document = HashMap::<String, Vec<String>>::new();
    for row in alias_rows {
        let (document_id, alias_text) = row.map_err(AppError::operation)?;
        aliases_by_document
            .entry(document_id)
            .or_default()
            .push(alias_text);
    }

    let mut document_statement = connection
        .prepare("SELECT id, path, filename, extension FROM documents ORDER BY path")
        .map_err(AppError::operation)?;
    let document_rows = document_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(AppError::operation)?;

    let mut resolver_documents = Vec::new();
    let mut documents_by_id = HashMap::new();
    for row in document_rows {
        let (id, path, filename, extension) = row.map_err(AppError::operation)?;
        resolver_documents.push(ResolverDocument {
            id: id.clone(),
            path: path.clone(),
            filename,
            aliases: aliases_by_document.remove(&id).unwrap_or_default(),
        });
        documents_by_id.insert(id, ExportResolvedDocument { path, extension });
    }

    Ok((ResolverIndex::build(&resolver_documents), documents_by_id))
}

pub fn load_export_links(
    paths: &VaultPaths,
    notes: &[ExportedNoteDocument],
) -> Result<Vec<ExportLinkRecord>, AppError> {
    if notes.is_empty() {
        return Ok(Vec::new());
    }

    let document_ids = notes
        .iter()
        .map(|entry| entry.note.document_id.as_str())
        .collect::<Vec<_>>();
    let placeholders = document_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT
            source.path,
            links.raw_text,
            links.link_kind,
            links.display_text,
            links.target_path_candidate,
            links.target_heading,
            links.target_block,
            target.path,
            links.origin_context,
            links.byte_offset,
            target.extension
         FROM links
         JOIN documents AS source ON source.id = links.source_document_id
         LEFT JOIN documents AS target ON target.id = links.resolved_target_id
         WHERE links.source_document_id IN ({placeholders})
         ORDER BY source.path ASC, links.byte_offset ASC"
    );

    let connection = Connection::open(paths.cache_db()).map_err(AppError::operation)?;
    let mut statement = connection.prepare(&sql).map_err(AppError::operation)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(document_ids.iter()), |row| {
            Ok(ExportLinkRecord {
                source_document_path: row.get(0)?,
                raw_text: row.get(1)?,
                link_kind: row.get(2)?,
                display_text: row.get(3)?,
                target_path_candidate: row.get(4)?,
                target_heading: row.get(5)?,
                target_block: row.get(6)?,
                resolved_target_path: row.get(7)?,
                origin_context: row.get(8)?,
                byte_offset: row.get(9)?,
                resolved_target_extension: row.get(10)?,
            })
        })
        .map_err(AppError::operation)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::operation)
}

fn derive_export_links_from_notes(
    paths: &VaultPaths,
    notes: &[ParsedExportedNoteDocument],
    resolution_mode: LinkResolutionMode,
) -> Result<Vec<ExportLinkRecord>, AppError> {
    let (resolver_index, documents_by_id) = load_export_resolution_documents(paths)?;
    let mut links = Vec::new();

    for note in notes {
        for link in &note.parsed.links {
            let resolution = resolver_index.resolve(
                &ResolverLink {
                    source_document_id: note.note.document_id.clone(),
                    source_path: note.note.document_path.clone(),
                    target_path_candidate: link.target_path_candidate.clone(),
                    link_kind: link.link_kind,
                },
                resolution_mode,
            );
            let resolved = resolution
                .resolved_target_id
                .as_deref()
                .and_then(|id| documents_by_id.get(id));
            let byte_offset = i64::try_from(link.byte_offset).map_err(|_| {
                AppError::operation(format!(
                    "link byte offset overflow in {}",
                    note.note.document_path
                ))
            })?;

            links.push(ExportLinkRecord {
                source_document_path: note.note.document_path.clone(),
                raw_text: link.raw_text.clone(),
                link_kind: render_export_link_kind(link.link_kind),
                display_text: link.display_text.clone(),
                target_path_candidate: link.target_path_candidate.clone(),
                target_heading: link.target_heading.clone(),
                target_block: link.target_block.clone(),
                resolved_target_path: resolved.map(|document| document.path.clone()),
                origin_context: render_export_origin_context(link.origin_context),
                byte_offset,
                resolved_target_extension: resolved.map(|document| document.extension.clone()),
            });
        }
    }

    links.sort_by(|left, right| {
        left.source_document_path
            .cmp(&right.source_document_path)
            .then(left.byte_offset.cmp(&right.byte_offset))
    });
    Ok(links)
}

fn note_targets_by_source(links: &[ExportLinkRecord]) -> HashMap<String, BTreeSet<String>> {
    let mut targets = HashMap::<String, BTreeSet<String>>::new();
    for link in links {
        if link.link_kind != "wikilink" {
            continue;
        }
        if !link
            .resolved_target_extension
            .as_deref()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let Some(target_path) = link.resolved_target_path.as_ref() else {
            continue;
        };
        targets
            .entry(link.source_document_path.clone())
            .or_default()
            .insert(target_path.clone());
    }
    targets
}

fn transformed_note_links_by_source(links: &[ExportLinkRecord]) -> HashMap<String, Vec<String>> {
    let mut grouped = HashMap::<String, Vec<(i64, String)>>::new();
    for link in links {
        if link.link_kind != "wikilink" {
            continue;
        }
        grouped
            .entry(link.source_document_path.clone())
            .or_default()
            .push((link.byte_offset, link.raw_text.clone()));
    }

    grouped
        .into_iter()
        .map(|(path, mut values)| {
            values.sort_by_key(|(offset, _)| *offset);
            (
                path,
                values
                    .into_iter()
                    .map(|(_, raw_text)| raw_text)
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

pub fn prepare_export_data(
    paths: &VaultPaths,
    report: &QueryReport,
    read_filter: Option<&PermissionFilter>,
    transform_rules: Option<&[ContentTransformRuleConfig]>,
) -> Result<PreparedExportData, AppError> {
    let notes = load_exported_notes(paths, report)?;
    let Some(transform_rules) =
        transform_rules.filter(|rules| content_transform_rules_have_effective_transforms(rules))
    else {
        let links = load_export_links(paths, &notes)?;
        return Ok(PreparedExportData { notes, links });
    };
    let effective_transforms =
        build_effective_content_transforms(paths, report, read_filter, transform_rules)?;
    if effective_transforms.is_empty() {
        let links = load_export_links(paths, &notes)?;
        return Ok(PreparedExportData { notes, links });
    }

    prepare_transformed_export_data(paths, notes, &effective_transforms)
}

fn prepare_transformed_export_data(
    paths: &VaultPaths,
    notes: Vec<ExportedNoteDocument>,
    effective_transforms: &HashMap<String, ContentTransformConfig>,
) -> Result<PreparedExportData, AppError> {
    let original_links = load_export_links(paths, &notes)?;
    let config = load_vault_config(paths).config;
    let parsed_notes = notes
        .into_iter()
        .map(|entry| {
            let content = match effective_transforms.get(&entry.note.document_path) {
                Some(transforms) => apply_content_transforms(&entry.content, transforms),
                None => entry.content,
            };
            let parsed = parse_document(&content, &config);
            ParsedExportedNoteDocument {
                note: entry.note,
                content,
                parsed,
            }
        })
        .collect::<Vec<_>>();
    let transformed_links =
        derive_export_links_from_notes(paths, &parsed_notes, config.link_resolution)?;
    let transformed_targets = note_targets_by_source(&transformed_links);
    let original_targets = note_targets_by_source(&original_links);
    let transformed_note_links = transformed_note_links_by_source(&transformed_links);
    let (mut exported_notes, note_indexes) =
        build_transformed_exported_notes(parsed_notes, &transformed_note_links, &config)?;
    apply_transformed_backlink_adjustments(
        &mut exported_notes,
        &note_indexes,
        &original_targets,
        &transformed_targets,
    );
    evaluate_transformed_export_inline_expressions(paths, &mut exported_notes)?;

    Ok(PreparedExportData {
        notes: exported_notes,
        links: transformed_links,
    })
}

fn build_transformed_exported_notes(
    parsed_notes: Vec<ParsedExportedNoteDocument>,
    transformed_note_links: &HashMap<String, Vec<String>>,
    config: &VaultConfig,
) -> Result<(Vec<ExportedNoteDocument>, HashMap<String, usize>), AppError> {
    let mut exported_notes = Vec::with_capacity(parsed_notes.len());
    let mut note_indexes = HashMap::<String, usize>::new();

    for parsed_note in parsed_notes {
        let mut note = parsed_note.note;
        note.tags = parsed_note
            .parsed
            .tags
            .iter()
            .map(|tag| tag.tag_text.clone())
            .collect();
        note.links = transformed_note_links
            .get(&note.document_path)
            .cloned()
            .unwrap_or_default();
        note.aliases.clone_from(&parsed_note.parsed.aliases);
        note.frontmatter = transformed_export_frontmatter(&parsed_note.parsed);
        note.properties = transformed_export_properties(&parsed_note.parsed, config)?;
        note.raw_inline_expressions = parsed_note
            .parsed
            .inline_expressions
            .iter()
            .map(|expression| expression.expression.clone())
            .collect();
        note.inline_expressions.clear();

        note_indexes.insert(note.document_path.clone(), exported_notes.len());
        exported_notes.push(ExportedNoteDocument {
            note,
            content: parsed_note.content,
        });
    }

    Ok((exported_notes, note_indexes))
}

fn apply_transformed_backlink_adjustments(
    exported_notes: &mut [ExportedNoteDocument],
    note_indexes: &HashMap<String, usize>,
    original_targets: &HashMap<String, BTreeSet<String>>,
    transformed_targets: &HashMap<String, BTreeSet<String>>,
) {
    let backlink_adjustments = exported_notes
        .iter()
        .map(|export_note| {
            let source_path = export_note.note.document_path.clone();
            let source_link = synthetic_export_file_link(&source_path, &export_note.note.file_ext);
            let original = original_targets
                .get(&source_path)
                .cloned()
                .unwrap_or_default();
            let transformed = transformed_targets
                .get(&source_path)
                .cloned()
                .unwrap_or_default();
            (source_link, original, transformed)
        })
        .collect::<Vec<_>>();

    for (source_link, original, transformed) in backlink_adjustments {
        for target in original.difference(&transformed) {
            let Some(&target_index) = note_indexes.get(target) else {
                continue;
            };
            exported_notes[target_index]
                .note
                .inlinks
                .retain(|candidate| candidate != &source_link);
        }
        for target in transformed.difference(&original) {
            let Some(&target_index) = note_indexes.get(target) else {
                continue;
            };
            if !exported_notes[target_index]
                .note
                .inlinks
                .iter()
                .any(|candidate| candidate == &source_link)
            {
                exported_notes[target_index]
                    .note
                    .inlinks
                    .push(source_link.clone());
            }
        }
    }
}

fn evaluate_transformed_export_inline_expressions(
    paths: &VaultPaths,
    exported_notes: &mut [ExportedNoteDocument],
) -> Result<(), AppError> {
    let mut note_lookup = load_note_index(paths).map_err(AppError::operation)?;
    for export_note in exported_notes.iter() {
        note_lookup.insert(export_note.note.file_name.clone(), export_note.note.clone());
    }
    for export_note in exported_notes.iter_mut() {
        export_note.note.inline_expressions =
            evaluate_note_inline_expressions(&export_note.note, &note_lookup);
    }
    Ok(())
}

fn transformed_export_frontmatter(parsed: &ParsedDocument) -> Value {
    parsed.frontmatter.as_ref().map_or_else(
        || Value::Object(serde_json::Map::new()),
        |frontmatter| match serde_json::to_value(frontmatter) {
            Ok(Value::Object(object)) => Value::Object(object),
            Ok(_) | Err(_) => Value::Object(serde_json::Map::new()),
        },
    )
}

fn transformed_export_properties(
    parsed: &ParsedDocument,
    config: &VaultConfig,
) -> Result<Value, AppError> {
    let Some(indexed) = extract_indexed_properties(parsed, config).map_err(AppError::operation)?
    else {
        return Ok(Value::Object(serde_json::Map::new()));
    };

    serde_json::from_str(&indexed.canonical_json).map_err(AppError::operation)
}

fn build_effective_content_transforms(
    paths: &VaultPaths,
    report: &QueryReport,
    read_filter: Option<&PermissionFilter>,
    transform_rules: &[ContentTransformRuleConfig],
) -> Result<HashMap<String, ContentTransformConfig>, AppError> {
    let exported_paths = report
        .notes
        .iter()
        .map(|note| note.document_path.clone())
        .collect::<HashSet<_>>();
    let mut effective = HashMap::<String, ContentTransformConfig>::new();

    for rule in transform_rules.iter().filter(|rule| !rule.is_empty()) {
        let matched_paths = if rule.query.is_none() && rule.query_json.is_none() {
            exported_paths.iter().cloned().collect::<Vec<_>>()
        } else {
            execute_export_query(
                paths,
                rule.query.as_deref(),
                rule.query_json.as_deref(),
                read_filter,
            )?
            .notes
            .into_iter()
            .map(|note| note.document_path)
            .filter(|path| exported_paths.contains(path))
            .collect::<Vec<_>>()
        };

        for path in matched_paths {
            effective
                .entry(path)
                .or_default()
                .merge_in(&rule.transforms);
        }
    }

    Ok(effective)
}

fn json_note_export_report(
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
) -> JsonNotesExportReport {
    JsonNotesExportReport {
        query: report.query.clone(),
        result_count: notes.len(),
        notes: notes
            .iter()
            .map(|entry| JsonNoteExportDocument {
                document_path: entry.note.document_path.clone(),
                file_name: entry.note.file_name.clone(),
                file_ext: entry.note.file_ext.clone(),
                file_mtime: entry.note.file_mtime,
                file_size: entry.note.file_size,
                tags: entry.note.tags.clone(),
                links: entry.note.links.clone(),
                inlinks: entry.note.inlinks.clone(),
                aliases: entry.note.aliases.clone(),
                frontmatter: entry.note.frontmatter.clone(),
                properties: entry.note.properties.clone(),
                inline_expressions: entry.note.inline_expressions.clone(),
                content: entry.content.clone(),
            })
            .collect(),
    }
}

pub fn render_json_export_payload(
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
    pretty: bool,
) -> Result<String, AppError> {
    let payload = json_note_export_report(report, notes);
    if pretty {
        serde_json::to_string_pretty(&payload).map_err(AppError::operation)
    } else {
        serde_json::to_string(&payload).map_err(AppError::operation)
    }
}

#[must_use]
pub fn render_markdown_export_payload(
    notes: &[ExportedNoteDocument],
    title: Option<&str>,
) -> String {
    if notes.len() == 1 && title.is_none() {
        let mut rendered = notes[0].content.clone();
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        return rendered;
    }

    let mut rendered = String::new();
    if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
        rendered.push_str("# ");
        rendered.push_str(title);
        rendered.push_str("\n\n");
    }

    for (index, note) in notes.iter().enumerate() {
        if index > 0 {
            rendered.push_str("\n\n---\n\n");
        }
        rendered.push_str("## ");
        rendered.push_str(&note.note.document_path);
        rendered.push_str("\n\n");
        rendered.push_str(&note.content);
        if !note.content.ends_with('\n') {
            rendered.push('\n');
        }
    }

    rendered
}

fn query_export_rows(report: &QueryReport) -> Result<Vec<Value>, AppError> {
    let query_value = serde_json::to_value(&report.query).map_err(AppError::operation)?;
    Ok(report
        .notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "file_name": note.file_name,
                "file_ext": note.file_ext,
                "file_mtime": note.file_mtime,
                "tags": note.tags,
                "starred": note.starred,
                "properties": note.properties,
                "inline_expressions": note.inline_expressions,
                "query": query_value,
            })
        })
        .collect())
}

fn query_export_fields() -> &'static [&'static str] {
    &[
        "document_path",
        "file_name",
        "file_ext",
        "file_mtime",
        "tags",
        "starred",
        "properties",
        "inline_expressions",
        "query",
    ]
}

pub fn render_csv_export_payload(report: &QueryReport) -> Result<String, AppError> {
    let rows = query_export_rows(report)?;
    let fields = query_export_fields();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(fields.iter().copied())
        .map_err(AppError::operation)?;
    for row in &rows {
        let selected = row
            .as_object()
            .ok_or_else(|| AppError::operation("query export row was not an object"))?;
        let record = fields
            .iter()
            .map(|field| csv_cell_for_value(selected.get(*field)))
            .collect::<Vec<_>>();
        writer.write_record(record).map_err(AppError::operation)?;
    }
    writer.flush().map_err(AppError::operation)?;
    let bytes = writer.into_inner().map_err(AppError::operation)?;
    String::from_utf8(bytes)
        .map_err(|error| AppError::operation(format!("csv export was not valid UTF-8: {error}")))
}

#[must_use]
pub fn collect_export_attachment_paths(links: &[ExportLinkRecord]) -> Vec<String> {
    let mut attachments = links
        .iter()
        .filter_map(|link| {
            let extension = link.resolved_target_extension.as_deref()?;
            (!matches!(extension, "md" | "base"))
                .then(|| link.resolved_target_path.clone())
                .flatten()
        })
        .collect::<BTreeSet<_>>();
    attachments.retain(|path| !path.trim().is_empty());
    attachments.into_iter().collect()
}

fn csv_cell_for_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

pub fn write_zip_export(
    paths: &VaultPaths,
    output_path: &Path,
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
) -> Result<ZipExportSummary, AppError> {
    prepare_export_output_path(output_path)?;

    let attachments = collect_export_attachment_paths(links);
    let file = fs::File::create(output_path).map_err(AppError::operation)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for note in notes {
        writer
            .start_file(&note.note.document_path, options)
            .map_err(AppError::operation)?;
        writer
            .write_all(note.content.as_bytes())
            .map_err(AppError::operation)?;
    }

    for attachment in &attachments {
        writer
            .start_file(attachment, options)
            .map_err(AppError::operation)?;
        let bytes = fs::read(paths.vault_root().join(attachment)).map_err(AppError::operation)?;
        writer.write_all(&bytes).map_err(AppError::operation)?;
    }

    let notes_json = render_json_export_payload(report, notes, true)?;
    writer
        .start_file(".vulcan-export/notes.json", options)
        .map_err(AppError::operation)?;
    writer
        .write_all(notes_json.as_bytes())
        .map_err(AppError::operation)?;

    let manifest = ZipExportManifest {
        query: report.query.clone(),
        result_count: notes.len(),
        notes: notes
            .iter()
            .map(|entry| entry.note.document_path.clone())
            .collect(),
        attachments,
    };
    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(AppError::operation)?;
    writer
        .start_file(".vulcan-export/manifest.json", options)
        .map_err(AppError::operation)?;
    writer
        .write_all(manifest_json.as_bytes())
        .map_err(AppError::operation)?;

    writer.finish().map_err(AppError::operation)?;

    Ok(ZipExportSummary {
        path: output_path.display().to_string(),
        result_count: notes.len(),
        attachment_count: manifest.attachments.len(),
    })
}

#[derive(Debug, Clone)]
struct EpubHeading {
    level: u8,
    text: String,
    anchor_id: String,
}

#[derive(Debug, Clone)]
struct EpubChapter {
    document_path: String,
    title: String,
    nav_path: String,
    file_href: String,
    headings: Vec<EpubHeading>,
    content: String,
}

#[derive(Debug, Clone)]
struct EpubAsset {
    source_path: String,
    manifest_id: String,
    package_href: String,
    media_type: String,
}

struct EpubRenderContext<'a, 'render> {
    config: &'a VaultConfig,
    note_index: &'a HashMap<String, NoteRecord>,
    note_targets: &'a HashMap<String, String>,
    tag_targets: &'a HashMap<String, String>,
    asset_targets: &'a HashMap<String, String>,
    callbacks: &'render EpubRenderCallbacks<'render>,
}

#[derive(Clone, Copy)]
struct EpubChapterBuildOptions<'a, 'render> {
    asset_targets: &'a HashMap<String, String>,
    tag_targets: &'a HashMap<String, String>,
    backlinks: bool,
    include_frontmatter: bool,
    callbacks: EpubRenderCallbacks<'render>,
}

#[derive(Debug, Clone)]
struct EpubBookMetadata<'a> {
    title: &'a str,
    author: Option<&'a str>,
    identifier: &'a str,
}

#[derive(Debug, Clone)]
struct EpubTagPage {
    title: String,
    nav_path: String,
    file_href: String,
    content: String,
}

#[derive(Debug, Clone)]
enum EpubNavNode {
    Directory {
        name: String,
        children: Vec<EpubNavNode>,
    },
    Chapter {
        chapter: EpubChapter,
    },
    TagSection {
        title: String,
        pages: Vec<EpubTagPage>,
    },
}

#[derive(Debug, Clone)]
struct EpubMarkdownReplacement {
    range: std::ops::Range<usize>,
    replacement: String,
}

fn default_epub_title(paths: &VaultPaths) -> String {
    paths
        .vault_root()
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map_or_else(|| "Vulcan Export".to_string(), ToOwned::to_owned)
}

fn extension_suffix(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_default()
}

fn epub_media_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("tif" | "tiff") => "image/tiff",
        Some("pdf") => "application/pdf",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        _ => "application/octet-stream",
    }
}

fn build_epub_assets(links: &[ExportLinkRecord]) -> Vec<EpubAsset> {
    collect_export_attachment_paths(links)
        .into_iter()
        .enumerate()
        .map(|(index, source_path)| {
            let suffix = extension_suffix(&source_path);
            let file_name = format!("asset-{:03}{suffix}", index + 1);
            EpubAsset {
                source_path: source_path.clone(),
                manifest_id: format!("asset-{}", index + 1),
                package_href: format!("media/{file_name}"),
                media_type: epub_media_type(&source_path).to_string(),
            }
        })
        .collect()
}

fn build_epub_asset_targets(assets: &[EpubAsset]) -> HashMap<String, String> {
    assets
        .iter()
        .map(|asset| {
            (
                asset.source_path.clone(),
                format!("../{}", asset.package_href),
            )
        })
        .collect()
}

fn build_epub_link_targets(notes: &[ExportedNoteDocument]) -> HashMap<String, String> {
    let mut file_name_counts = HashMap::new();
    for note in notes {
        *file_name_counts
            .entry(note.note.file_name.clone())
            .or_insert(0_usize) += 1;
    }

    let mut targets = HashMap::new();
    for (index, note) in notes.iter().enumerate() {
        let chapter_name = format!("chapter-{:03}.xhtml", index + 1);
        let path = note.note.document_path.clone();
        targets.insert(path.clone(), chapter_name.clone());
        if let Some(stem) = path.strip_suffix(".md") {
            targets.insert(stem.to_string(), chapter_name.clone());
        }
        if file_name_counts
            .get(&note.note.file_name)
            .copied()
            .unwrap_or_default()
            == 1
        {
            targets.insert(note.note.file_name.clone(), chapter_name);
        }
    }
    targets
}

fn build_epub_tag_targets(notes: &[ExportedNoteDocument]) -> HashMap<String, String> {
    let tags = notes
        .iter()
        .flat_map(|note| note.note.tags.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut targets = HashMap::new();
    let mut seen = HashMap::new();

    for tag in tags {
        let slug = slugify_epub_fragment(&tag);
        let occurrence = seen.entry(slug.clone()).or_insert(0_usize);
        *occurrence += 1;
        let suffix = if *occurrence == 1 {
            String::new()
        } else {
            format!("-{}", *occurrence)
        };
        targets.insert(tag, format!("tag-{slug}{suffix}.xhtml"));
    }

    targets
}

fn normalize_epub_target_path(path: &str) -> String {
    let mut normalized = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized.to_string_lossy().replace('\\', "/")
}

fn resolve_epub_lookup_keys(source_document_path: &str, path_part: &str) -> Vec<String> {
    let trimmed = path_part.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut keys = Vec::new();
    let mut push_key = |candidate: String| {
        if !candidate.is_empty() && !keys.contains(&candidate) {
            keys.push(candidate);
        }
    };

    let direct = trimmed.trim_start_matches("./").to_string();
    push_key(direct.clone());

    let normalized_direct = normalize_epub_target_path(&direct);
    push_key(normalized_direct.clone());

    if trimmed.starts_with('.') || trimmed.contains('/') {
        if let Some(source_dir) = Path::new(source_document_path).parent() {
            let joined = normalize_epub_target_path(&source_dir.join(trimmed).to_string_lossy());
            push_key(joined);
        }
    }

    keys
}

fn resolve_epub_note_href(
    source_document_path: &str,
    path_part: &str,
    fragment: Option<&str>,
    targets: &HashMap<String, String>,
) -> Option<String> {
    for key in resolve_epub_lookup_keys(source_document_path, path_part) {
        let Some(target) = targets
            .get(&key)
            .or_else(|| key.strip_suffix(".md").and_then(|stem| targets.get(stem)))
        else {
            continue;
        };

        let mut rewritten = target.clone();
        if let Some(fragment) = fragment
            .map(slugify_epub_fragment)
            .filter(|value| !value.is_empty())
        {
            rewritten.push('#');
            rewritten.push_str(&fragment);
        }
        return Some(rewritten);
    }

    None
}

fn resolve_epub_asset_href(
    source_document_path: &str,
    path_part: &str,
    fragment: Option<&str>,
    targets: &HashMap<String, String>,
) -> Option<String> {
    for key in resolve_epub_lookup_keys(source_document_path, path_part) {
        let Some(target) = targets.get(&key) else {
            continue;
        };

        let mut rewritten = target.clone();
        if let Some(fragment) = fragment.filter(|value| !value.is_empty()) {
            rewritten.push('#');
            rewritten.push_str(fragment);
        }
        return Some(rewritten);
    }

    None
}

fn render_epub_tag_text(tag: &str) -> String {
    if tag.starts_with('#') {
        tag.to_string()
    } else {
        format!("#{tag}")
    }
}

fn render_epub_tag_link_html(tag: &str, href: &str) -> String {
    format!(
        "<a class=\"tag-link\" href=\"{}\">{}</a>",
        escape_xml_text(href),
        escape_xml_text(&render_epub_tag_text(tag))
    )
}

fn strip_epub_paragraph_wrapper(html: &str) -> String {
    let trimmed = html.trim();
    trimmed
        .strip_prefix("<p>")
        .and_then(|value| value.strip_suffix("</p>"))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn render_epub_inline_fragment_html(
    source: &str,
    source_document_path: &str,
    note_targets: &HashMap<String, String>,
    asset_targets: &HashMap<String, String>,
) -> String {
    strip_epub_paragraph_wrapper(&render_epub_markdown_html(
        source,
        source_document_path,
        note_targets,
        asset_targets,
    ))
}

fn render_epub_message_html(title: &str, message: &str) -> String {
    format!(
        "<div class=\"render-message\"><strong>{}</strong> {}</div>",
        escape_xml_text(title),
        escape_xml_text(message)
    )
}

fn render_epub_inline_field_html(
    key: &str,
    value_text: &str,
    source_document_path: &str,
    note_targets: &HashMap<String, String>,
    asset_targets: &HashMap<String, String>,
) -> String {
    let value_html = render_epub_inline_fragment_html(
        value_text,
        source_document_path,
        note_targets,
        asset_targets,
    );
    format!(
        "<span class=\"dataview-inline-field\"><span class=\"dataview-inline-field-key\">{}</span><span class=\"dataview-inline-field-separator\">:</span> <span class=\"dataview-inline-field-value\">{}</span></span>",
        escape_xml_text(key),
        value_html
    )
}

fn apply_epub_markdown_replacements(
    source: &str,
    mut replacements: Vec<EpubMarkdownReplacement>,
) -> String {
    replacements.sort_by(|left, right| {
        left.range
            .start
            .cmp(&right.range.start)
            .then(right.range.end.cmp(&left.range.end))
    });

    let mut rendered = String::new();
    let mut cursor = 0_usize;
    for replacement in replacements {
        if replacement.range.start < cursor
            || replacement.range.end < replacement.range.start
            || replacement.range.end > source.len()
        {
            continue;
        }
        rendered.push_str(&source[cursor..replacement.range.start]);
        rendered.push_str(&replacement.replacement);
        cursor = replacement.range.end;
    }
    rendered.push_str(&source[cursor..]);
    rendered
}

fn render_epub_asset_label(link: &ExportLinkRecord) -> String {
    link.display_text
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            link.resolved_target_path.as_deref().and_then(|path| {
                Path::new(path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(ToOwned::to_owned)
            })
        })
        .unwrap_or_else(|| "Attachment".to_string())
}

fn render_epub_asset_embed_html(link: &ExportLinkRecord, href: &str) -> String {
    let label = render_epub_asset_label(link);
    let extension = link
        .resolved_target_extension
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "tif" | "tiff" => format!(
            "<figure class=\"asset-embed asset-embed-image\"><img src=\"{}\" alt=\"{}\" /></figure>",
            escape_xml_text(href),
            escape_xml_text(&label)
        ),
        "mp3" | "wav" | "ogg" => format!(
            "<audio class=\"asset-embed asset-embed-audio\" controls=\"controls\" src=\"{}\">{}</audio>",
            escape_xml_text(href),
            escape_xml_text(&label)
        ),
        "mp4" | "mov" | "webm" | "avi" => format!(
            "<video class=\"asset-embed asset-embed-video\" controls=\"controls\" src=\"{}\">{}</video>",
            escape_xml_text(href),
            escape_xml_text(&label)
        ),
        _ => format!(
            "<p class=\"asset-embed asset-embed-link\"><a href=\"{}\">{}</a></p>",
            escape_xml_text(href),
            escape_xml_text(&label)
        ),
    }
}

fn render_epub_note_markdown(
    paths: &VaultPaths,
    note: &ExportedNoteDocument,
    body: &str,
    body_offset: usize,
    render_context: &EpubRenderContext<'_, '_>,
    export_links: &HashMap<i64, &ExportLinkRecord>,
) -> String {
    let parsed = parse_document(body, render_context.config);
    let inline_results = evaluate_note_inline_expressions(&note.note, render_context.note_index);
    let mut replacements = Vec::new();

    for field in &parsed.inline_fields {
        replacements.push(EpubMarkdownReplacement {
            range: field.byte_range.clone(),
            replacement: render_epub_inline_field_html(
                &field.key,
                &field.value_text,
                &note.note.document_path,
                render_context.note_targets,
                render_context.asset_targets,
            ),
        });
    }

    for (index, expression) in parsed.inline_expressions.iter().enumerate() {
        let replacement = inline_results.get(index).map_or_else(
            || expression.expression.clone(),
            |result| {
                result.error.as_ref().map_or_else(
                    || (render_context.callbacks.render_inline_value)(&result.value),
                    |error| render_epub_message_html("Dataview inline error:", error),
                )
            },
        );
        replacements.push(EpubMarkdownReplacement {
            range: expression.byte_range.clone(),
            replacement,
        });
    }

    for block in &parsed.dataview_blocks {
        replacements.push(EpubMarkdownReplacement {
            range: block.byte_range.clone(),
            replacement: (render_context.callbacks.render_dataview_block)(
                paths,
                &note.note.document_path,
                &block.language,
                &block.text,
            ),
        });
    }

    for link in &parsed.links {
        if link.link_kind != LinkKind::Embed {
            continue;
        }
        let Some(base_path) = link.target_path_candidate.as_deref().filter(|candidate| {
            Path::new(candidate)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("base"))
        }) else {
            let Some(export_byte_offset) = i64::try_from(link.byte_offset + body_offset).ok()
            else {
                continue;
            };
            let Some(export_link) = export_links.get(&export_byte_offset) else {
                continue;
            };
            let Some(resolved_target_path) = export_link.resolved_target_path.as_deref() else {
                continue;
            };
            let Some(asset_href) = render_context.asset_targets.get(resolved_target_path) else {
                continue;
            };
            replacements.push(EpubMarkdownReplacement {
                range: link.byte_offset..link.byte_offset + link.raw_text.len(),
                replacement: render_epub_asset_embed_html(export_link, asset_href),
            });
            continue;
        };
        replacements.push(EpubMarkdownReplacement {
            range: link.byte_offset..link.byte_offset + link.raw_text.len(),
            replacement: (render_context.callbacks.render_base_embed)(
                paths,
                base_path,
                link.target_heading.as_deref(),
            ),
        });
    }

    for tag in &parsed.tags {
        let Some(file_href) = render_context.tag_targets.get(&tag.tag_text) else {
            continue;
        };
        replacements.push(EpubMarkdownReplacement {
            range: tag.byte_offset..tag.byte_offset + tag.tag_text.len() + 1,
            replacement: render_epub_tag_link_html(&tag.tag_text, &format!("../tags/{file_href}")),
        });
    }

    apply_epub_markdown_replacements(body, replacements)
}

fn is_external_epub_href(destination: &str) -> bool {
    destination.starts_with('#')
        || destination.starts_with("mailto:")
        || destination.starts_with("tel:")
        || destination.starts_with("obsidian:")
        || destination.contains("://")
}

fn slugify_epub_fragment(text: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for character in text.trim().chars() {
        let lower = character.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "section".to_string()
    } else {
        slug
    }
}

fn rewrite_epub_link_destination(
    source_document_path: &str,
    destination: &str,
    note_targets: &HashMap<String, String>,
    asset_targets: &HashMap<String, String>,
) -> Option<String> {
    if destination.is_empty() || is_external_epub_href(destination) {
        return None;
    }

    let (path_part, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    resolve_epub_note_href(source_document_path, path_part, fragment, note_targets).or_else(|| {
        resolve_epub_asset_href(source_document_path, path_part, fragment, asset_targets)
    })
}

fn rewrite_epub_image_destination(
    source_document_path: &str,
    destination: &str,
    asset_targets: &HashMap<String, String>,
) -> Option<String> {
    if destination.is_empty() || is_external_epub_href(destination) {
        return None;
    }

    let (path_part, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    resolve_epub_asset_href(source_document_path, path_part, fragment, asset_targets)
}

fn render_epub_markdown_html(
    source: &str,
    source_document_path: &str,
    note_targets: &HashMap<String, String>,
    asset_targets: &HashMap<String, String>,
) -> String {
    let parser = MarkdownParser::new_ext(source, MarkdownOptions::all()).map(|event| match event {
        MarkdownEvent::Start(MarkdownTag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => MarkdownEvent::Start(MarkdownTag::Link {
            link_type,
            dest_url: rewrite_epub_link_destination(
                source_document_path,
                &dest_url,
                note_targets,
                asset_targets,
            )
            .map(CowStr::from)
            .unwrap_or(dest_url),
            title,
            id,
        }),
        MarkdownEvent::Start(MarkdownTag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => MarkdownEvent::Start(MarkdownTag::Image {
            link_type,
            dest_url: rewrite_epub_image_destination(
                source_document_path,
                &dest_url,
                asset_targets,
            )
            .map(CowStr::from)
            .unwrap_or(dest_url),
            title,
            id,
        }),
        other => other,
    });

    let mut rendered = String::new();
    html::push_html(&mut rendered, parser);
    rendered
}

fn collect_epub_headings(source: &str, config: &VaultConfig) -> Vec<EpubHeading> {
    let parsed = parse_document(source, config);
    let mut seen = HashMap::new();
    parsed
        .headings
        .into_iter()
        .map(|heading| {
            let slug = slugify_epub_fragment(&heading.text);
            let occurrence = seen.entry(slug.clone()).or_insert(0_usize);
            *occurrence += 1;
            let anchor_id = if *occurrence == 1 {
                slug
            } else {
                format!("{slug}-{}", *occurrence)
            };
            EpubHeading {
                level: heading.level,
                text: heading.text,
                anchor_id,
            }
        })
        .collect()
}

fn inject_epub_heading_ids(html: &str, headings: &[EpubHeading]) -> String {
    let mut rendered = String::with_capacity(html.len() + headings.len() * 16);
    let mut cursor = 0_usize;

    for heading in headings {
        let needle = format!("<h{}>", heading.level);
        let Some(relative_start) = html[cursor..].find(&needle) else {
            continue;
        };
        let start = cursor + relative_start;
        rendered.push_str(&html[cursor..start]);
        write!(
            rendered,
            "<h{} id=\"{}\">",
            heading.level, heading.anchor_id
        )
        .expect("writing to string cannot fail");
        cursor = start + needle.len();
    }

    rendered.push_str(&html[cursor..]);
    rendered
}

fn escape_xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn render_epub_backlinks(
    backlinks: &[String],
    link_targets: &HashMap<String, String>,
) -> Option<String> {
    let unique = backlinks
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique.is_empty() {
        return None;
    }

    let mut rendered = String::from("<section class=\"backlinks\"><h2>Backlinks</h2><ul>");
    for backlink in unique {
        let (lookup_key, label) = backlink
            .strip_prefix("[[")
            .and_then(|value| value.strip_suffix("]]"))
            .map_or_else(
                || (backlink.to_string(), backlink.to_string()),
                |value| {
                    let target = value.split('|').next().unwrap_or(value);
                    let target = target.split('#').next().unwrap_or(target).trim();
                    let label = value
                        .split('|')
                        .nth(1)
                        .map(str::trim)
                        .filter(|display| !display.is_empty())
                        .unwrap_or(target);
                    (target.to_string(), label.to_string())
                },
            );
        rendered.push_str("<li>");
        if let Some(target) = link_targets.get(&lookup_key) {
            write!(
                rendered,
                "<a href=\"{}\">{}</a>",
                escape_xml_text(target),
                escape_xml_text(&label)
            )
            .expect("writing to string cannot fail");
        } else {
            rendered.push_str(&escape_xml_text(&label));
        }
        rendered.push_str("</li>");
    }
    rendered.push_str("</ul></section>");
    Some(rendered)
}

fn render_epub_chapter_tags_html(
    tags: &[String],
    tag_targets: &HashMap<String, String>,
) -> Option<String> {
    let unique = tags
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if unique.is_empty() {
        return None;
    }

    let rendered_tags = unique
        .into_iter()
        .map(|tag| {
            tag_targets.get(tag).map_or_else(
                || escape_xml_text(&render_epub_tag_text(tag)),
                |file_href| render_epub_tag_link_html(tag, &format!("../tags/{file_href}")),
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("<p class=\"note-tags\">Tags: {rendered_tags}</p>"))
}

fn render_epub_chapter_document(
    chapter_title: &str,
    note_path: Option<&str>,
    tags_html: Option<&str>,
    frontmatter_html: Option<&str>,
    html_body: &str,
    backlinks_html: Option<&str>,
    stylesheet_href: &str,
) -> String {
    let mut rendered = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\" />\n",
    );
    write!(
        rendered,
        "<title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"{}\" />\n",
        escape_xml_text(chapter_title),
        escape_xml_text(stylesheet_href)
    )
    .expect("writing to string cannot fail");
    rendered.push_str("</head>\n<body>\n<article class=\"chapter\">\n");
    write!(
        rendered,
        "<header class=\"chapter-header\"><h1 class=\"chapter-title\">{}</h1>",
        escape_xml_text(chapter_title)
    )
    .expect("writing to string cannot fail");
    if let Some(note_path) = note_path {
        write!(
            rendered,
            "<p class=\"note-path\">{}</p>",
            escape_xml_text(note_path)
        )
        .expect("writing to string cannot fail");
    }
    if let Some(tags_html) = tags_html {
        rendered.push_str(tags_html);
    }
    rendered.push_str("</header>\n<section class=\"chapter-body\">");
    if let Some(frontmatter_html) = frontmatter_html {
        rendered.push_str(frontmatter_html);
        rendered.push('\n');
    }
    rendered.push_str(html_body);
    rendered.push_str("</section>\n");
    if let Some(backlinks_html) = backlinks_html {
        rendered.push_str(backlinks_html);
        rendered.push('\n');
    }
    rendered.push_str("</article>\n</body>\n</html>\n");
    rendered
}

fn render_epub_frontmatter_html(frontmatter: &YamlMapping) -> Result<String, AppError> {
    let yaml = format_frontmatter_block(frontmatter).map_err(AppError::operation)?;
    Ok(format!(
        "<details class=\"frontmatter-box\"><summary>Frontmatter</summary><pre><code>{}</code></pre></details>",
        escape_xml_text(&yaml)
    ))
}

fn render_epub_tag_page_document(
    tag: &str,
    note_links: &[(String, String)],
    stylesheet_href: &str,
) -> String {
    let title = format!("Tag {}", render_epub_tag_text(tag));
    let mut rendered = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\" />\n",
    );
    write!(
        rendered,
        "<title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"{}\" />\n",
        escape_xml_text(&title),
        escape_xml_text(stylesheet_href)
    )
    .expect("writing to string cannot fail");
    rendered.push_str("</head>\n<body>\n<article class=\"chapter tag-page\">\n");
    writeln!(
        rendered,
        "<header class=\"chapter-header\"><h1 class=\"chapter-title\">{}</h1><p class=\"note-tags\">{} note(s)</p></header>",
        escape_xml_text(&title),
        note_links.len()
    )
    .expect("writing to string cannot fail");
    rendered.push_str("<section class=\"chapter-body\"><ul class=\"tag-note-list\">");
    for (note_title, href) in note_links {
        write!(
            rendered,
            "<li><a href=\"{}\">{}</a></li>",
            escape_xml_text(href),
            escape_xml_text(note_title)
        )
        .expect("writing to string cannot fail");
    }
    rendered.push_str("</ul></section>\n</article>\n</body>\n</html>\n");
    rendered
}

fn body_offset_for_epub_document(source: &str) -> usize {
    find_frontmatter_block(source).map_or(0, |(_, _, body_start)| body_start)
}

fn build_epub_export_links_by_source(
    links: &[ExportLinkRecord],
) -> HashMap<String, HashMap<i64, &ExportLinkRecord>> {
    let mut export_links = HashMap::<String, HashMap<i64, &ExportLinkRecord>>::new();
    for link in links {
        export_links
            .entry(link.source_document_path.clone())
            .or_default()
            .insert(link.byte_offset, link);
    }
    export_links
}

fn build_epub_chapter(
    paths: &VaultPaths,
    note: &ExportedNoteDocument,
    chapter_index: usize,
    backlinks: bool,
    include_frontmatter: bool,
    render_context: &EpubRenderContext<'_, '_>,
    export_links: &HashMap<i64, &ExportLinkRecord>,
) -> Result<EpubChapter, AppError> {
    let body_offset = body_offset_for_epub_document(&note.content);
    let (frontmatter, body) =
        parse_frontmatter_document(&note.content, false).map_err(AppError::operation)?;
    let rendered_markdown = render_epub_note_markdown(
        paths,
        note,
        &body,
        body_offset,
        render_context,
        export_links,
    );
    let headings = collect_epub_headings(&rendered_markdown, render_context.config);
    let chapter_title = headings
        .first()
        .filter(|heading| heading.level == 1)
        .map_or_else(
            || note.note.file_name.clone(),
            |heading| heading.text.clone(),
        );
    let body_html = inject_epub_heading_ids(
        &render_epub_markdown_html(
            &rendered_markdown,
            &note.note.document_path,
            render_context.note_targets,
            render_context.asset_targets,
        ),
        &headings,
    );
    let backlinks_html = backlinks
        .then(|| render_epub_backlinks(&note.note.inlinks, render_context.note_targets))
        .flatten();
    let tags_html = render_epub_chapter_tags_html(&note.note.tags, render_context.tag_targets);
    let frontmatter_html = if include_frontmatter {
        frontmatter
            .as_ref()
            .map(render_epub_frontmatter_html)
            .transpose()?
    } else {
        None
    };
    let file_href = format!("chapter-{:03}.xhtml", chapter_index + 1);
    Ok(EpubChapter {
        document_path: note.note.document_path.clone(),
        title: chapter_title.clone(),
        nav_path: format!("text/{file_href}"),
        file_href,
        headings,
        content: render_epub_chapter_document(
            &chapter_title,
            Some(&note.note.document_path),
            tags_html.as_deref(),
            frontmatter_html.as_deref(),
            &body_html,
            backlinks_html.as_deref(),
            "../styles.css",
        ),
    })
}

fn build_epub_chapters(
    paths: &VaultPaths,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
    options: EpubChapterBuildOptions<'_, '_>,
) -> Result<Vec<EpubChapter>, AppError> {
    let config = load_vault_config(paths).config;
    let note_index = load_note_index(paths).map_err(AppError::operation)?;
    let note_targets = build_epub_link_targets(notes);
    let render_context = EpubRenderContext {
        config: &config,
        note_index: &note_index,
        note_targets: &note_targets,
        tag_targets: options.tag_targets,
        asset_targets: options.asset_targets,
        callbacks: &options.callbacks,
    };
    let export_links = build_epub_export_links_by_source(links);
    let empty_links = HashMap::new();

    let mut chapters = notes
        .iter()
        .enumerate()
        .map(|(index, note)| {
            build_epub_chapter(
                paths,
                note,
                index,
                options.backlinks,
                options.include_frontmatter,
                &render_context,
                export_links
                    .get(&note.note.document_path)
                    .unwrap_or(&empty_links),
            )
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    if chapters.is_empty() {
        chapters.push(EpubChapter {
            document_path: String::new(),
            title: "No results".to_string(),
            nav_path: "text/chapter-001.xhtml".to_string(),
            file_href: "chapter-001.xhtml".to_string(),
            headings: Vec::new(),
            content: render_epub_chapter_document(
                "No results",
                None,
                None,
                None,
                "<p>No notes matched this export query.</p>",
                None,
                "../styles.css",
            ),
        });
    }

    Ok(chapters)
}

fn render_epub_nav_headings(chapter: &EpubChapter) -> String {
    let mut headings = chapter.headings.as_slice();
    if headings
        .first()
        .is_some_and(|heading| heading.level == 1 && heading.text == chapter.title)
    {
        headings = &headings[1..];
    }
    if headings.is_empty() {
        return String::new();
    }

    let mut rendered = String::from("<ol>");
    for heading in headings {
        write!(
            rendered,
            "<li><a href=\"{}#{}\">{}</a></li>",
            escape_xml_text(&chapter.nav_path),
            escape_xml_text(&heading.anchor_id),
            escape_xml_text(&heading.text)
        )
        .expect("writing to string cannot fail");
    }
    rendered.push_str("</ol>");
    rendered
}

fn build_epub_tag_pages(
    chapters: &[EpubChapter],
    notes: &[ExportedNoteDocument],
    tag_targets: &HashMap<String, String>,
) -> Vec<EpubTagPage> {
    let chapters_by_path = chapters
        .iter()
        .map(|chapter| (chapter.document_path.as_str(), chapter))
        .collect::<HashMap<_, _>>();
    let mut tags = tag_targets.iter().collect::<Vec<_>>();
    tags.sort_by(|left, right| left.0.cmp(right.0));

    tags.into_iter()
        .filter_map(|(tag, file_href)| {
            let note_links = notes
                .iter()
                .filter(|note| note.note.tags.iter().any(|candidate| candidate == tag))
                .filter_map(|note| {
                    chapters_by_path
                        .get(note.note.document_path.as_str())
                        .map(|chapter| (chapter.title.clone(), format!("../{}", chapter.nav_path)))
                })
                .collect::<Vec<_>>();
            if note_links.is_empty() {
                return None;
            }

            Some(EpubTagPage {
                title: format!("Tag {}", render_epub_tag_text(tag)),
                nav_path: format!("tags/{file_href}"),
                file_href: file_href.clone(),
                content: render_epub_tag_page_document(tag, &note_links, "../styles.css"),
            })
        })
        .collect()
}

fn common_epub_directory_prefix_len(chapters: &[EpubChapter]) -> usize {
    let directories = chapters
        .iter()
        .filter(|chapter| !chapter.document_path.is_empty())
        .map(|chapter| {
            let mut segments = chapter.document_path.split('/').collect::<Vec<_>>();
            let _ = segments.pop();
            segments
        })
        .collect::<Vec<_>>();
    let Some(first) = directories.first() else {
        return 0;
    };

    let mut prefix_len = first.len();
    for directory in directories.iter().skip(1) {
        prefix_len = prefix_len.min(directory.len());
        for index in 0..prefix_len {
            if first[index] != directory[index] {
                prefix_len = index;
                break;
            }
        }
    }
    prefix_len
}

fn insert_epub_nav_chapter(
    nodes: &mut Vec<EpubNavNode>,
    directories: &[&str],
    chapter: EpubChapter,
) {
    let Some((name, remaining)) = directories.split_first() else {
        nodes.push(EpubNavNode::Chapter { chapter });
        return;
    };

    if let Some(EpubNavNode::Directory { children, .. }) = nodes.iter_mut().find(|node| {
        matches!(
            node,
            EpubNavNode::Directory {
                name: existing,
                children: _,
            } if existing == name
        )
    }) {
        insert_epub_nav_chapter(children, remaining, chapter);
    } else {
        let mut children = Vec::new();
        insert_epub_nav_chapter(&mut children, remaining, chapter);
        nodes.push(EpubNavNode::Directory {
            name: (*name).to_string(),
            children,
        });
    }
}

fn build_epub_nav_nodes(
    chapters: &[EpubChapter],
    tag_pages: &[EpubTagPage],
    toc_style: ExportEpubTocStyleConfig,
) -> Vec<EpubNavNode> {
    let mut nodes = Vec::new();

    if toc_style == ExportEpubTocStyleConfig::Flat {
        nodes.extend(
            chapters
                .iter()
                .cloned()
                .map(|chapter| EpubNavNode::Chapter { chapter }),
        );
    } else {
        let prefix_len = common_epub_directory_prefix_len(chapters);
        for chapter in chapters {
            if chapter.document_path.is_empty() {
                nodes.push(EpubNavNode::Chapter {
                    chapter: chapter.clone(),
                });
                continue;
            }
            let mut segments = chapter.document_path.split('/').collect::<Vec<_>>();
            let _ = segments.pop();
            let trimmed = if prefix_len >= segments.len() {
                &[][..]
            } else {
                &segments[prefix_len..]
            };
            insert_epub_nav_chapter(&mut nodes, trimmed, chapter.clone());
        }
    }

    if !tag_pages.is_empty() {
        nodes.push(EpubNavNode::TagSection {
            title: "Tags".to_string(),
            pages: tag_pages.to_vec(),
        });
    }

    nodes
}

fn render_epub_nav_nodes(nodes: &[EpubNavNode]) -> String {
    let mut rendered = String::from("<ol>");
    for node in nodes {
        match node {
            EpubNavNode::Directory { name, children } => {
                write!(
                    rendered,
                    "<li class=\"toc-directory\"><span class=\"toc-directory-label\">{}</span>{}</li>",
                    escape_xml_text(name),
                    render_epub_nav_nodes(children)
                )
                .expect("writing to string cannot fail");
            }
            EpubNavNode::Chapter { chapter } => {
                write!(
                    rendered,
                    "<li><a href=\"{}\">{}</a>{}</li>",
                    escape_xml_text(&chapter.nav_path),
                    escape_xml_text(&chapter.title),
                    render_epub_nav_headings(chapter)
                )
                .expect("writing to string cannot fail");
            }
            EpubNavNode::TagSection { title, pages } => {
                write!(
                    rendered,
                    "<li class=\"toc-directory toc-tag-section\"><span class=\"toc-directory-label\">{}</span><ol>",
                    escape_xml_text(title)
                )
                .expect("writing to string cannot fail");
                for page in pages {
                    write!(
                        rendered,
                        "<li><a href=\"{}\">{}</a></li>",
                        escape_xml_text(&page.nav_path),
                        escape_xml_text(&page.title)
                    )
                    .expect("writing to string cannot fail");
                }
                rendered.push_str("</ol></li>");
            }
        }
    }
    rendered.push_str("</ol>");
    rendered
}

fn epub_nav_node_primary_path(node: &EpubNavNode) -> Option<&str> {
    match node {
        EpubNavNode::Directory { children, .. } => {
            children.first().and_then(epub_nav_node_primary_path)
        }
        EpubNavNode::Chapter { chapter } => Some(chapter.nav_path.as_str()),
        EpubNavNode::TagSection { pages, .. } => pages.first().map(|page| page.nav_path.as_str()),
    }
}

fn render_epub_ncx_nodes(nodes: &[EpubNavNode], play_order: &mut usize) -> String {
    let mut rendered = String::new();
    for node in nodes {
        match node {
            EpubNavNode::Directory { name, children } => {
                let current = *play_order;
                *play_order += 1;
                let nested = render_epub_ncx_nodes(children, play_order);
                let src = children
                    .first()
                    .and_then(epub_nav_node_primary_path)
                    .unwrap_or("text/chapter-001.xhtml");
                writeln!(
                    rendered,
                    "<navPoint id=\"nav-{}\" playOrder=\"{}\"><navLabel><text>{}</text></navLabel><content src=\"{}\" />{}</navPoint>",
                    current,
                    current,
                    escape_xml_text(name),
                    escape_xml_text(src),
                    nested
                )
                .expect("writing to string cannot fail");
            }
            EpubNavNode::Chapter { chapter } => {
                let current = *play_order;
                *play_order += 1;
                writeln!(
                    rendered,
                    "<navPoint id=\"nav-{}\" playOrder=\"{}\"><navLabel><text>{}</text></navLabel><content src=\"{}\" /></navPoint>",
                    current,
                    current,
                    escape_xml_text(&chapter.title),
                    escape_xml_text(&chapter.nav_path)
                )
                .expect("writing to string cannot fail");
            }
            EpubNavNode::TagSection { title, pages } => {
                let current = *play_order;
                *play_order += 1;
                let src = pages
                    .first()
                    .map_or("text/chapter-001.xhtml", |page| page.nav_path.as_str());
                write!(
                    rendered,
                    "<navPoint id=\"nav-{}\" playOrder=\"{}\"><navLabel><text>{}</text></navLabel><content src=\"{}\" />",
                    current,
                    current,
                    escape_xml_text(title),
                    escape_xml_text(src)
                )
                .expect("writing to string cannot fail");
                for page in pages {
                    let page_order = *play_order;
                    *play_order += 1;
                    writeln!(
                        rendered,
                        "<navPoint id=\"nav-{}\" playOrder=\"{}\"><navLabel><text>{}</text></navLabel><content src=\"{}\" /></navPoint>",
                        page_order,
                        page_order,
                        escape_xml_text(&page.title),
                        escape_xml_text(&page.nav_path)
                    )
                    .expect("writing to string cannot fail");
                }
                rendered.push_str("</navPoint>\n");
            }
        }
    }
    rendered
}

fn render_epub_nav_document(book_title: &str, nodes: &[EpubNavNode]) -> String {
    let mut rendered = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\" />\n",
    );
    write!(
        rendered,
        "<title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"styles.css\" />\n",
        escape_xml_text(book_title)
    )
    .expect("writing to string cannot fail");
    rendered.push_str("</head>\n<body>\n<nav epub:type=\"toc\" id=\"toc\">\n<h1>Contents</h1>\n");
    rendered.push_str(&render_epub_nav_nodes(nodes));
    rendered.push_str("\n</nav>\n</body>\n</html>\n");
    rendered
}

fn render_epub_ncx(book_title: &str, nodes: &[EpubNavNode], identifier: &str) -> String {
    let mut rendered = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n",
    );
    writeln!(
        rendered,
        "<head><meta name=\"dtb:uid\" content=\"{}\" /></head>",
        escape_xml_text(identifier)
    )
    .expect("writing to string cannot fail");
    write!(
        rendered,
        "<docTitle><text>{}</text></docTitle>\n<navMap>\n",
        escape_xml_text(book_title)
    )
    .expect("writing to string cannot fail");
    let mut play_order = 1_usize;
    rendered.push_str(&render_epub_ncx_nodes(nodes, &mut play_order));
    rendered.push_str("</navMap>\n</ncx>\n");
    rendered
}

fn current_utc_timestamp_string() -> String {
    TemplateTimestamp::current().default_strings().datetime
}

fn render_epub_package(
    book_title: &str,
    author: Option<&str>,
    chapters: &[EpubChapter],
    assets: &[EpubAsset],
    tag_pages: &[EpubTagPage],
    identifier: &str,
) -> String {
    let modified = current_utc_timestamp_string();
    let mut rendered = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\">\n\
         <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n",
    );
    write!(
        rendered,
        "<dc:identifier id=\"bookid\">{}</dc:identifier>\n<dc:title>{}</dc:title>\n<dc:language>en</dc:language>\n",
        escape_xml_text(identifier),
        escape_xml_text(book_title)
    )
    .expect("writing to string cannot fail");
    if let Some(author) = author.filter(|value| !value.trim().is_empty()) {
        writeln!(
            rendered,
            "<dc:creator>{}</dc:creator>",
            escape_xml_text(author)
        )
        .expect("writing to string cannot fail");
    }
    write!(
        rendered,
        "<meta property=\"dcterms:modified\">{modified}</meta>\n</metadata>\n<manifest>\n\
         <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\" />\n\
         <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\" />\n\
         <item id=\"css\" href=\"styles.css\" media-type=\"text/css\" />\n"
    )
    .expect("writing to string cannot fail");
    for (index, chapter) in chapters.iter().enumerate() {
        writeln!(
            rendered,
            "<item id=\"chapter-{}\" href=\"{}\" media-type=\"application/xhtml+xml\" />",
            index + 1,
            escape_xml_text(&chapter.nav_path)
        )
        .expect("writing to string cannot fail");
    }
    for asset in assets {
        writeln!(
            rendered,
            "<item id=\"{}\" href=\"{}\" media-type=\"{}\" />",
            escape_xml_text(&asset.manifest_id),
            escape_xml_text(&asset.package_href),
            escape_xml_text(&asset.media_type)
        )
        .expect("writing to string cannot fail");
    }
    for (index, page) in tag_pages.iter().enumerate() {
        writeln!(
            rendered,
            "<item id=\"tag-{}\" href=\"{}\" media-type=\"application/xhtml+xml\" />",
            index + 1,
            escape_xml_text(&page.nav_path)
        )
        .expect("writing to string cannot fail");
    }
    rendered.push_str("</manifest>\n<spine toc=\"ncx\">\n");
    for index in 0..chapters.len() {
        writeln!(rendered, "<itemref idref=\"chapter-{}\" />", index + 1)
            .expect("writing to string cannot fail");
    }
    for index in 0..tag_pages.len() {
        writeln!(rendered, "<itemref idref=\"tag-{}\" />", index + 1)
            .expect("writing to string cannot fail");
    }
    rendered.push_str("</spine>\n</package>\n");
    rendered
}

fn epub_stylesheet() -> &'static str {
    "body { font-family: serif; line-height: 1.5; margin: 0; padding: 0 1rem 2rem; }\n\
     .chapter-header { border-bottom: 1px solid #d0d0d0; margin-bottom: 1.5rem; padding-bottom: 0.75rem; }\n\
     .chapter-title { margin-bottom: 0.25rem; }\n\
     .note-path, .note-tags { color: #555; font-size: 0.95em; margin: 0.15rem 0; }\n\
     .asset-embed { margin: 1rem 0; }\n\
     .asset-embed-image img, .asset-embed-video { display: block; max-width: 100%; }\n\
     .asset-embed-audio, .asset-embed-video { width: 100%; }\n\
     .asset-embed-link a { font-weight: 600; }\n\
     .frontmatter-box { background: #f6f4ef; border: 1px solid #d7d2c7; border-radius: 0.45rem; margin: 0 0 1.25rem; padding: 0.75rem 0.9rem; }\n\
     .frontmatter-box summary { cursor: pointer; font-weight: 600; }\n\
     .frontmatter-box pre { background: #fffdf8; border: 1px solid #e3ddd2; margin: 0.75rem 0 0; overflow-x: auto; padding: 0.75rem; white-space: pre-wrap; }\n\
     .tag-link { text-decoration: none; }\n\
     .toc-directory-label { font-weight: 600; }\n\
     .toc-directory > ol, .toc-headings { margin-top: 0.35rem; }\n\
     .dataview-inline-field { background: #f4f4f4; border-radius: 0.3rem; padding: 0.05rem 0.35rem; }\n\
     .dataview-inline-field-key { font-weight: 600; }\n\
     .render-message { border-left: 0.2rem solid #888; color: #444; margin: 0.75rem 0; padding: 0.35rem 0.75rem; }\n\
     .tag-note-list { padding-left: 1.25rem; }\n\
     .backlinks { border-top: 1px solid #d0d0d0; margin-top: 2rem; padding-top: 1rem; }\n\
     code, pre { font-family: monospace; }\n"
}

fn write_epub_scaffold(
    writer: &mut zip::ZipWriter<fs::File>,
    metadata: &EpubBookMetadata<'_>,
    chapters: &[EpubChapter],
    assets: &[EpubAsset],
    tag_pages: &[EpubTagPage],
    nav_nodes: &[EpubNavNode],
) -> Result<(), AppError> {
    let stored = FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    writer
        .start_file("mimetype", stored)
        .map_err(AppError::operation)?;
    writer
        .write_all(b"application/epub+zip")
        .map_err(AppError::operation)?;

    writer
        .start_file("META-INF/container.xml", deflated)
        .map_err(AppError::operation)?;
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#,
        )
        .map_err(AppError::operation)?;

    writer
        .start_file("OEBPS/content.opf", deflated)
        .map_err(AppError::operation)?;
    writer
        .write_all(
            render_epub_package(
                metadata.title,
                metadata.author,
                chapters,
                assets,
                tag_pages,
                metadata.identifier,
            )
            .as_bytes(),
        )
        .map_err(AppError::operation)?;

    writer
        .start_file("OEBPS/nav.xhtml", deflated)
        .map_err(AppError::operation)?;
    writer
        .write_all(render_epub_nav_document(metadata.title, nav_nodes).as_bytes())
        .map_err(AppError::operation)?;

    writer
        .start_file("OEBPS/toc.ncx", deflated)
        .map_err(AppError::operation)?;
    writer
        .write_all(render_epub_ncx(metadata.title, nav_nodes, metadata.identifier).as_bytes())
        .map_err(AppError::operation)?;

    writer
        .start_file("OEBPS/styles.css", deflated)
        .map_err(AppError::operation)?;
    writer
        .write_all(epub_stylesheet().as_bytes())
        .map_err(AppError::operation)?;

    Ok(())
}

fn write_epub_assets(
    writer: &mut zip::ZipWriter<fs::File>,
    paths: &VaultPaths,
    assets: &[EpubAsset],
) -> Result<(), AppError> {
    let deflated = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for asset in assets {
        writer
            .start_file(format!("OEBPS/{}", asset.package_href), deflated)
            .map_err(AppError::operation)?;
        let bytes =
            fs::read(paths.vault_root().join(&asset.source_path)).map_err(AppError::operation)?;
        writer.write_all(&bytes).map_err(AppError::operation)?;
    }

    Ok(())
}

fn write_epub_documents(
    writer: &mut zip::ZipWriter<fs::File>,
    chapters: &[EpubChapter],
    tag_pages: &[EpubTagPage],
) -> Result<(), AppError> {
    let deflated = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for chapter in chapters {
        writer
            .start_file(format!("OEBPS/text/{}", chapter.file_href), deflated)
            .map_err(AppError::operation)?;
        writer
            .write_all(chapter.content.as_bytes())
            .map_err(AppError::operation)?;
    }

    for page in tag_pages {
        writer
            .start_file(format!("OEBPS/tags/{}", page.file_href), deflated)
            .map_err(AppError::operation)?;
        writer
            .write_all(page.content.as_bytes())
            .map_err(AppError::operation)?;
    }

    Ok(())
}

pub fn write_epub_export(
    paths: &VaultPaths,
    output_path: &Path,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
    options: EpubExportOptions<'_>,
    callbacks: EpubRenderCallbacks<'_>,
) -> Result<EpubExportSummary, AppError> {
    prepare_export_output_path(output_path)?;
    let assets = build_epub_assets(links);
    let asset_targets = build_epub_asset_targets(&assets);
    let tag_targets = build_epub_tag_targets(notes);
    let chapters = build_epub_chapters(
        paths,
        notes,
        links,
        EpubChapterBuildOptions {
            asset_targets: &asset_targets,
            tag_targets: &tag_targets,
            backlinks: options.backlinks,
            include_frontmatter: options.frontmatter,
            callbacks,
        },
    )?;
    let tag_pages = build_epub_tag_pages(&chapters, notes, &tag_targets);
    let nav_nodes = build_epub_nav_nodes(&chapters, &tag_pages, options.toc_style);
    let book_title = options
        .title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| default_epub_title(paths), ToOwned::to_owned);
    let identifier = format!("urn:vulcan:{}", current_utc_timestamp_string());
    let metadata = EpubBookMetadata {
        title: &book_title,
        author: options.author,
        identifier: &identifier,
    };

    let file = fs::File::create(output_path).map_err(AppError::operation)?;
    let mut writer = zip::ZipWriter::new(file);
    write_epub_scaffold(
        &mut writer,
        &metadata,
        &chapters,
        &assets,
        &tag_pages,
        &nav_nodes,
    )?;
    write_epub_assets(&mut writer, paths, &assets)?;
    write_epub_documents(&mut writer, &chapters, &tag_pages)?;

    writer.finish().map_err(AppError::operation)?;

    Ok(EpubExportSummary {
        path: output_path.display().to_string(),
        result_count: notes.len(),
    })
}

#[must_use]
pub fn build_export_profile_list(paths: &VaultPaths) -> Vec<ExportProfileListEntry> {
    load_vault_config(paths)
        .config
        .export
        .profiles
        .into_iter()
        .map(|(name, profile)| ExportProfileListEntry {
            resolved_path: profile.path.as_deref().map(|path| {
                resolve_export_profile_output_path(paths, path)
                    .display()
                    .to_string()
            }),
            path: profile.path.map(|path| path.display().to_string()),
            format: profile
                .format
                .map(export_profile_format_label)
                .map(ToOwned::to_owned),
            query: profile.query,
            name,
        })
        .collect()
}

pub fn build_export_profile_show_report(
    paths: &VaultPaths,
    name: &str,
) -> Result<ExportProfileShowReport, AppError> {
    validate_export_profile_name(name)?;
    let loaded = load_vault_config(paths);
    let profile = loaded
        .config
        .export
        .profiles
        .get(name)
        .cloned()
        .ok_or_else(|| AppError::operation(format!("unknown export profile `{name}`")))?;

    Ok(ExportProfileShowReport {
        name: name.to_string(),
        profile: serde_json::to_value(&profile).map_err(AppError::operation)?,
        diagnostics: normalize_config_diagnostics(paths, &loaded.diagnostics),
        rendered_toml: render_export_profile_section_toml_string(name, &profile)?,
    })
}

pub fn apply_export_profile_create(
    paths: &VaultPaths,
    name: &str,
    request: &ExportProfileCreateRequest,
    replace_existing: bool,
    dry_run: bool,
) -> Result<ExportProfileWriteReport, AppError> {
    validate_export_profile_name(name)?;
    let profile = build_export_profile_config(request);
    validate_export_profile_config(name, &profile)?;

    let existing = load_shared_export_profile(paths, name)?;
    if existing.is_some() && !replace_existing {
        return Err(AppError::operation(format!(
            "export profile `{name}` already exists; pass --replace to overwrite it"
        )));
    }

    let persisted = persist_shared_export_profile(paths, name, &profile, dry_run)?;
    let action = if !persisted.updated {
        ExportProfileWriteAction::Unchanged
    } else if persisted.existing_profile {
        ExportProfileWriteAction::Replaced
    } else {
        ExportProfileWriteAction::Created
    };

    build_export_profile_write_report(
        paths,
        name,
        &profile,
        action,
        persisted.created_config,
        dry_run,
        persisted.diagnostics,
        persisted.changed_paths,
    )
}

pub fn apply_export_profile_set(
    paths: &VaultPaths,
    name: &str,
    request: &ExportProfileSetRequest,
    dry_run: bool,
) -> Result<ExportProfileWriteReport, AppError> {
    validate_export_profile_name(name)?;
    if !export_profile_set_request_has_changes(request) {
        return Err(AppError::operation(
            "export profile set requires at least one field to update",
        ));
    }

    let mut profile = load_shared_export_profile(paths, name)?
        .ok_or_else(|| AppError::operation(format!("unknown export profile `{name}`")))?;
    let changed = apply_export_profile_settings(&mut profile, request);
    validate_export_profile_config(name, &profile)?;
    let persisted = persist_shared_export_profile(paths, name, &profile, dry_run)?;

    build_export_profile_write_report(
        paths,
        name,
        &profile,
        if changed && persisted.updated {
            ExportProfileWriteAction::Updated
        } else {
            ExportProfileWriteAction::Unchanged
        },
        persisted.created_config,
        dry_run,
        persisted.diagnostics,
        persisted.changed_paths,
    )
}

pub fn apply_export_profile_delete(
    paths: &VaultPaths,
    name: &str,
    dry_run: bool,
) -> Result<ExportProfileDeleteReport, AppError> {
    validate_export_profile_name(name)?;
    let config_path = paths.config_file().to_path_buf();
    let existing_contents = fs::read_to_string(&config_path).ok();
    let mut config_value = app_config::load_config_file_toml(&config_path)?;
    let storage_path = shared_export_profile_storage_path(name);
    if !app_config::config_toml_path_exists(&config_value, &storage_path) {
        return Err(AppError::operation(format!(
            "unknown export profile `{name}`"
        )));
    }

    let deleted = app_config::remove_config_toml_value(&mut config_value, &storage_path)?;
    let rendered = toml::to_string_pretty(&config_value).map_err(AppError::operation)?;
    validate_vulcan_overrides_toml(&rendered).map_err(AppError::operation)?;
    let updated = existing_contents.as_deref() != Some(rendered.as_str());

    let changed_paths = if !dry_run && updated {
        fs::write(&config_path, rendered).map_err(AppError::operation)?;
        vec![relativize_path_string(paths, &config_path)]
    } else {
        Vec::new()
    };

    Ok(ExportProfileDeleteReport {
        name: name.to_string(),
        config_path: relativize_path(paths, &config_path),
        deleted,
        dry_run,
        diagnostics: normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics),
        changed_paths,
    })
}

pub fn build_export_profile_rule_list(
    paths: &VaultPaths,
    name: &str,
) -> Result<Vec<ExportProfileRuleListEntry>, AppError> {
    validate_export_profile_name(name)?;
    let profile = load_shared_export_profile(paths, name)?
        .ok_or_else(|| AppError::operation(format!("unknown export profile `{name}`")))?;
    let rules = profile.content_transform_rules.unwrap_or_default();

    rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            Ok(ExportProfileRuleListEntry {
                index: index + 1,
                rule: serde_json::to_value(rule).map_err(AppError::operation)?,
            })
        })
        .collect()
}

pub fn apply_export_profile_rule_add(
    paths: &VaultPaths,
    name: &str,
    before: Option<usize>,
    request: &ExportProfileRuleRequest,
    dry_run: bool,
) -> Result<ExportProfileRuleWriteReport, AppError> {
    validate_export_profile_name(name)?;
    let mut profile = load_shared_export_profile(paths, name)?
        .ok_or_else(|| AppError::operation(format!("unknown export profile `{name}`")))?;
    let format = require_export_profile_format(name, &profile)?;
    if !export_profile_supports_content_transforms(format) {
        return Err(AppError::operation(format!(
            "export profile `{name}` only supports `content_transforms` for markdown, json, epub, and zip exports"
        )));
    }

    let rule = build_export_profile_rule(request)?;
    let rules = profile.content_transform_rules.get_or_insert_with(Vec::new);
    let insert_at = resolve_export_profile_rule_insert_before(rules.len(), before)?;
    rules.insert(insert_at, rule.clone());
    validate_export_profile_config(name, &profile)?;
    let persisted = persist_shared_export_profile(paths, name, &profile, dry_run)?;

    build_export_profile_rule_write_report(
        paths,
        name,
        &profile,
        if persisted.updated {
            ExportProfileRuleWriteAction::Added
        } else {
            ExportProfileRuleWriteAction::Unchanged
        },
        Some(insert_at + 1),
        None,
        Some(rule),
        dry_run,
        persisted.diagnostics,
        persisted.changed_paths,
    )
}

pub fn apply_export_profile_rule_update(
    paths: &VaultPaths,
    name: &str,
    index: usize,
    request: &ExportProfileRuleRequest,
    dry_run: bool,
) -> Result<ExportProfileRuleWriteReport, AppError> {
    validate_export_profile_name(name)?;
    let mut profile = load_shared_export_profile(paths, name)?
        .ok_or_else(|| AppError::operation(format!("unknown export profile `{name}`")))?;
    let format = require_export_profile_format(name, &profile)?;
    if !export_profile_supports_content_transforms(format) {
        return Err(AppError::operation(format!(
            "export profile `{name}` only supports `content_transforms` for markdown, json, epub, and zip exports"
        )));
    }

    let rule = build_export_profile_rule(request)?;
    let rules = profile.content_transform_rules.get_or_insert_with(Vec::new);
    let rule_index = require_export_profile_rule_index(rules, index)?;
    let changed = rules.get(rule_index) != Some(&rule);
    rules[rule_index] = rule.clone();
    validate_export_profile_config(name, &profile)?;
    let persisted = persist_shared_export_profile(paths, name, &profile, dry_run)?;

    build_export_profile_rule_write_report(
        paths,
        name,
        &profile,
        if changed && persisted.updated {
            ExportProfileRuleWriteAction::Updated
        } else {
            ExportProfileRuleWriteAction::Unchanged
        },
        Some(rule_index + 1),
        None,
        Some(rule),
        dry_run,
        persisted.diagnostics,
        persisted.changed_paths,
    )
}

pub fn apply_export_profile_rule_delete(
    paths: &VaultPaths,
    name: &str,
    index: usize,
    dry_run: bool,
) -> Result<ExportProfileRuleWriteReport, AppError> {
    validate_export_profile_name(name)?;
    let mut profile = load_shared_export_profile(paths, name)?
        .ok_or_else(|| AppError::operation(format!("unknown export profile `{name}`")))?;
    let rules = profile.content_transform_rules.get_or_insert_with(Vec::new);
    let rule_index = require_export_profile_rule_index(rules, index)?;
    let removed_rule = rules.remove(rule_index);
    normalize_export_profile_rules(&mut profile);
    validate_export_profile_config(name, &profile)?;
    let persisted = persist_shared_export_profile(paths, name, &profile, dry_run)?;

    build_export_profile_rule_write_report(
        paths,
        name,
        &profile,
        ExportProfileRuleWriteAction::Deleted,
        Some(rule_index + 1),
        None,
        Some(removed_rule),
        dry_run,
        persisted.diagnostics,
        persisted.changed_paths,
    )
}

pub fn apply_export_profile_rule_move(
    paths: &VaultPaths,
    name: &str,
    request: ExportProfileRuleMoveRequest,
    dry_run: bool,
) -> Result<ExportProfileRuleWriteReport, AppError> {
    validate_export_profile_name(name)?;
    if request.before.is_none() && request.after.is_none() && !request.last {
        return Err(AppError::operation(
            "export profile rule move requires --before, --after, or --last",
        ));
    }

    let mut profile = load_shared_export_profile(paths, name)?
        .ok_or_else(|| AppError::operation(format!("unknown export profile `{name}`")))?;
    let rules = profile.content_transform_rules.get_or_insert_with(Vec::new);
    let source_index = require_export_profile_rule_index(rules, request.index)?;
    let original_len = rules.len();
    let mut destination = if let Some(before_index) = request.before {
        if before_index == 0 || before_index > original_len {
            return Err(AppError::operation(format!(
                "content_transforms destination index {before_index} is out of range; expected 1..={original_len}"
            )));
        }
        before_index - 1
    } else if let Some(after_index) = request.after {
        if after_index == 0 || after_index > original_len {
            return Err(AppError::operation(format!(
                "content_transforms destination index {after_index} is out of range; expected 1..={original_len}"
            )));
        }
        after_index
    } else {
        original_len
    };

    let rule = rules.remove(source_index);
    if destination > source_index {
        destination -= 1;
    }
    if destination > rules.len() {
        destination = rules.len();
    }
    let changed = destination != source_index;
    rules.insert(destination, rule.clone());
    validate_export_profile_config(name, &profile)?;
    let persisted = persist_shared_export_profile(paths, name, &profile, dry_run)?;

    build_export_profile_rule_write_report(
        paths,
        name,
        &profile,
        if changed && persisted.updated {
            ExportProfileRuleWriteAction::Moved
        } else {
            ExportProfileRuleWriteAction::Unchanged
        },
        Some(destination + 1),
        Some(source_index + 1),
        Some(rule),
        dry_run,
        persisted.diagnostics,
        persisted.changed_paths,
    )
}

pub fn build_content_transform_rules(
    exclude_callouts: &[String],
    exclude_headings: &[String],
    exclude_frontmatter_keys: &[String],
    exclude_inline_fields: &[String],
    replacement_rules: &[String],
) -> Result<Option<Vec<ContentTransformRuleConfig>>, AppError> {
    if exclude_callouts.is_empty()
        && exclude_headings.is_empty()
        && exclude_frontmatter_keys.is_empty()
        && exclude_inline_fields.is_empty()
        && replacement_rules.is_empty()
    {
        return Ok(None);
    }

    build_content_transform_rule(
        None,
        None,
        exclude_callouts,
        exclude_headings,
        exclude_frontmatter_keys,
        exclude_inline_fields,
        replacement_rules,
    )
    .map(|rule| Some(vec![rule]))
}

#[must_use]
pub fn export_profile_format_label(format: ExportProfileFormat) -> &'static str {
    match format {
        ExportProfileFormat::Markdown => "markdown",
        ExportProfileFormat::Json => "json",
        ExportProfileFormat::Csv => "csv",
        ExportProfileFormat::Graph => "graph",
        ExportProfileFormat::Epub => "epub",
        ExportProfileFormat::Zip => "zip",
        ExportProfileFormat::Sqlite => "sqlite",
        ExportProfileFormat::SearchIndex => "search-index",
    }
}

#[must_use]
pub fn resolve_export_profile_output_path(paths: &VaultPaths, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        paths.vault_root().join(path)
    }
}

pub fn require_export_profile_format(
    name: &str,
    profile: &ExportProfileConfig,
) -> Result<ExportProfileFormat, AppError> {
    profile
        .format
        .ok_or_else(|| AppError::operation(format!("export profile `{name}` is missing `format`")))
}

pub fn require_export_profile_path(
    paths: &VaultPaths,
    name: &str,
    profile: &ExportProfileConfig,
) -> Result<PathBuf, AppError> {
    profile.path.as_deref().map_or_else(
        || {
            Err(AppError::operation(format!(
                "export profile `{name}` is missing `path`"
            )))
        },
        |path| Ok(resolve_export_profile_output_path(paths, path)),
    )
}

pub fn export_profile_query_args<'a>(
    name: &str,
    format: ExportProfileFormat,
    profile: &'a ExportProfileConfig,
) -> Result<(Option<&'a str>, Option<&'a str>), AppError> {
    let query = profile.query.as_deref();
    let query_json = profile.query_json.as_deref();
    let has_query = query.is_some() || query_json.is_some();
    let needs_query = export_profile_requires_query(format);

    if needs_query && !has_query {
        return Err(AppError::operation(format!(
            "export profile `{name}` requires `query` or `query_json` for {} exports",
            export_profile_format_label(format)
        )));
    }
    if !needs_query && has_query {
        return Err(AppError::operation(format!(
            "export profile `{name}` does not use `query` or `query_json` for {} exports",
            export_profile_format_label(format)
        )));
    }

    Ok((query, query_json))
}

pub fn validate_export_profile_config(
    name: &str,
    profile: &ExportProfileConfig,
) -> Result<(), AppError> {
    let format = profile.format.ok_or_else(|| {
        AppError::operation(format!("export profile `{name}` is missing `format`"))
    })?;
    let has_query = profile.query.is_some() || profile.query_json.is_some();

    if export_profile_requires_query(format) && !has_query {
        return Err(AppError::operation(format!(
            "export profile `{name}` requires `query` or `query_json` for {} exports",
            export_profile_format_label(format)
        )));
    }
    if !export_profile_requires_query(format) && has_query {
        return Err(AppError::operation(format!(
            "export profile `{name}` does not use `query` or `query_json` for {} exports",
            export_profile_format_label(format)
        )));
    }
    if !matches!(
        format,
        ExportProfileFormat::Markdown | ExportProfileFormat::Epub
    ) && profile.title.is_some()
    {
        return Err(AppError::operation(format!(
            "export profile `{name}` only supports `title` for markdown and epub exports"
        )));
    }
    if !matches!(format, ExportProfileFormat::Epub) {
        if profile.author.is_some() {
            return Err(AppError::operation(format!(
                "export profile `{name}` only supports `author` for epub exports"
            )));
        }
        if profile.toc.is_some() {
            return Err(AppError::operation(format!(
                "export profile `{name}` only supports `toc` for epub exports"
            )));
        }
        if profile.backlinks.is_some() {
            return Err(AppError::operation(format!(
                "export profile `{name}` only supports `backlinks` for epub exports"
            )));
        }
        if profile.frontmatter.is_some() {
            return Err(AppError::operation(format!(
                "export profile `{name}` only supports `frontmatter` for epub exports"
            )));
        }
    }
    if !matches!(
        format,
        ExportProfileFormat::Json | ExportProfileFormat::SearchIndex
    ) && profile.pretty.is_some()
    {
        return Err(AppError::operation(format!(
            "export profile `{name}` only supports `pretty` for json and search-index exports"
        )));
    }
    if !matches!(format, ExportProfileFormat::Graph) && profile.graph_format.is_some() {
        return Err(AppError::operation(format!(
            "export profile `{name}` only supports `graph_format` for graph exports"
        )));
    }
    if let Some(content_transform_rules) = profile.content_transform_rules.as_ref() {
        for (index, rule) in content_transform_rules.iter().enumerate() {
            if rule.query.is_some() && rule.query_json.is_some() {
                return Err(AppError::operation(format!(
                    "content_transforms rule {} in export profile `{name}` must set only one of `query` or `query_json`",
                    index + 1
                )));
            }
        }
    }
    if !export_profile_supports_content_transforms(format)
        && profile
            .content_transform_rules
            .as_ref()
            .is_some_and(|rules| content_transform_rules_have_effective_transforms(rules))
    {
        return Err(AppError::operation(format!(
            "export profile `{name}` only supports `content_transforms` for markdown, json, epub, and zip exports"
        )));
    }
    if let Some(content_transform_rules) = profile.content_transform_rules.as_ref() {
        for (rule_index, rule) in content_transform_rules.iter().enumerate() {
            for (replace_index, replacement_rule) in rule.transforms.replace.iter().enumerate() {
                validate_content_replacement_rule(
                    replacement_rule,
                    &format!(
                        "content_transforms rule {} replace entry {} in export profile `{name}`",
                        rule_index + 1,
                        replace_index + 1
                    ),
                )?;
            }
        }
    }

    Ok(())
}

fn validate_export_profile_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() {
        return Err(AppError::operation("export profile name cannot be empty"));
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(AppError::operation(
            "export profile names may only contain ASCII letters, numbers, `-`, and `_`",
        ));
    }
    Ok(())
}

fn validate_content_replacement_rule(
    rule: &ContentReplacementRuleConfig,
    context: &str,
) -> Result<(), AppError> {
    if rule.pattern.trim().is_empty() {
        return Err(AppError::operation(format!(
            "{context} must set a non-empty `pattern`"
        )));
    }
    if rule.regex {
        Regex::new(&rule.pattern).map_err(|error| {
            AppError::operation(format!(
                "{context} has invalid regex pattern `{}`: {error}",
                rule.pattern
            ))
        })?;
    }
    Ok(())
}

fn parse_content_replacement_rules(
    values: &[String],
) -> Result<Vec<ContentReplacementRuleConfig>, AppError> {
    let chunks = values.chunks_exact(3);
    if !chunks.remainder().is_empty() {
        return Err(AppError::operation(
            "content transform replacement rules must be provided as MODE PATTERN REPLACEMENT triples",
        ));
    }

    let mut rules = Vec::new();
    for (index, chunk) in values.chunks_exact(3).enumerate() {
        let mode = chunk[0].trim().to_ascii_lowercase();
        let regex = match mode.as_str() {
            "literal" => false,
            "regex" => true,
            _ => {
                return Err(AppError::operation(format!(
                    "content transform replacement rule {} must use mode `literal` or `regex`, got `{}`",
                    index + 1,
                    chunk[0]
                )));
            }
        };
        let rule = ContentReplacementRuleConfig {
            pattern: chunk[1].clone(),
            replacement: chunk[2].clone(),
            regex,
        };
        validate_content_replacement_rule(
            &rule,
            &format!("content transform replacement rule {}", index + 1),
        )?;
        rules.push(rule);
    }
    Ok(rules)
}

fn build_content_transform_rule(
    query: Option<&str>,
    query_json: Option<&str>,
    exclude_callouts: &[String],
    exclude_headings: &[String],
    exclude_frontmatter_keys: &[String],
    exclude_inline_fields: &[String],
    replacement_rules: &[String],
) -> Result<ContentTransformRuleConfig, AppError> {
    if query.is_some() && query_json.is_some() {
        return Err(AppError::operation(
            "content transform rule must set only one of `query` or `query_json`",
        ));
    }

    let exclude_callouts = exclude_callouts
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let exclude_headings = exclude_headings
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let exclude_frontmatter_keys = exclude_frontmatter_keys
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let exclude_inline_fields = exclude_inline_fields
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let replace = parse_content_replacement_rules(replacement_rules)?;

    if exclude_callouts.is_empty()
        && exclude_headings.is_empty()
        && exclude_frontmatter_keys.is_empty()
        && exclude_inline_fields.is_empty()
        && replace.is_empty()
    {
        return Err(AppError::operation(
            "content transform rule must include at least one transform",
        ));
    }

    Ok(ContentTransformRuleConfig {
        query: query.map(ToOwned::to_owned),
        query_json: query_json.map(ToOwned::to_owned),
        transforms: ContentTransformConfig {
            exclude_callouts,
            exclude_headings,
            exclude_frontmatter_keys,
            exclude_inline_fields,
            replace,
        },
    })
}

fn build_export_profile_config(request: &ExportProfileCreateRequest) -> ExportProfileConfig {
    ExportProfileConfig {
        format: Some(request.format),
        query: request.query.clone(),
        query_json: request.query_json.clone(),
        path: Some(request.path.clone()),
        title: request.title.clone(),
        author: request.author.clone(),
        toc: request.toc,
        backlinks: request.backlinks.then_some(true),
        frontmatter: request.frontmatter.then_some(true),
        pretty: request.pretty.then_some(true),
        graph_format: request.graph_format,
        content_transform_rules: None,
    }
}

fn build_export_profile_rule(
    request: &ExportProfileRuleRequest,
) -> Result<ContentTransformRuleConfig, AppError> {
    build_content_transform_rule(
        request.query.as_deref(),
        request.query_json.as_deref(),
        &request.exclude_callouts,
        &request.exclude_headings,
        &request.exclude_frontmatter_keys,
        &request.exclude_inline_fields,
        &request.replacement_rules,
    )
}

fn apply_updated_value<T: Copy + PartialEq>(
    current: &mut Option<T>,
    update: &ConfigValueUpdate<T>,
) -> bool {
    let next = match update {
        ConfigValueUpdate::Keep => return false,
        ConfigValueUpdate::Set(value) => Some(*value),
        ConfigValueUpdate::Clear => None,
    };
    if *current == next {
        false
    } else {
        *current = next;
        true
    }
}

fn apply_updated_string(current: &mut Option<String>, update: &ConfigValueUpdate<String>) -> bool {
    let next = match update {
        ConfigValueUpdate::Keep => return false,
        ConfigValueUpdate::Set(value) => Some(value.clone()),
        ConfigValueUpdate::Clear => None,
    };
    if *current == next {
        false
    } else {
        *current = next;
        true
    }
}

fn apply_updated_path(current: &mut Option<PathBuf>, update: &ConfigValueUpdate<PathBuf>) -> bool {
    let next = match update {
        ConfigValueUpdate::Keep => return false,
        ConfigValueUpdate::Set(value) => Some(value.clone()),
        ConfigValueUpdate::Clear => None,
    };
    if *current == next {
        false
    } else {
        *current = next;
        true
    }
}

fn apply_updated_flag(current: &mut Option<bool>, update: BoolConfigUpdate) -> bool {
    let next = match update {
        BoolConfigUpdate::Keep => return false,
        BoolConfigUpdate::SetTrue => Some(true),
        BoolConfigUpdate::Clear => None,
    };
    if *current == next {
        false
    } else {
        *current = next;
        true
    }
}

fn apply_export_profile_settings(
    profile: &mut ExportProfileConfig,
    request: &ExportProfileSetRequest,
) -> bool {
    let mut changed = false;

    if let Some(format) = request.format {
        if profile.format != Some(format) {
            profile.format = Some(format);
            changed = true;
        }
    }

    if request.clear_query {
        if profile.query.take().is_some() || profile.query_json.take().is_some() {
            changed = true;
        }
    } else if let Some(query) = request.query.as_deref() {
        if profile.query != Some(query.to_string()) {
            profile.query = Some(query.to_string());
            changed = true;
        }
        if profile.query_json.take().is_some() {
            changed = true;
        }
    } else if let Some(query_json) = request.query_json.as_deref() {
        if profile.query_json != Some(query_json.to_string()) {
            profile.query_json = Some(query_json.to_string());
            changed = true;
        }
        if profile.query.take().is_some() {
            changed = true;
        }
    }

    changed |= apply_updated_path(&mut profile.path, &request.path);
    changed |= apply_updated_string(&mut profile.title, &request.title);
    changed |= apply_updated_string(&mut profile.author, &request.author);
    changed |= apply_updated_value(&mut profile.toc, &request.toc);
    changed |= apply_updated_flag(&mut profile.backlinks, request.backlinks);
    changed |= apply_updated_flag(&mut profile.frontmatter, request.frontmatter);
    changed |= apply_updated_flag(&mut profile.pretty, request.pretty);
    changed |= apply_updated_value(&mut profile.graph_format, &request.graph_format);

    changed
}

fn export_profile_set_request_has_changes(request: &ExportProfileSetRequest) -> bool {
    request.format.is_some()
        || request.query.is_some()
        || request.query_json.is_some()
        || request.clear_query
        || request.path.has_change()
        || request.title.has_change()
        || request.author.has_change()
        || request.toc.has_change()
        || request.backlinks.has_change()
        || request.frontmatter.has_change()
        || request.pretty.has_change()
        || request.graph_format.has_change()
}

fn export_profile_requires_query(format: ExportProfileFormat) -> bool {
    matches!(
        format,
        ExportProfileFormat::Markdown
            | ExportProfileFormat::Json
            | ExportProfileFormat::Csv
            | ExportProfileFormat::Epub
            | ExportProfileFormat::Zip
            | ExportProfileFormat::Sqlite
    )
}

fn export_profile_supports_content_transforms(format: ExportProfileFormat) -> bool {
    matches!(
        format,
        ExportProfileFormat::Markdown
            | ExportProfileFormat::Json
            | ExportProfileFormat::Epub
            | ExportProfileFormat::Zip
    )
}

fn content_transform_rules_have_effective_transforms(rules: &[ContentTransformRuleConfig]) -> bool {
    rules.iter().any(|rule| !rule.is_empty())
}

fn render_export_profile_section_toml(
    name: &str,
    profile: &ExportProfileConfig,
) -> Result<TomlValue, AppError> {
    let value = TomlValue::try_from(profile).map_err(AppError::operation)?;
    Ok(wrap_config_section_toml(
        &format!("export.profiles.{name}"),
        value,
    ))
}

fn render_export_profile_section_toml_string(
    name: &str,
    profile: &ExportProfileConfig,
) -> Result<String, AppError> {
    toml::to_string_pretty(&render_export_profile_section_toml(name, profile)?)
        .map_err(AppError::operation)
}

#[allow(clippy::too_many_arguments)]
fn build_export_profile_write_report(
    paths: &VaultPaths,
    name: &str,
    profile: &ExportProfileConfig,
    action: ExportProfileWriteAction,
    created_config: bool,
    dry_run: bool,
    diagnostics: Vec<ConfigDiagnostic>,
    changed_paths: Vec<String>,
) -> Result<ExportProfileWriteReport, AppError> {
    Ok(ExportProfileWriteReport {
        name: name.to_string(),
        profile: serde_json::to_value(profile).map_err(AppError::operation)?,
        config_path: relativize_path(paths, paths.config_file()),
        action,
        created_config,
        dry_run,
        diagnostics,
        changed_paths,
        rendered_toml: render_export_profile_section_toml_string(name, profile)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_export_profile_rule_write_report(
    paths: &VaultPaths,
    name: &str,
    profile: &ExportProfileConfig,
    action: ExportProfileRuleWriteAction,
    rule_index: Option<usize>,
    previous_rule_index: Option<usize>,
    rule: Option<ContentTransformRuleConfig>,
    dry_run: bool,
    diagnostics: Vec<ConfigDiagnostic>,
    changed_paths: Vec<String>,
) -> Result<ExportProfileRuleWriteReport, AppError> {
    Ok(ExportProfileRuleWriteReport {
        name: name.to_string(),
        profile: serde_json::to_value(profile).map_err(AppError::operation)?,
        config_path: relativize_path(paths, paths.config_file()),
        action,
        rule_index,
        previous_rule_index,
        rule: rule
            .map(|rule| serde_json::to_value(rule).map_err(AppError::operation))
            .transpose()?,
        dry_run,
        diagnostics,
        changed_paths,
        rendered_toml: render_export_profile_section_toml_string(name, profile)?,
    })
}

fn shared_export_profile_storage_path(name: &str) -> [&str; 3] {
    ["export", "profiles", name]
}

fn load_shared_export_profile(
    paths: &VaultPaths,
    name: &str,
) -> Result<Option<ExportProfileConfig>, AppError> {
    let config_value = app_config::load_config_file_toml(paths.config_file())?;
    let storage_path = shared_export_profile_storage_path(name);
    if !app_config::config_toml_path_exists(&config_value, &storage_path) {
        return Ok(None);
    }

    let mut current = &config_value;
    for segment in storage_path {
        current = current.get(segment).ok_or_else(|| {
            AppError::operation(format!(
                "failed to read export profile `{name}` from config"
            ))
        })?;
    }

    current
        .clone()
        .try_into()
        .map(Some)
        .map_err(AppError::operation)
}

fn persist_shared_export_profile(
    paths: &VaultPaths,
    name: &str,
    profile: &ExportProfileConfig,
    dry_run: bool,
) -> Result<ExportProfilePersistOutcome, AppError> {
    let config_path = paths.config_file().to_path_buf();
    let created_config = !config_path.exists();
    let had_gitignore = paths.gitignore_file().exists();
    let existing_contents = fs::read_to_string(&config_path).ok();
    let mut config_value = app_config::load_config_file_toml(&config_path)?;
    let storage_path = shared_export_profile_storage_path(name);
    let existing_profile = app_config::config_toml_path_exists(&config_value, &storage_path);

    let profile_toml = TomlValue::try_from(profile).map_err(AppError::operation)?;
    app_config::set_config_toml_value(&mut config_value, &storage_path, profile_toml)?;
    let rendered = toml::to_string_pretty(&config_value).map_err(AppError::operation)?;
    validate_vulcan_overrides_toml(&rendered).map_err(AppError::operation)?;
    let updated = existing_contents.as_deref() != Some(rendered.as_str());

    let changed_paths = if !dry_run && updated {
        ensure_vulcan_dir(paths).map_err(AppError::operation)?;
        fs::write(&config_path, rendered).map_err(AppError::operation)?;
        let mut changed_paths = vec![relativize_path_string(paths, &config_path)];
        let gitignore_path = paths.gitignore_file();
        if !had_gitignore && gitignore_path.exists() {
            changed_paths.push(relativize_path_string(paths, &gitignore_path));
        }
        changed_paths
    } else {
        Vec::new()
    };

    Ok(ExportProfilePersistOutcome {
        created_config,
        existing_profile,
        updated,
        diagnostics: normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics),
        changed_paths,
    })
}

fn normalize_export_profile_rules(profile: &mut ExportProfileConfig) {
    if profile
        .content_transform_rules
        .as_ref()
        .is_some_and(Vec::is_empty)
    {
        profile.content_transform_rules = None;
    }
}

fn require_export_profile_rule_index(
    rules: &[ContentTransformRuleConfig],
    index: usize,
) -> Result<usize, AppError> {
    if index == 0 || index > rules.len() {
        return Err(AppError::operation(format!(
            "content_transforms rule index {} is out of range; expected 1..={}",
            index,
            rules.len()
        )));
    }
    Ok(index - 1)
}

fn resolve_export_profile_rule_insert_before(
    existing_len: usize,
    before: Option<usize>,
) -> Result<usize, AppError> {
    match before {
        None => Ok(existing_len),
        Some(index) if (1..=existing_len + 1).contains(&index) => Ok(index - 1),
        Some(index) => Err(AppError::operation(format!(
            "content_transforms insertion index {} is out of range; expected 1..={}",
            index,
            existing_len + 1
        ))),
    }
}

fn normalize_config_diagnostics(
    paths: &VaultPaths,
    diagnostics: &[ConfigDiagnostic],
) -> Vec<ConfigDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| ConfigDiagnostic {
            path: relativize_path(paths, &diagnostic.path),
            message: diagnostic.message.clone(),
        })
        .collect()
}

fn relativize_path(paths: &VaultPaths, path: &Path) -> PathBuf {
    let relative_or_original = path
        .strip_prefix(paths.vault_root())
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf);
    PathBuf::from(relative_or_original.to_string_lossy().replace('\\', "/"))
}

fn relativize_path_string(paths: &VaultPaths, path: &Path) -> String {
    relativize_path(paths, path).display().to_string()
}

fn wrap_config_section_toml(section: &str, value: TomlValue) -> TomlValue {
    let mut wrapped = value;
    for part in section.split('.').rev() {
        let mut table = toml::map::Map::new();
        table.insert(part.to_string(), wrapped);
        wrapped = TomlValue::Table(table);
    }
    wrapped
}

#[derive(Debug, Clone)]
pub struct ExportedNoteDocument {
    pub note: NoteRecord,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportLinkRecord {
    pub source_document_path: String,
    pub raw_text: String,
    pub link_kind: String,
    pub display_text: Option<String>,
    pub target_path_candidate: Option<String>,
    pub target_heading: Option<String>,
    pub target_block: Option<String>,
    pub resolved_target_path: Option<String>,
    pub origin_context: String,
    pub byte_offset: i64,
    #[serde(skip_serializing)]
    pub resolved_target_extension: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SqliteExportSummary {
    pub path: String,
    pub result_count: usize,
    pub link_count: usize,
    pub tag_count: usize,
    pub task_count: usize,
}

fn prepare_export_output_path(output_path: &Path) -> Result<(), AppError> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    if output_path.exists() {
        fs::remove_file(output_path).map_err(AppError::operation)?;
    }
    Ok(())
}

fn initialize_sqlite_export(connection: &Connection) -> Result<(), AppError> {
    connection
        .execute_batch(
            "
            PRAGMA user_version = 1;

            CREATE TABLE meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE notes (
                document_path TEXT PRIMARY KEY,
                file_name TEXT NOT NULL,
                file_ext TEXT NOT NULL,
                file_mtime INTEGER NOT NULL,
                file_size INTEGER NOT NULL,
                tags_json TEXT NOT NULL,
                aliases_json TEXT NOT NULL,
                frontmatter_json TEXT NOT NULL,
                properties_json TEXT NOT NULL,
                content TEXT NOT NULL
            );

            CREATE TABLE links (
                source_document_path TEXT NOT NULL,
                raw_text TEXT NOT NULL,
                link_kind TEXT NOT NULL,
                display_text TEXT,
                target_path_candidate TEXT,
                target_heading TEXT,
                target_block TEXT,
                resolved_target_path TEXT,
                origin_context TEXT NOT NULL,
                byte_offset INTEGER NOT NULL
            );

            CREATE TABLE tags (
                document_path TEXT NOT NULL,
                tag_text TEXT NOT NULL
            );

            CREATE TABLE tasks (
                task_id TEXT PRIMARY KEY,
                document_path TEXT NOT NULL,
                task_source TEXT NOT NULL,
                text TEXT NOT NULL,
                status_char TEXT NOT NULL,
                status_name TEXT NOT NULL,
                status_type TEXT NOT NULL,
                line_number INTEGER NOT NULL,
                byte_offset INTEGER NOT NULL,
                section_heading TEXT,
                properties_json TEXT NOT NULL
            );

            CREATE INDEX idx_links_source_document_path ON links(source_document_path);
            CREATE INDEX idx_tags_document_path ON tags(document_path);
            CREATE INDEX idx_tasks_document_path ON tasks(document_path);
            ",
        )
        .map_err(AppError::operation)
}

fn insert_sqlite_export_meta(
    connection: &Connection,
    report: &QueryReport,
    result_count: usize,
) -> Result<(), AppError> {
    let query_json = serde_json::to_string(&report.query).map_err(AppError::operation)?;
    let timestamp = TemplateTimestamp::current().default_strings().datetime;
    connection
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2), (?3, ?4), (?5, ?6)",
            rusqlite::params![
                "query_json",
                query_json,
                "result_count",
                result_count.to_string(),
                "generated_at",
                timestamp
            ],
        )
        .map_err(AppError::operation)?;
    Ok(())
}

fn insert_sqlite_export_notes(
    transaction: &rusqlite::Transaction<'_>,
    notes: &[ExportedNoteDocument],
) -> Result<(usize, usize), AppError> {
    let mut tag_count = 0;
    let mut task_count = 0;

    for note in notes {
        transaction
            .execute(
                "INSERT INTO notes (
                    document_path, file_name, file_ext, file_mtime, file_size,
                    tags_json, aliases_json, frontmatter_json, properties_json, content
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    &note.note.document_path,
                    &note.note.file_name,
                    &note.note.file_ext,
                    note.note.file_mtime,
                    note.note.file_size,
                    serde_json::to_string(&note.note.tags).map_err(AppError::operation)?,
                    serde_json::to_string(&note.note.aliases).map_err(AppError::operation)?,
                    serde_json::to_string(&note.note.frontmatter).map_err(AppError::operation)?,
                    serde_json::to_string(&note.note.properties).map_err(AppError::operation)?,
                    &note.content,
                ],
            )
            .map_err(AppError::operation)?;

        for tag in &note.note.tags {
            transaction
                .execute(
                    "INSERT INTO tags (document_path, tag_text) VALUES (?1, ?2)",
                    rusqlite::params![&note.note.document_path, tag],
                )
                .map_err(AppError::operation)?;
            tag_count += 1;
        }

        for task in &note.note.tasks {
            let task_source = task
                .properties
                .get("taskSource")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("inline");
            transaction
                .execute(
                    "INSERT INTO tasks (
                        task_id, document_path, task_source, text, status_char, status_name,
                        status_type, line_number, byte_offset, section_heading, properties_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    rusqlite::params![
                        &task.id,
                        &note.note.document_path,
                        task_source,
                        &task.text,
                        &task.status_char,
                        &task.status_name,
                        &task.status_type,
                        task.line_number,
                        task.byte_offset,
                        &task.section_heading,
                        serde_json::to_string(&task.properties).map_err(AppError::operation)?,
                    ],
                )
                .map_err(AppError::operation)?;
            task_count += 1;
        }
    }

    Ok((tag_count, task_count))
}

fn insert_sqlite_export_links(
    transaction: &rusqlite::Transaction<'_>,
    links: &[ExportLinkRecord],
) -> Result<(), AppError> {
    for link in links {
        transaction
            .execute(
                "INSERT INTO links (
                    source_document_path, raw_text, link_kind, display_text, target_path_candidate,
                    target_heading, target_block, resolved_target_path, origin_context, byte_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    &link.source_document_path,
                    &link.raw_text,
                    &link.link_kind,
                    &link.display_text,
                    &link.target_path_candidate,
                    &link.target_heading,
                    &link.target_block,
                    &link.resolved_target_path,
                    &link.origin_context,
                    link.byte_offset,
                ],
            )
            .map_err(AppError::operation)?;
    }
    Ok(())
}

pub fn write_sqlite_export(
    output_path: &Path,
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
) -> Result<SqliteExportSummary, AppError> {
    prepare_export_output_path(output_path)?;
    let mut connection = Connection::open(output_path).map_err(AppError::operation)?;
    initialize_sqlite_export(&connection)?;
    insert_sqlite_export_meta(&connection, report, notes.len())?;
    let transaction = connection.transaction().map_err(AppError::operation)?;
    let (tag_count, task_count) = insert_sqlite_export_notes(&transaction, notes)?;
    insert_sqlite_export_links(&transaction, links)?;
    transaction.commit().map_err(AppError::operation)?;

    Ok(SqliteExportSummary {
        path: output_path.display().to_string(),
        result_count: notes.len(),
        link_count: links.len(),
        tag_count,
        task_count,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        apply_export_profile_create, apply_export_profile_delete, apply_export_profile_rule_add,
        apply_export_profile_rule_delete, apply_export_profile_rule_move,
        apply_export_profile_rule_update, apply_export_profile_set, build_content_transform_rules,
        build_epub_nav_nodes, build_epub_tag_targets, build_export_profile_list,
        build_export_profile_rule_list, build_export_profile_show_report,
        collect_export_attachment_paths, execute_export_query, inject_epub_heading_ids,
        load_export_links, load_exported_notes, prepare_export_data, render_csv_export_payload,
        render_epub_nav_document, render_json_export_payload, render_markdown_export_payload,
        rewrite_epub_link_destination, write_epub_export, write_sqlite_export, write_zip_export,
        BoolConfigUpdate, ConfigValueUpdate, EpubChapter, EpubExportOptions, EpubHeading,
        EpubRenderCallbacks, ExportLinkRecord, ExportProfileCreateRequest, ExportProfileFormat,
        ExportProfileRuleMoveRequest, ExportProfileRuleRequest, ExportProfileRuleWriteAction,
        ExportProfileSetRequest, ExportProfileWriteAction, ExportedNoteDocument,
    };
    use serde_json::{Map, Value};
    use std::collections::HashMap;
    use std::fs;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;
    use vulcan_core::config::ExportEpubTocStyleConfig;
    use vulcan_core::properties::NoteTaskRecord;
    use vulcan_core::{
        scan_vault, EvaluatedInlineExpression, NoteRecord, QueryAst, QueryProjection, QueryReport,
        QuerySource, ScanMode, VaultPaths,
    };
    use zip::ZipArchive;

    fn export_paths() -> (tempfile::TempDir, VaultPaths) {
        let temp_dir = tempdir().expect("temp dir");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir");
        let paths = VaultPaths::new(&vault_root);
        (temp_dir, paths)
    }

    fn create_json_profile_request() -> ExportProfileCreateRequest {
        ExportProfileCreateRequest {
            format: ExportProfileFormat::Json,
            query: Some("from notes".to_string()),
            query_json: None,
            path: PathBuf::from("exports/public.json"),
            title: None,
            author: None,
            toc: None,
            backlinks: false,
            frontmatter: false,
            pretty: true,
            graph_format: None,
        }
    }

    fn config_contents(path: &Path) -> String {
        fs::read_to_string(path.join(".vulcan/config.toml")).expect("config contents")
    }

    fn build_export_transform_vault() -> (tempfile::TempDir, VaultPaths) {
        let temp_dir = tempdir().expect("temp dir");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir");
        fs::create_dir_all(vault_root.join("People")).expect("people dir");
        fs::create_dir_all(vault_root.join("assets")).expect("assets dir");
        fs::write(
            vault_root.join("Home.md"),
            concat!(
                "# Home\n\n",
                "Visible note.\n\n",
                "> [!secret gm]- Internal\n",
                "> Hidden [[People/Bob]].\n",
                "> ![[assets/secret.png]]\n\n",
                "![[assets/public.png]]\n",
            ),
        )
        .expect("home note");
        fs::write(vault_root.join("People/Bob.md"), "# Bob\n").expect("bob note");
        fs::write(vault_root.join("assets/public.png"), b"public").expect("public asset");
        fs::write(vault_root.join("assets/secret.png"), b"secret").expect("secret asset");
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        (temp_dir, paths)
    }

    fn test_epub_render_inline_value(value: &Value) -> String {
        value
            .as_str()
            .map_or_else(|| value.to_string(), ToOwned::to_owned)
    }

    #[test]
    fn export_profile_create_list_and_show_reports_share_app_layer_logic() {
        let (_temp_dir, paths) = export_paths();
        let report = apply_export_profile_create(
            &paths,
            "public_json",
            &create_json_profile_request(),
            false,
            false,
        )
        .expect("create profile");

        assert_eq!(report.action, ExportProfileWriteAction::Created);
        assert_eq!(
            report.changed_paths,
            vec![
                ".vulcan/config.toml".to_string(),
                ".vulcan/.gitignore".to_string()
            ]
        );
        assert!(report
            .rendered_toml
            .contains("[export.profiles.public_json]"));

        let listed = build_export_profile_list(&paths);
        let expected_resolved = paths
            .vault_root()
            .join("exports/public.json")
            .display()
            .to_string();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "public_json");
        assert_eq!(listed[0].format.as_deref(), Some("json"));
        assert_eq!(listed[0].path.as_deref(), Some("exports/public.json"));
        assert_eq!(
            listed[0].resolved_path.as_deref(),
            Some(expected_resolved.as_str())
        );

        let show = build_export_profile_show_report(&paths, "public_json").expect("show report");
        assert_eq!(show.profile["pretty"], Value::Bool(true));
        assert!(show.rendered_toml.contains("pretty = true"));
        assert!(config_contents(paths.vault_root()).contains("format = \"json\""));
    }

    #[test]
    fn export_profile_set_rewrites_profile_fields_in_shared_config() {
        let (_temp_dir, paths) = export_paths();
        apply_export_profile_create(
            &paths,
            "docs",
            &ExportProfileCreateRequest {
                format: ExportProfileFormat::Markdown,
                query: Some("from notes".to_string()),
                query_json: None,
                path: PathBuf::from("exports/docs.md"),
                title: Some("Docs".to_string()),
                author: None,
                toc: None,
                backlinks: false,
                frontmatter: false,
                pretty: false,
                graph_format: None,
            },
            false,
            false,
        )
        .expect("create markdown profile");

        let report = apply_export_profile_set(
            &paths,
            "docs",
            &ExportProfileSetRequest {
                format: Some(ExportProfileFormat::Json),
                query: None,
                query_json: Some("{\"source\":\"notes\"}".to_string()),
                clear_query: false,
                path: ConfigValueUpdate::Set(PathBuf::from("exports/docs.json")),
                title: ConfigValueUpdate::Clear,
                author: ConfigValueUpdate::Keep,
                toc: ConfigValueUpdate::Keep,
                backlinks: BoolConfigUpdate::Keep,
                frontmatter: BoolConfigUpdate::Keep,
                pretty: BoolConfigUpdate::SetTrue,
                graph_format: ConfigValueUpdate::Keep,
            },
            false,
        )
        .expect("set profile");

        assert_eq!(report.action, ExportProfileWriteAction::Updated);
        assert_eq!(
            report.changed_paths,
            vec![".vulcan/config.toml".to_string()]
        );
        assert_eq!(report.profile["format"], "json");
        assert!(report.profile["query"].is_null());
        assert_eq!(report.profile["query_json"], "{\"source\":\"notes\"}");
        assert!(report.profile["title"].is_null());
        assert_eq!(report.profile["pretty"], Value::Bool(true));
        let contents = config_contents(paths.vault_root());
        assert!(contents.contains("format = \"json\""));
        assert!(contents.contains("query_json = "));
        assert!(!contents.contains("title = \"Docs\""));
    }

    #[test]
    fn export_profile_rule_workflows_persist_add_update_move_and_delete() {
        let (_temp_dir, paths) = export_paths();
        apply_export_profile_create(
            &paths,
            "public_json",
            &create_json_profile_request(),
            false,
            false,
        )
        .expect("create profile");

        let add_first = apply_export_profile_rule_add(
            &paths,
            "public_json",
            None,
            &ExportProfileRuleRequest {
                query: None,
                query_json: None,
                exclude_callouts: Vec::new(),
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replacement_rules: vec![
                    "literal".to_string(),
                    "[[People/Bob]]".to_string(),
                    "[[People/Alice]]".to_string(),
                ],
            },
            false,
        )
        .expect("add first rule");
        assert_eq!(add_first.action, ExportProfileRuleWriteAction::Added);
        assert_eq!(add_first.rule_index, Some(1));

        let add_second = apply_export_profile_rule_add(
            &paths,
            "public_json",
            None,
            &ExportProfileRuleRequest {
                query: None,
                query_json: None,
                exclude_callouts: vec!["secret".to_string()],
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replacement_rules: Vec::new(),
            },
            false,
        )
        .expect("add second rule");
        assert_eq!(add_second.rule_index, Some(2));

        let update = apply_export_profile_rule_update(
            &paths,
            "public_json",
            1,
            &ExportProfileRuleRequest {
                query: None,
                query_json: None,
                exclude_callouts: Vec::new(),
                exclude_headings: Vec::new(),
                exclude_frontmatter_keys: Vec::new(),
                exclude_inline_fields: Vec::new(),
                replacement_rules: vec![
                    "regex".to_string(),
                    "[A-Z]+".to_string(),
                    "redacted".to_string(),
                ],
            },
            false,
        )
        .expect("update first rule");
        assert_eq!(update.action, ExportProfileRuleWriteAction::Updated);
        assert_eq!(update.rule_index, Some(1));

        let moved = apply_export_profile_rule_move(
            &paths,
            "public_json",
            ExportProfileRuleMoveRequest {
                index: 2,
                before: Some(1),
                after: None,
                last: false,
            },
            false,
        )
        .expect("move rule");
        assert_eq!(moved.action, ExportProfileRuleWriteAction::Moved);
        assert_eq!(moved.previous_rule_index, Some(2));
        assert_eq!(moved.rule_index, Some(1));

        let listed = build_export_profile_rule_list(&paths, "public_json").expect("rule list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].rule["exclude_callouts"][0], "secret");

        let deleted =
            apply_export_profile_rule_delete(&paths, "public_json", 1, false).expect("delete");
        assert_eq!(deleted.action, ExportProfileRuleWriteAction::Deleted);
        assert_eq!(deleted.rule_index, Some(1));

        let remaining = build_export_profile_rule_list(&paths, "public_json").expect("rule list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].rule["replace"][0]["pattern"], "[A-Z]+");
        assert!(config_contents(paths.vault_root()).contains("regex = true"));
    }

    #[test]
    fn export_profile_delete_and_invalid_regex_rules_are_reported() {
        let (_temp_dir, paths) = export_paths();
        apply_export_profile_create(
            &paths,
            "public_json",
            &create_json_profile_request(),
            false,
            false,
        )
        .expect("create profile");

        let error = build_content_transform_rules(
            &[],
            &[],
            &[],
            &[],
            &["regex".to_string(), "(".to_string(), "x".to_string()],
        )
        .expect_err("invalid regex should fail");
        assert!(error
            .message()
            .contains("content transform replacement rule 1 has invalid regex pattern"));

        let delete = apply_export_profile_delete(&paths, "public_json", false).expect("delete");
        assert!(delete.deleted);
        assert_eq!(
            delete.changed_paths,
            vec![".vulcan/config.toml".to_string()]
        );
        assert!(!config_contents(paths.vault_root()).contains("public_json"));
    }

    #[test]
    fn prepare_export_data_applies_transforms_and_backlink_adjustments() {
        let (_temp_dir, paths) = build_export_transform_vault();
        let report = execute_export_query(
            &paths,
            Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
            None,
            None,
        )
        .expect("query report");
        let raw_notes = load_exported_notes(&paths, &report).expect("raw notes");
        let raw_links = load_export_links(&paths, &raw_notes).expect("raw links");
        let transform_rules =
            build_content_transform_rules(&["secret gm".to_string()], &[], &[], &[], &[])
                .expect("rules");
        let prepared = prepare_export_data(&paths, &report, None, transform_rules.as_deref())
            .expect("prepared export");

        let home = prepared
            .notes
            .iter()
            .find(|note| note.note.document_path == "Home.md")
            .expect("home note");
        let bob = prepared
            .notes
            .iter()
            .find(|note| note.note.document_path == "People/Bob.md")
            .expect("bob note");

        assert!(home.content.contains("assets/public.png"));
        assert!(!home.content.contains("assets/secret.png"));
        assert!(!home.content.contains("Hidden [[People/Bob]]"));
        assert_eq!(
            collect_export_attachment_paths(&raw_links),
            vec![
                "assets/public.png".to_string(),
                "assets/secret.png".to_string()
            ]
        );
        assert_eq!(
            collect_export_attachment_paths(&prepared.links),
            vec!["assets/public.png".to_string()]
        );
        assert!(!bob.note.inlinks.iter().any(|link| link == "[[Home]]"));
    }

    #[test]
    fn shared_export_renderers_emit_expected_json_markdown_and_csv() {
        let (_temp_dir, paths) = build_export_transform_vault();
        let report = execute_export_query(
            &paths,
            Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
            None,
            None,
        )
        .expect("query report");
        let transform_rules =
            build_content_transform_rules(&["secret gm".to_string()], &[], &[], &[], &[])
                .expect("rules");
        let prepared = prepare_export_data(&paths, &report, None, transform_rules.as_deref())
            .expect("prepared export");

        let json = render_json_export_payload(&report, &prepared.notes, true).expect("json export");
        let parsed: Value = serde_json::from_str(&json).expect("json payload");
        assert_eq!(parsed["result_count"], Value::Number(2.into()));
        assert!(!json.contains("assets/secret.png"));

        let markdown = render_markdown_export_payload(&prepared.notes, Some("Public Notes"));
        assert!(markdown.starts_with("# Public Notes"));
        assert!(!markdown.contains("assets/secret.png"));
        assert!(markdown.contains("assets/public.png"));

        let csv = render_csv_export_payload(&report).expect("csv export");
        assert!(csv.starts_with(
            "document_path,file_name,file_ext,file_mtime,tags,starred,properties,inline_expressions,query"
        ));
        assert!(csv.contains("Home.md"));
        assert!(csv.contains("People/Bob.md"));
    }

    #[test]
    fn write_zip_export_packages_transformed_notes_and_manifest() {
        let (_temp_dir, paths) = build_export_transform_vault();
        let report = execute_export_query(
            &paths,
            Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
            None,
            None,
        )
        .expect("query report");
        let transform_rules =
            build_content_transform_rules(&["secret gm".to_string()], &[], &[], &[], &[])
                .expect("rules");
        let prepared = prepare_export_data(&paths, &report, None, transform_rules.as_deref())
            .expect("prepared export");
        let output_path = paths.vault_root().join("exports/public.zip");

        let summary = write_zip_export(
            &paths,
            &output_path,
            &report,
            &prepared.notes,
            &prepared.links,
        )
        .expect("zip export");

        assert_eq!(summary.result_count, 2);
        assert_eq!(summary.attachment_count, 1);

        let file = fs::File::open(&output_path).expect("zip file");
        let mut archive = ZipArchive::new(file).expect("zip archive");
        let mut names = Vec::new();
        for index in 0..archive.len() {
            names.push(
                archive
                    .by_index(index)
                    .expect("zip entry")
                    .name()
                    .to_string(),
            );
        }

        assert!(names.contains(&"Home.md".to_string()));
        assert!(names.contains(&"People/Bob.md".to_string()));
        assert!(names.contains(&"assets/public.png".to_string()));
        assert!(!names.contains(&"assets/secret.png".to_string()));
        assert!(names.contains(&".vulcan-export/manifest.json".to_string()));
        assert!(names.contains(&".vulcan-export/notes.json".to_string()));

        let mut manifest = String::new();
        archive
            .by_name(".vulcan-export/manifest.json")
            .expect("manifest")
            .read_to_string(&mut manifest)
            .expect("manifest read");
        assert!(manifest.contains("assets/public.png"));
        assert!(!manifest.contains("assets/secret.png"));

        let mut notes_json = String::new();
        archive
            .by_name(".vulcan-export/notes.json")
            .expect("notes json")
            .read_to_string(&mut notes_json)
            .expect("notes json read");
        assert!(notes_json.contains("assets/public.png"));
        assert!(!notes_json.contains("assets/secret.png"));
    }

    #[test]
    fn write_epub_export_packages_book_navigation_assets_and_backlinks() {
        let (_temp_dir, paths) = build_export_transform_vault();
        let report = execute_export_query(
            &paths,
            Some(r#"from notes where file.path matches "^(Home|People/Bob)\.md$""#),
            None,
            None,
        )
        .expect("query report");
        let prepared = prepare_export_data(&paths, &report, None, None).expect("prepared export");
        let output_path = paths.vault_root().join("exports/public.epub");
        let render_dataview_block = |_: &VaultPaths, _: &str, _: &str, _: &str| String::new();
        let render_base_embed = |_: &VaultPaths, _: &str, _: Option<&str>| String::new();

        let summary = write_epub_export(
            &paths,
            &output_path,
            &prepared.notes,
            &prepared.links,
            EpubExportOptions {
                title: Some("Public Notes"),
                author: Some("Vulcan"),
                backlinks: true,
                frontmatter: false,
                toc_style: ExportEpubTocStyleConfig::Tree,
            },
            EpubRenderCallbacks {
                render_dataview_block: &render_dataview_block,
                render_base_embed: &render_base_embed,
                render_inline_value: &test_epub_render_inline_value,
            },
        )
        .expect("epub export");

        assert_eq!(summary.result_count, 2);

        let file = fs::File::open(&output_path).expect("epub file");
        let mut archive = ZipArchive::new(file).expect("epub archive");

        let mut mimetype = String::new();
        archive
            .by_name("mimetype")
            .expect("mimetype")
            .read_to_string(&mut mimetype)
            .expect("mimetype read");
        assert_eq!(mimetype, "application/epub+zip");

        let mut nav = String::new();
        archive
            .by_name("OEBPS/nav.xhtml")
            .expect("nav")
            .read_to_string(&mut nav)
            .expect("nav read");
        assert!(nav.contains("Public Notes"));
        assert!(nav.contains("Home"));
        assert!(nav.contains("Bob"));

        let mut names = Vec::new();
        for index in 0..archive.len() {
            names.push(
                archive
                    .by_index(index)
                    .expect("archive entry")
                    .name()
                    .to_string(),
            );
        }
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("OEBPS/media/asset-"))
                .count(),
            2
        );

        let mut chapter_by_note = std::collections::HashMap::new();
        for name in names
            .iter()
            .filter(|name| name.starts_with("OEBPS/text/chapter-"))
        {
            let mut chapter = String::new();
            archive
                .by_name(name)
                .expect("chapter")
                .read_to_string(&mut chapter)
                .expect("chapter read");
            if chapter.contains("Home.md") {
                chapter_by_note.insert("Home.md", chapter);
            } else if chapter.contains("People/Bob.md") {
                chapter_by_note.insert("People/Bob.md", chapter);
            }
        }

        let home_chapter = chapter_by_note
            .get("Home.md")
            .expect("home chapter should be captured");
        let bob_chapter = chapter_by_note
            .get("People/Bob.md")
            .expect("bob chapter should be captured");

        assert!(home_chapter.contains("asset-embed asset-embed-image"));
        assert!(home_chapter.contains("src=\"../media/asset-"));
        assert!(bob_chapter.contains("<section class=\"backlinks\">"));
        assert!(bob_chapter.contains(">Home</a>"));
    }

    #[test]
    fn rewrite_epub_link_destination_maps_selected_notes_and_fragments() {
        let note_targets = HashMap::from([
            (
                "Projects/Alpha".to_string(),
                "chapter-001.xhtml".to_string(),
            ),
            (
                "Projects/Alpha.md".to_string(),
                "chapter-001.xhtml".to_string(),
            ),
            ("Home".to_string(), "chapter-002.xhtml".to_string()),
        ]);
        let asset_targets = HashMap::from([(
            "assets/logo.png".to_string(),
            "../media/asset-001.png".to_string(),
        )]);

        assert_eq!(
            rewrite_epub_link_destination(
                "People/Bob.md",
                "Projects/Alpha#Status",
                &note_targets,
                &asset_targets,
            ),
            Some("chapter-001.xhtml#status".to_string())
        );
        assert_eq!(
            rewrite_epub_link_destination("People/Bob.md", "Home", &note_targets, &asset_targets),
            Some("chapter-002.xhtml".to_string())
        );
        assert_eq!(
            rewrite_epub_link_destination(
                "Notes/Guide.md",
                "../assets/logo.png",
                &note_targets,
                &asset_targets,
            ),
            Some("../media/asset-001.png".to_string())
        );
        assert_eq!(
            rewrite_epub_link_destination(
                "Home.md",
                "https://example.com",
                &note_targets,
                &asset_targets,
            ),
            None
        );
    }

    #[test]
    fn inject_epub_heading_ids_applies_unique_anchor_ids_in_order() {
        let html = "<h1>Home</h1><p>x</p><h2>Status</h2><h2>Status</h2>";
        let headings = vec![
            EpubHeading {
                level: 1,
                text: "Home".to_string(),
                anchor_id: "home".to_string(),
            },
            EpubHeading {
                level: 2,
                text: "Status".to_string(),
                anchor_id: "status".to_string(),
            },
            EpubHeading {
                level: 2,
                text: "Status".to_string(),
                anchor_id: "status-2".to_string(),
            },
        ];

        let rendered = inject_epub_heading_ids(html, &headings);

        assert!(rendered.contains("<h1 id=\"home\">Home</h1>"));
        assert!(rendered.contains("<h2 id=\"status\">Status</h2>"));
        assert!(rendered.contains("<h2 id=\"status-2\">Status</h2>"));
    }

    #[test]
    fn epub_tag_targets_are_slugged_and_unique() {
        let notes = vec![
            ExportedNoteDocument {
                note: NoteRecord {
                    document_id: "1".to_string(),
                    document_path: "Home.md".to_string(),
                    file_name: "Home".to_string(),
                    file_ext: "md".to_string(),
                    file_mtime: 0,
                    file_ctime: 0,
                    file_size: 0,
                    properties: Value::Object(Map::new()),
                    tags: vec!["project".to_string(), "Project".to_string()],
                    links: Vec::new(),
                    starred: false,
                    inlinks: Vec::new(),
                    aliases: Vec::new(),
                    frontmatter: Value::Null,
                    periodic_type: None,
                    periodic_date: None,
                    list_items: Vec::new(),
                    tasks: Vec::new(),
                    raw_inline_expressions: Vec::new(),
                    inline_expressions: Vec::new(),
                },
                content: String::new(),
            },
            ExportedNoteDocument {
                note: NoteRecord {
                    document_id: "2".to_string(),
                    document_path: "Nested/Deep.md".to_string(),
                    file_name: "Deep".to_string(),
                    file_ext: "md".to_string(),
                    file_mtime: 0,
                    file_ctime: 0,
                    file_size: 0,
                    properties: Value::Object(Map::new()),
                    tags: vec!["project/alpha".to_string()],
                    links: Vec::new(),
                    starred: false,
                    inlinks: Vec::new(),
                    aliases: Vec::new(),
                    frontmatter: Value::Null,
                    periodic_type: None,
                    periodic_date: None,
                    list_items: Vec::new(),
                    tasks: Vec::new(),
                    raw_inline_expressions: Vec::new(),
                    inline_expressions: Vec::new(),
                },
                content: String::new(),
            },
        ];

        let targets = build_epub_tag_targets(&notes);

        assert_ne!(targets["project"], targets["Project"]);
        assert!(targets["project"].starts_with("tag-project"));
        assert!(targets["Project"].starts_with("tag-project"));
        assert_eq!(targets["project/alpha"], "tag-project-alpha.xhtml");
    }

    #[test]
    fn epub_tree_nav_trims_common_prefix_and_keeps_nested_directories() {
        let chapters = vec![
            EpubChapter {
                document_path: "Guides/Intro.md".to_string(),
                title: "Intro".to_string(),
                nav_path: "text/chapter-001.xhtml".to_string(),
                file_href: "chapter-001.xhtml".to_string(),
                headings: Vec::new(),
                content: String::new(),
            },
            EpubChapter {
                document_path: "Guides/Nested/Deep.md".to_string(),
                title: "Deep".to_string(),
                nav_path: "text/chapter-002.xhtml".to_string(),
                file_href: "chapter-002.xhtml".to_string(),
                headings: Vec::new(),
                content: String::new(),
            },
        ];

        let nav = render_epub_nav_document(
            "Guide Export",
            &build_epub_nav_nodes(&chapters, &[], ExportEpubTocStyleConfig::Tree),
        );

        assert!(!nav.contains("toc-directory-label\">Guides<"));
        assert!(nav.contains("toc-directory-label\">Nested<"));
        assert!(nav.contains("href=\"text/chapter-001.xhtml\">Intro</a>"));
        assert!(nav.contains("href=\"text/chapter-002.xhtml\">Deep</a>"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn write_sqlite_export_writes_expected_schema_and_rows() {
        let temp_dir = tempdir().expect("temp dir");
        let output_path = temp_dir.path().join("export.db");
        let note = NoteRecord {
            document_id: "doc-1".to_string(),
            document_path: "Tasks/Alpha.md".to_string(),
            file_name: "Alpha".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1_700_000_000,
            file_ctime: 1_700_000_000,
            file_size: 128,
            properties: Value::Object(Map::new()),
            tags: vec!["task".to_string(), "project".to_string()],
            links: vec!["[[Tasks/Beta]]".to_string()],
            starred: false,
            inlinks: Vec::new(),
            aliases: vec!["Alias".to_string()],
            frontmatter: serde_json::json!({"status": "open"}),
            periodic_type: None,
            periodic_date: None,
            list_items: Vec::new(),
            tasks: vec![NoteTaskRecord {
                id: "task-1".to_string(),
                list_item_id: "list-1".to_string(),
                status_char: " ".to_string(),
                status_name: "Todo".to_string(),
                status_type: "TODO".to_string(),
                status_next_symbol: None,
                checked: false,
                completed: false,
                text: "Ship Alpha".to_string(),
                byte_offset: 0,
                parent_task_id: None,
                section_heading: Some("Tasks".to_string()),
                line_number: 3,
                properties: Map::from_iter([(
                    "taskSource".to_string(),
                    Value::String("inline".to_string()),
                )]),
            }],
            raw_inline_expressions: Vec::new(),
            inline_expressions: vec![EvaluatedInlineExpression {
                expression: "2 + 2".to_string(),
                value: Value::from(4),
                error: None,
            }],
        };
        let report = QueryReport {
            query: QueryAst {
                source: QuerySource::Notes,
                predicates: Vec::new(),
                sort: None,
                projection: QueryProjection::All,
                limit: None,
                offset: 0,
            },
            notes: vec![note.clone()],
        };
        let notes = vec![ExportedNoteDocument {
            note,
            content: "# Alpha\n\n- [ ] Ship Alpha\n".to_string(),
        }];
        let links = vec![ExportLinkRecord {
            source_document_path: "Tasks/Alpha.md".to_string(),
            raw_text: "[[Tasks/Beta]]".to_string(),
            link_kind: "wikilink".to_string(),
            display_text: None,
            target_path_candidate: Some("Tasks/Beta".to_string()),
            target_heading: None,
            target_block: None,
            resolved_target_path: Some("Tasks/Beta.md".to_string()),
            origin_context: "body".to_string(),
            byte_offset: 8,
            resolved_target_extension: Some("md".to_string()),
        }];

        let summary =
            write_sqlite_export(&output_path, &report, &notes, &links).expect("sqlite export");

        assert_eq!(summary.result_count, 1);
        assert_eq!(summary.link_count, 1);
        assert_eq!(summary.tag_count, 2);
        assert_eq!(summary.task_count, 1);

        let connection = rusqlite::Connection::open(&output_path).expect("export db");
        let user_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user version");
        let note_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))
            .expect("notes count");
        let link_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM links", [], |row| row.get(0))
            .expect("links count");
        let tag_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
            .expect("tags count");
        let task_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .expect("tasks count");
        let meta_result_count: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'result_count'",
                [],
                |row| row.get(0),
            )
            .expect("meta result count");

        assert_eq!(user_version, 1);
        assert_eq!(note_count, 1);
        assert_eq!(link_count, 1);
        assert_eq!(tag_count, 2);
        assert_eq!(task_count, 1);
        assert_eq!(meta_result_count, "1");
    }
}
