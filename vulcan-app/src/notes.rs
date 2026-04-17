use crate::plugins;
use crate::templates::{
    find_frontmatter_block, load_named_template, parse_frontmatter_document,
    render_loaded_template, render_note_from_parts, LoadedTemplateRenderRequest,
    TemplateEngineKind, TemplateRunMode, TemplateTimestamp, YamlMapping,
};
use crate::AppError;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use vulcan_core::expression::functions::{date_components, parse_date_like_string};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::properties::{extract_indexed_properties, load_note_index};
use vulcan_core::{
    expected_periodic_note_path, load_vault_config, parse_document, parse_dql_with_diagnostics,
    period_range_for_date, resolve_link, resolve_note_reference, DoctorByteRange,
    DoctorDiagnosticIssue, GraphQueryError, LinkResolutionProblem, ParsedDocument, PeriodicConfig,
    PluginEvent, ResolverDocument, ResolverLink, VaultConfig, VaultPaths,
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
    quiet: bool,
) -> Result<NoteCreateReport, AppError> {
    let requested_path = normalize_note_path(&request.path)?;
    let config = load_vault_config(paths).config;
    let mut warnings = Vec::new();
    let mut frontmatter = request.frontmatter.clone();
    let mut body = request.body.clone();
    let mut final_path = requested_path.clone();
    let mut template = None;
    let mut engine = None;
    let mut changed_paths = Vec::new();

    if let Some(template_name) = request.template.as_deref() {
        let loaded = load_named_template(paths, &config, template_name)?;
        let vars = HashMap::new();
        let rendered = render_loaded_template(
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
            },
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
    }

    let absolute_path = paths.vault_root().join(&final_path);
    if absolute_path.exists() {
        return Err(AppError::operation(format!(
            "destination note already exists: {final_path}"
        )));
    }

    let content =
        render_note_from_parts(frontmatter.as_ref(), &body).map_err(AppError::operation)?;
    dispatch_note_write_plugin_hooks(
        paths,
        permission_profile,
        &final_path,
        "create",
        None,
        &content,
        quiet,
    )?;
    if let Some(parent) = absolute_path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    fs::write(&absolute_path, &content).map_err(AppError::operation)?;
    dispatch_note_create_plugin_hooks(paths, permission_profile, &final_path, &content, quiet);
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

pub fn apply_note_append(
    paths: &VaultPaths,
    request: &NoteAppendRequest,
    permission_profile: Option<&str>,
    quiet: bool,
) -> Result<NoteAppendReport, AppError> {
    if request.periodic.is_some() && request.note.is_some() {
        return Err(AppError::operation(
            "`note append` accepts either a note or a periodic target, not both",
        ));
    }

    let config = load_vault_config(paths).config;
    let target = load_note_append_target(paths, &config, request)?;
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
        })?;

    let mut warnings = target.warnings;
    warnings.extend(rendered.warnings);
    warnings.extend(rendered.diagnostics);

    let content = match request.mode {
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

    dispatch_note_write_plugin_hooks(
        paths,
        permission_profile,
        &target.path,
        "append",
        Some(&target.existing),
        &content,
        quiet,
    )?;
    if let Some(parent) = paths.vault_root().join(&target.path).parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    fs::write(paths.vault_root().join(&target.path), &content).map_err(AppError::operation)?;
    if target.created {
        dispatch_note_create_plugin_hooks(paths, permission_profile, &target.path, &content, quiet);
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
    quiet: bool,
) -> Result<NoteSetReport, AppError> {
    let path = resolve_existing_note_path(paths, &request.note)?;
    let absolute_path = paths.vault_root().join(&path);
    let existing = fs::read_to_string(&absolute_path).map_err(AppError::operation)?;
    let content = if request.preserve_frontmatter {
        preserve_existing_frontmatter(&existing, &request.replacement)
    } else {
        request.replacement.clone()
    };
    dispatch_note_write_plugin_hooks(
        paths,
        permission_profile,
        &path,
        "set",
        Some(&existing),
        &content,
        quiet,
    )?;
    fs::write(&absolute_path, &content).map_err(AppError::operation)?;

    Ok(NoteSetReport {
        path: path.clone(),
        preserved_frontmatter: request.preserve_frontmatter,
        changed_paths: vec![path],
        content,
    })
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
) -> Result<String, AppError> {
    let config = load_vault_config(paths).config;
    let template_name = config
        .periodic
        .note(period_type)
        .and_then(|note| note.template.as_deref());
    let Some(template_name) = template_name else {
        return Ok(String::new());
    };

    let loaded = match load_named_template(paths, &config, template_name) {
        Ok(loaded) => loaded,
        Err(error) => {
            warnings.push(format!(
                "failed to resolve periodic template `{template_name}` for `{period_type}`: {error}"
            ));
            return Ok(String::new());
        }
    };
    let vars = HashMap::new();
    let rendered = render_loaded_template(
        paths,
        &config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path: relative_path,
            target_contents: None,
            engine: TemplateEngineKind::Auto,
            vars: &vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
        },
    )?;
    warnings.extend(loaded.template.warning);
    warnings.extend(rendered.warnings);
    warnings.extend(rendered.diagnostics);
    Ok(rendered.content)
}

fn normalize_note_path(path: &str) -> Result<String, AppError> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(AppError::operation)
}

fn normalize_date_argument(date: Option<&str>) -> Result<String, AppError> {
    match date
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        None => Ok(current_utc_date_string()),
        Some(value) if value == "today" => Ok(current_utc_date_string()),
        Some(value) => {
            let timestamp = parse_date_like_string(&value)
                .ok_or_else(|| AppError::operation(format!("invalid date: {value}")))?;
            let (year, month, day, _, _, _, _) = date_components(timestamp);
            Ok(format!("{year:04}-{month:02}-{day:02}"))
        }
    }
}

fn current_utc_date_string() -> String {
    TemplateTimestamp::current().default_date_string()
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

fn resolve_existing_note_path(paths: &VaultPaths, note: &str) -> Result<String, AppError> {
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
            Some(LinkResolutionProblem::Unresolved) => diagnostics.push(DoctorDiagnosticIssue {
                document_path: Some(relative_path.to_string()),
                message: format!("Unresolved link target `{}`", link.raw_text),
                byte_range: Some(DoctorByteRange {
                    start: link.byte_offset,
                    end: link.byte_offset + link.raw_text.len(),
                }),
            }),
            Some(LinkResolutionProblem::Ambiguous(matches)) => {
                diagnostics.push(DoctorDiagnosticIssue {
                    document_path: Some(relative_path.to_string()),
                    message: format!(
                        "Ambiguous link target `{}` matched {}",
                        link.raw_text,
                        matches.join(", ")
                    ),
                    byte_range: Some(DoctorByteRange {
                        start: link.byte_offset,
                        end: link.byte_offset + link.raw_text.len(),
                    }),
                });
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
                        diagnostics.push(DoctorDiagnosticIssue {
                            document_path: Some(relative_path.to_string()),
                            message: format!(
                                "Broken heading link `{}`: heading `{target_heading}` was not found in {target_path}",
                                link.raw_text
                            ),
                            byte_range: Some(DoctorByteRange {
                                start: link.byte_offset,
                                end: link.byte_offset + link.raw_text.len(),
                            }),
                        });
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
                        diagnostics.push(DoctorDiagnosticIssue {
                            document_path: Some(relative_path.to_string()),
                            message: format!(
                                "Broken block link `{}`: block `^{target_block}` was not found in {target_path}",
                                link.raw_text
                            ),
                            byte_range: Some(DoctorByteRange {
                                start: link.byte_offset,
                                end: link.byte_offset + link.raw_text.len(),
                            }),
                        });
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
    quiet: bool,
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
        quiet,
    )
}

fn dispatch_note_create_plugin_hooks(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    relative_path: &str,
    content: &str,
    quiet: bool,
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
        quiet,
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
                render_periodic_note_contents(paths, period_type, &target.path, &mut warnings)?,
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
        apply_note_append, apply_note_create, apply_note_set, diagnose_note_contents,
        NoteAppendMode, NoteAppendRequest, NoteCreateRequest, NoteSetRequest,
    };
    use crate::templates::{YamlMapping, YamlValue};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{initialize_vulcan_dir, VaultPaths};

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
            true,
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
            true,
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
            true,
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
}
