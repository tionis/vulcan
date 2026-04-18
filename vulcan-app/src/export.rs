use crate::config as app_config;
use crate::templates::TemplateTimestamp;
use crate::AppError;
use regex::Regex;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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
        build_export_profile_list, build_export_profile_rule_list,
        build_export_profile_show_report, collect_export_attachment_paths, execute_export_query,
        load_export_links, load_exported_notes, prepare_export_data, render_csv_export_payload,
        render_json_export_payload, render_markdown_export_payload, write_sqlite_export,
        write_zip_export, BoolConfigUpdate, ConfigValueUpdate, ExportLinkRecord,
        ExportProfileCreateRequest, ExportProfileFormat, ExportProfileRuleMoveRequest,
        ExportProfileRuleRequest, ExportProfileRuleWriteAction, ExportProfileSetRequest,
        ExportProfileWriteAction, ExportedNoteDocument,
    };
    use serde_json::{Map, Value};
    use std::fs;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;
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
