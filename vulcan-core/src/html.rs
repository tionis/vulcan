use crate::bases::{BasesEvalReport, BasesEvaluatedView, BasesRow};
use crate::config::{load_vault_config, JsRuntimeSandbox, VaultConfig};
use crate::dataview_js::{DataviewJsOutput, DataviewJsResult};
use crate::dql::{DqlDiagnostic, DqlQueryResult, DqlQueryType};
use crate::note::{read_note, NoteReadOptions};
use crate::parser::{
    fragment_parser_options, parser_options, LinkKind, ParseDiagnosticKind, ParsedDocument,
};
use crate::paths::VaultPaths;
use crate::properties::{evaluate_note_inline_expressions, load_note_index, NoteRecord};
use crate::resolver::{resolve_link, ResolverDocument, ResolverLink};
use crate::tasks::{evaluate_tasks_query, TasksQueryResult};
use crate::{
    evaluate_base_file, evaluate_dataview_js_with_options, evaluate_dql_with_filter, parse_document,
};
use ammonia::clean as sanitize_html_fragment;
use pulldown_cmark::{
    html, CowStr, Event as MarkdownEvent, Parser as MarkdownParser, Tag as MarkdownTag,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HtmlDataviewJsPolicy {
    #[default]
    Off,
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HtmlRawHtmlPolicy {
    #[default]
    Passthrough,
    Sanitize,
    Strip,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HtmlLinkTargets {
    pub note_hrefs: HashMap<String, String>,
    pub asset_hrefs: HashMap<String, String>,
    pub tag_hrefs: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlRenderOptions<'a> {
    pub source_path: Option<&'a str>,
    pub full_document: bool,
    pub link_targets: Option<&'a HtmlLinkTargets>,
    pub dataview_js_policy: HtmlDataviewJsPolicy,
    pub raw_html_policy: HtmlRawHtmlPolicy,
    pub max_embed_depth: usize,
}

impl Default for HtmlRenderOptions<'_> {
    fn default() -> Self {
        Self {
            source_path: None,
            full_document: true,
            link_targets: None,
            dataview_js_policy: HtmlDataviewJsPolicy::Off,
            raw_html_policy: HtmlRawHtmlPolicy::Passthrough,
            max_embed_depth: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlRenderHeading {
    pub level: u8,
    pub text: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlRenderDiagnostic {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlRenderResult {
    pub html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headings: Vec<HtmlRenderHeading>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<HtmlRenderDiagnostic>,
}

#[derive(Debug, Default)]
struct HtmlRenderState {
    embed_stack: Vec<String>,
}

struct HtmlRenderEnvironment<'a> {
    config: &'a VaultConfig,
    note_index: Option<&'a HashMap<String, NoteRecord>>,
    resolver_documents: Option<&'a [ResolverDocument]>,
    link_targets: &'a HtmlLinkTargets,
    dataview_js_policy: HtmlDataviewJsPolicy,
    raw_html_policy: HtmlRawHtmlPolicy,
    max_embed_depth: usize,
}

#[derive(Debug, Clone)]
struct HtmlMarkdownReplacement {
    start: usize,
    end: usize,
    replacement: String,
}

#[derive(Debug, Default)]
struct RenderedMarkdownHtml {
    html: String,
    raw_html_events: usize,
    sanitized_raw_html_events: usize,
    stripped_raw_html_events: usize,
}

#[must_use]
pub fn render_vault_html(
    paths: &VaultPaths,
    source: &str,
    options: &HtmlRenderOptions<'_>,
) -> HtmlRenderResult {
    let config = load_vault_config(paths).config;
    let note_index = load_note_index(paths).ok();
    let resolver_documents = note_index.as_ref().map(build_resolver_documents);
    let empty_targets = HtmlLinkTargets::default();
    let env = HtmlRenderEnvironment {
        config: &config,
        note_index: note_index.as_ref(),
        resolver_documents: resolver_documents.as_deref(),
        link_targets: options.link_targets.unwrap_or(&empty_targets),
        dataview_js_policy: options.dataview_js_policy,
        raw_html_policy: options.raw_html_policy,
        max_embed_depth: options.max_embed_depth.max(1),
    };
    let mut state = HtmlRenderState::default();
    render_html_internal(
        paths,
        source,
        options.source_path,
        options.full_document,
        &env,
        &mut state,
    )
}

#[must_use]
pub fn render_note_html(paths: &VaultPaths, source_path: &str, source: &str) -> HtmlRenderResult {
    render_vault_html(
        paths,
        source,
        &HtmlRenderOptions {
            source_path: Some(source_path),
            full_document: true,
            ..HtmlRenderOptions::default()
        },
    )
}

#[must_use]
pub fn render_note_fragment_html(
    paths: &VaultPaths,
    source_path: Option<&str>,
    source: &str,
) -> HtmlRenderResult {
    render_vault_html(
        paths,
        source,
        &HtmlRenderOptions {
            source_path,
            full_document: false,
            ..HtmlRenderOptions::default()
        },
    )
}

fn render_html_internal(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
    full_document: bool,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
) -> HtmlRenderResult {
    let parsed = parse_document(source, env.config);
    let mut diagnostics = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| HtmlRenderDiagnostic {
            kind: parse_diagnostic_kind_name(diagnostic.kind).to_string(),
            message: diagnostic.message.clone(),
        })
        .collect::<Vec<_>>();
    let rendered_markdown = render_markdown_with_replacements(
        paths,
        source,
        source_path,
        &parsed,
        env,
        state,
        &mut diagnostics,
    );
    let headings = collect_headings(&parsed);
    let rendered = render_markdown_html_with_targets(
        &rendered_markdown,
        source_path,
        full_document,
        env.link_targets,
        env.raw_html_policy,
    );
    if rendered.sanitized_raw_html_events > 0 {
        diagnostics.push(HtmlRenderDiagnostic {
            kind: "raw_html_sanitized".to_string(),
            message: format!(
                "sanitized {} raw HTML fragment(s) during HTML rendering",
                rendered.sanitized_raw_html_events
            ),
        });
    }
    if rendered.stripped_raw_html_events > 0 {
        diagnostics.push(HtmlRenderDiagnostic {
            kind: "raw_html_stripped".to_string(),
            message: format!(
                "stripped {} raw HTML fragment(s) during HTML rendering",
                rendered.stripped_raw_html_events
            ),
        });
    }
    let mut html = rendered.html;
    if !headings.is_empty() {
        html = inject_heading_ids(&html, &headings);
    }
    HtmlRenderResult {
        html,
        title: select_render_title(source_path, &headings),
        headings,
        diagnostics,
    }
}

#[allow(clippy::too_many_lines)]
fn render_markdown_with_replacements(
    paths: &VaultPaths,
    source: &str,
    source_path: Option<&str>,
    parsed: &ParsedDocument,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    let mut replacements = Vec::new();

    for field in &parsed.inline_fields {
        replacements.push(HtmlMarkdownReplacement {
            start: field.byte_range.start,
            end: field.byte_range.end,
            replacement: render_inline_field_html(
                paths,
                &field.key,
                &field.value_text,
                source_path,
                env,
                state,
                diagnostics,
            ),
        });
    }

    if let (Some(source_path), Some(note_index)) = (source_path, env.note_index) {
        if let Some(note) = note_by_path(note_index, source_path) {
            let inline_results = evaluate_note_inline_expressions(note, note_index);
            for (index, expression) in parsed.inline_expressions.iter().enumerate() {
                let result = inline_results.get(index).cloned();
                let replacement = result.map_or_else(
                    || escape_html_text(&expression.expression),
                    |evaluated| {
                        evaluated.error.as_ref().map_or_else(
                            || {
                                render_json_value_html(
                                    paths,
                                    &evaluated.value,
                                    Some(source_path),
                                    env,
                                    state,
                                    diagnostics,
                                )
                            },
                            |error| render_message_html("Dataview inline error:", error),
                        )
                    },
                );
                replacements.push(HtmlMarkdownReplacement {
                    start: expression.byte_range.start,
                    end: expression.byte_range.end,
                    replacement,
                });
            }
        }
    }

    for block in &parsed.dataview_blocks {
        replacements.push(HtmlMarkdownReplacement {
            start: block.byte_range.start,
            end: block.byte_range.end,
            replacement: render_dataview_block_html(
                paths,
                source_path,
                &block.language,
                &block.text,
                env,
                state,
                diagnostics,
            ),
        });
    }

    for block in &parsed.tasks_blocks {
        replacements.push(HtmlMarkdownReplacement {
            start: block.byte_range.start,
            end: block.byte_range.end,
            replacement: render_tasks_block_html(paths, &block.text),
        });
    }

    for link in &parsed.links {
        if link.link_kind != LinkKind::Embed {
            continue;
        }
        let replacement = if link.is_note_embed {
            render_note_embed_html(paths, source_path, link, env, state, diagnostics)
        } else if link
            .target_path_candidate
            .as_deref()
            .is_some_and(|candidate| {
                Path::new(candidate)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
            })
        {
            render_base_embed_html(
                paths,
                source_path,
                link.target_path_candidate.as_deref().unwrap_or_default(),
                link.target_heading.as_deref(),
            )
        } else {
            render_asset_embed_html(source_path, link, env)
        };
        replacements.push(HtmlMarkdownReplacement {
            start: link.byte_offset,
            end: link.byte_offset + link.raw_text.len(),
            replacement,
        });
    }

    for tag in &parsed.tags {
        let Some(href) = env.link_targets.tag_hrefs.get(&tag.tag_text) else {
            continue;
        };
        replacements.push(HtmlMarkdownReplacement {
            start: tag.byte_offset,
            end: tag.byte_offset + tag.tag_text.len() + 1,
            replacement: format!(
                "<a class=\"tag-link\" href=\"{}\">#{}</a>",
                escape_html_text(href),
                escape_html_text(&tag.tag_text)
            ),
        });
    }

    apply_markdown_replacements(source, replacements)
}

fn render_dataview_block_html(
    paths: &VaultPaths,
    source_path: Option<&str>,
    language: &str,
    source: &str,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    if language == "dataview" {
        return match evaluate_dql_with_filter(paths, source, source_path, None) {
            Ok(result) => {
                render_dql_query_html(paths, &result, source_path, env, state, diagnostics)
            }
            Err(error) => render_message_html("Dataview error:", &error.to_string()),
        };
    }
    if language == "dataviewjs" {
        if env.dataview_js_policy != HtmlDataviewJsPolicy::Static {
            diagnostics.push(HtmlRenderDiagnostic {
                kind: "dataviewjs_disabled".to_string(),
                message: "DataviewJS rendering is disabled for this HTML render".to_string(),
            });
            return render_message_html(
                "DataviewJS disabled:",
                "enable static DataviewJS rendering in the caller to evaluate this block",
            );
        }
        return match evaluate_dataview_js_with_options(
            paths,
            source,
            source_path,
            crate::DataviewJsEvalOptions {
                sandbox: Some(JsRuntimeSandbox::Strict),
                ..crate::DataviewJsEvalOptions::default()
            },
        ) {
            Ok(result) => {
                render_dataview_js_result_html(paths, &result, source_path, env, state, diagnostics)
            }
            Err(error) => render_message_html("DataviewJS error:", &error.to_string()),
        };
    }
    render_message_html(
        "Dataview error:",
        &format!("unsupported block language `{language}`"),
    )
}

fn render_tasks_block_html(paths: &VaultPaths, source: &str) -> String {
    match evaluate_tasks_query(paths, source) {
        Ok(result) => render_tasks_query_html(&result),
        Err(error) => render_message_html("Tasks error:", &error.to_string()),
    }
}

fn render_base_embed_html(
    paths: &VaultPaths,
    source_path: Option<&str>,
    base_path: &str,
    view_name: Option<&str>,
) -> String {
    let resolved_path = resolve_relative_asset_path(source_path, base_path)
        .unwrap_or_else(|| base_path.to_string());
    match evaluate_base_file(paths, &resolved_path) {
        Ok(mut report) => {
            if let Some(view_name) = view_name.map(str::trim).filter(|value| !value.is_empty()) {
                if let Some(view) = report
                    .views
                    .iter()
                    .find(|view| view.name.as_deref() == Some(view_name))
                    .cloned()
                {
                    report.views = vec![view];
                } else {
                    return render_message_html(
                        "Bases error:",
                        &format!("view `{view_name}` was not found in {resolved_path}"),
                    );
                }
            }
            render_bases_report_html(&report)
        }
        Err(error) => render_message_html("Bases error:", &error.to_string()),
    }
}

fn render_note_embed_html(
    paths: &VaultPaths,
    source_path: Option<&str>,
    link: &crate::RawLink,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    let Some(source_path) = source_path else {
        return render_message_html("Embed error:", "note embeds require a source note path");
    };
    let Some(target_path) = resolve_embedded_note_path(source_path, link, env) else {
        diagnostics.push(HtmlRenderDiagnostic {
            kind: "unresolved_embed".to_string(),
            message: format!("could not resolve embedded note `{}`", link.raw_text),
        });
        return render_message_html(
            "Embed error:",
            &format!("unresolved note embed {}", link.raw_text),
        );
    };
    let embed_key = format!(
        "{} -> {}#{}^{}",
        source_path,
        target_path,
        link.target_heading.clone().unwrap_or_default(),
        link.target_block.clone().unwrap_or_default()
    );
    if state.embed_stack.len() >= env.max_embed_depth {
        diagnostics.push(HtmlRenderDiagnostic {
            kind: "embed_depth_limit".to_string(),
            message: format!(
                "embed depth limit reached while rendering {}",
                link.raw_text
            ),
        });
        return render_message_html("Embed error:", "embed depth limit reached");
    }
    if state
        .embed_stack
        .iter()
        .any(|candidate| candidate == &embed_key)
    {
        diagnostics.push(HtmlRenderDiagnostic {
            kind: "embed_cycle".to_string(),
            message: format!("embed cycle detected while rendering {}", link.raw_text),
        });
        return render_message_html("Embed error:", "embed cycle detected");
    }
    let Ok(target_source) = fs::read_to_string(paths.vault_root().join(&target_path)) else {
        diagnostics.push(HtmlRenderDiagnostic {
            kind: "missing_embed_target".to_string(),
            message: format!("embedded note `{target_path}` could not be read"),
        });
        return render_message_html("Embed error:", &format!("could not read {target_path}"));
    };
    let target_parsed = parse_document(&target_source, env.config);
    let selection = read_note(
        &target_source,
        &target_parsed,
        &NoteReadOptions {
            heading: link.target_heading.clone(),
            block_ref: link.target_block.clone(),
            no_frontmatter: true,
            ..NoteReadOptions::default()
        },
    );
    let selected = match selection {
        Ok(selection) => selection.content,
        Err(error) => {
            diagnostics.push(HtmlRenderDiagnostic {
                kind: "invalid_embed_target".to_string(),
                message: error.to_string(),
            });
            return render_message_html("Embed error:", &error.to_string());
        }
    };
    state.embed_stack.push(embed_key);
    let rendered = render_html_internal(paths, &selected, Some(&target_path), false, env, state);
    let _ = state.embed_stack.pop();
    format!(
        "<section class=\"note-embed\" data-note-path=\"{}\">{}</section>",
        escape_html_text(&target_path),
        rendered.html
    )
}

fn render_asset_embed_html(
    source_path: Option<&str>,
    link: &crate::RawLink,
    env: &HtmlRenderEnvironment<'_>,
) -> String {
    let resolved = link
        .target_path_candidate
        .as_deref()
        .and_then(|candidate| resolve_relative_asset_path(source_path, candidate))
        .or_else(|| link.target_path_candidate.clone())
        .unwrap_or_else(|| "Attachment".to_string());
    let href = resolve_asset_href(
        source_path.unwrap_or_default(),
        &resolved,
        None,
        &env.link_targets.asset_hrefs,
    )
    .unwrap_or(resolved.clone());
    let label = link
        .display_text
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            Path::new(&resolved)
                .file_name()
                .and_then(|value| value.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Attachment".to_string());
    let extension = Path::new(&resolved)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "tif" | "tiff" => format!(
            "<figure class=\"asset-embed asset-embed-image\"><img src=\"{}\" alt=\"{}\" /></figure>",
            escape_html_text(&href),
            escape_html_text(&label)
        ),
        "mp3" | "wav" | "ogg" => format!(
            "<audio class=\"asset-embed asset-embed-audio\" controls=\"controls\" src=\"{}\">{}</audio>",
            escape_html_text(&href),
            escape_html_text(&label)
        ),
        "mp4" | "mov" | "webm" | "avi" => format!(
            "<video class=\"asset-embed asset-embed-video\" controls=\"controls\" src=\"{}\">{}</video>",
            escape_html_text(&href),
            escape_html_text(&label)
        ),
        "pdf" => format!(
            "<figure class=\"asset-embed asset-embed-pdf\"><iframe src=\"{}\" title=\"{}\"></iframe><figcaption><a href=\"{}\">{}</a></figcaption></figure>",
            escape_html_text(&href),
            escape_html_text(&label),
            escape_html_text(&href),
            escape_html_text(&label)
        ),
        _ => format!(
            "<p class=\"asset-embed asset-embed-link\"><a href=\"{}\">{}</a></p>",
            escape_html_text(&href),
            escape_html_text(&label)
        ),
    }
}

fn render_inline_field_html(
    paths: &VaultPaths,
    key: &str,
    value_text: &str,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    let value_html = render_json_value_html(
        paths,
        &Value::String(value_text.to_string()),
        source_path,
        env,
        state,
        diagnostics,
    );
    format!(
        "<span class=\"dataview-inline-field\"><span class=\"dataview-inline-field-key\">{}</span><span class=\"dataview-inline-field-separator\">:</span> <span class=\"dataview-inline-field-value\">{}</span></span>",
        escape_html_text(key),
        value_html
    )
}

fn render_json_value_html(
    paths: &VaultPaths,
    value: &Value,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    _diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    match value {
        Value::String(text) => strip_paragraph_wrapper(
            &render_html_internal(paths, text, source_path, false, env, state).html,
        ),
        Value::Null => String::new(),
        Value::Bool(flag) => escape_html_text(&flag.to_string()),
        Value::Number(number) => escape_html_text(&number.to_string()),
        other => {
            escape_html_text(&serde_json::to_string(other).unwrap_or_else(|_| other.to_string()))
        }
    }
}

fn render_dql_query_html(
    paths: &VaultPaths,
    result: &DqlQueryResult,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    let mut sections = Vec::new();
    let body = match result.query_type {
        DqlQueryType::Table => {
            render_dql_table_html(paths, result, source_path, env, state, diagnostics)
        }
        DqlQueryType::List => {
            render_dql_list_html(paths, result, source_path, env, state, diagnostics)
        }
        DqlQueryType::Task => {
            render_dql_task_html(paths, result, source_path, env, state, diagnostics)
        }
        DqlQueryType::Calendar => {
            render_dql_calendar_html(paths, result, source_path, env, state, diagnostics)
        }
    };
    if !body.is_empty() {
        sections.push(body);
    }
    let diagnostics_html = render_dql_diagnostics_html(&result.diagnostics);
    if !diagnostics_html.is_empty() {
        sections.push(diagnostics_html);
    }
    sections.join("\n")
}

fn render_dataview_js_result_html(
    paths: &VaultPaths,
    result: &DataviewJsResult,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    if result.outputs.is_empty() {
        return result.value.as_ref().map_or_else(String::new, |value| {
            render_json_value_html(paths, value, source_path, env, state, diagnostics)
        });
    }

    result
        .outputs
        .iter()
        .map(|output| match output {
            DataviewJsOutput::Query { result } => {
                render_dql_query_html(paths, result, source_path, env, state, diagnostics)
            }
            DataviewJsOutput::Table { headers, rows } => {
                render_value_table_html(paths, headers, rows, source_path, env, state, diagnostics)
            }
            DataviewJsOutput::List { items } => {
                render_value_list_html(paths, items, source_path, env, state, diagnostics)
            }
            DataviewJsOutput::TaskList {
                tasks,
                group_by_file,
            } => render_task_values_html(
                paths,
                tasks,
                *group_by_file,
                source_path,
                env,
                state,
                diagnostics,
            ),
            DataviewJsOutput::Paragraph { text } => format!("<p>{}</p>", escape_html_text(text)),
            DataviewJsOutput::Span { text } => {
                format!("<span>{}</span>", escape_html_text(text))
            }
            DataviewJsOutput::Header { level, text } => {
                let level = (*level).clamp(1, 6);
                format!("<h{level}>{}</h{level}>", escape_html_text(text))
            }
            DataviewJsOutput::Element {
                element,
                text,
                attrs,
            } => render_generic_element_html(element, text, attrs),
        })
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_tasks_query_html(result: &TasksQueryResult) -> String {
    if result.tasks.is_empty() {
        return "<p>No matching tasks.</p>".to_string();
    }

    if !result.groups.is_empty() {
        let mut rendered = String::from("<div class=\"tasks-query-groups\">");
        for group in &result.groups {
            writeln!(
                rendered,
                "<section class=\"tasks-query-group\"><h4>{}: {}</h4><ul>",
                escape_html_text(&group.field),
                escape_html_text(&render_plain_json_value(&group.key))
            )
            .expect("writing to string cannot fail");
            for task in &group.tasks {
                rendered.push_str(&render_single_task_item_html(task));
            }
            rendered.push_str("</ul></section>");
        }
        rendered.push_str("</div>");
        return rendered;
    }

    let mut rendered = String::from("<ul class=\"tasks-query-list\">");
    for task in &result.tasks {
        rendered.push_str(&render_single_task_item_html(task));
    }
    rendered.push_str("</ul>");
    rendered
}

fn render_single_task_item_html(task: &Value) -> String {
    let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
    let text = task
        .get("text")
        .map(render_plain_json_value)
        .unwrap_or_default();
    let path = task.get("path").and_then(Value::as_str);
    match path {
        Some(path) if !path.is_empty() => format!(
            "<li><span class=\"task-status\">[{status}]</span> <span class=\"task-text\">{}</span> <span class=\"task-path\">{}</span></li>",
            escape_html_text(&text),
            escape_html_text(path)
        ),
        _ => format!(
            "<li><span class=\"task-status\">[{status}]</span> <span class=\"task-text\">{}</span></li>",
            escape_html_text(&text)
        ),
    }
}

#[allow(clippy::format_collect)]
fn render_bases_report_html(report: &BasesEvalReport) -> String {
    let mut sections = Vec::new();
    for view in &report.views {
        sections.push(render_bases_view_html(view));
    }
    if !report.diagnostics.is_empty() {
        let items = report
            .diagnostics
            .iter()
            .map(|diagnostic| format!("<li>{}</li>", escape_html_text(&diagnostic.message)))
            .collect::<String>();
        sections.push(format!(
            "<section class=\"bases-diagnostics\"><h4>Diagnostics</h4><ul>{items}</ul></section>"
        ));
    }
    sections.join("\n")
}

fn render_bases_view_html(view: &BasesEvaluatedView) -> String {
    let title = view
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&view.view_type);
    let body = if view.view_type.eq_ignore_ascii_case("tasknoteskanban") {
        render_bases_kanban_html(view)
    } else if view.view_type.eq_ignore_ascii_case("tasknotestasklist") {
        render_bases_task_list_html(view)
    } else {
        render_bases_table_html(view)
    };
    format!(
        "<section class=\"base-view base-view-{}\"><h4>{}</h4>{}</section>",
        escape_html_text(&view.view_type.to_ascii_lowercase()),
        escape_html_text(title),
        body
    )
}

fn render_bases_table_html(view: &BasesEvaluatedView) -> String {
    let mut rendered = String::from("<table class=\"bases-table\"><thead><tr>");
    for column in &view.columns {
        write!(
            rendered,
            "<th>{}</th>",
            escape_html_text(&column.display_name)
        )
        .expect("writing to string cannot fail");
    }
    rendered.push_str("</tr></thead><tbody>");
    for row in &view.rows {
        rendered.push_str("<tr>");
        for column in &view.columns {
            let value = row.cells.get(&column.key).unwrap_or(&Value::Null);
            write!(
                rendered,
                "<td>{}</td>",
                escape_html_text(&render_plain_json_value(value))
            )
            .expect("writing to string cannot fail");
        }
        rendered.push_str("</tr>");
    }
    rendered.push_str("</tbody></table>");
    rendered
}

fn render_bases_task_list_html(view: &BasesEvaluatedView) -> String {
    let mut rendered = String::from("<ul class=\"bases-task-list\">");
    for row in &view.rows {
        rendered.push_str(&render_bases_row_item_html(row));
    }
    rendered.push_str("</ul>");
    rendered
}

fn render_bases_kanban_html(view: &BasesEvaluatedView) -> String {
    let mut grouped = HashMap::<String, Vec<&BasesRow>>::new();
    for row in &view.rows {
        let key = row
            .group_value
            .as_ref()
            .map(render_plain_json_value)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Ungrouped".to_string());
        grouped.entry(key).or_default().push(row);
    }
    let mut keys = grouped.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    let mut rendered = String::from("<div class=\"bases-kanban\">");
    for key in keys {
        rendered.push_str("<section class=\"bases-kanban-column\">");
        write!(rendered, "<h5>{}</h5><ul>", escape_html_text(&key))
            .expect("writing to string cannot fail");
        for row in grouped.remove(&key).unwrap_or_default() {
            rendered.push_str(&render_bases_row_item_html(row));
        }
        rendered.push_str("</ul></section>");
    }
    rendered.push_str("</div>");
    rendered
}

fn render_bases_row_item_html(row: &BasesRow) -> String {
    let text = row
        .cells
        .values()
        .find(|value| !value.is_null())
        .map(render_plain_json_value)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| row.file_name.clone());
    format!(
        "<li><span class=\"bases-row-text\">{}</span> <span class=\"bases-row-path\">{}</span></li>",
        escape_html_text(&text),
        escape_html_text(&row.document_path)
    )
}

fn render_dql_table_html(
    paths: &VaultPaths,
    result: &DqlQueryResult,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    let headers = result.columns.clone();
    let rows = result
        .rows
        .iter()
        .map(|row| {
            headers
                .iter()
                .map(|column| {
                    render_json_value_html(
                        paths,
                        &row[column],
                        source_path,
                        env,
                        state,
                        diagnostics,
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    render_html_table_from_cells(&headers, &rows, "dql-table")
}

fn render_dql_list_html(
    paths: &VaultPaths,
    result: &DqlQueryResult,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    if result.rows.is_empty() {
        return String::new();
    }
    let mut rendered = String::from("<ul class=\"dql-list\">");
    for row in &result.rows {
        let item_html = match result.columns.as_slice() {
            [column] => {
                render_json_value_html(paths, &row[column], source_path, env, state, diagnostics)
            }
            [left, right, ..] => format!(
                "{}: {}",
                render_json_value_html(paths, &row[left], source_path, env, state, diagnostics),
                render_json_value_html(paths, &row[right], source_path, env, state, diagnostics)
            ),
            [] => escape_html_text(&serde_json::to_string(row).unwrap_or_default()),
        };
        write!(rendered, "<li>{item_html}</li>").expect("writing to string cannot fail");
    }
    rendered.push_str("</ul>");
    rendered
}

fn render_dql_task_html(
    paths: &VaultPaths,
    result: &DqlQueryResult,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    if result.rows.is_empty() {
        return String::new();
    }
    let file_column = result.columns.first().map_or("File", String::as_str);
    let mut current_file = None::<String>;
    let mut rendered = String::from("<div class=\"dql-task-groups\">");
    for row in &result.rows {
        let file = row[file_column].as_str().unwrap_or_default().to_string();
        if current_file.as_deref() != Some(file.as_str()) {
            if current_file.is_some() {
                rendered.push_str("</ul></section>");
            }
            current_file = Some(file.clone());
            write!(
                rendered,
                "<section class=\"dql-task-group\"><h4>{}</h4><ul>",
                escape_html_text(&file)
            )
            .expect("writing to string cannot fail");
        }
        let status = row["status"].as_str().unwrap_or(" ");
        let text =
            render_json_value_html(paths, &row["text"], source_path, env, state, diagnostics);
        write!(
            rendered,
            "<li><span class=\"task-status\">[{status}]</span> {text}</li>"
        )
        .expect("writing to string cannot fail");
    }
    if current_file.is_some() {
        rendered.push_str("</ul></section>");
    }
    rendered.push_str("</div>");
    rendered
}

fn render_dql_calendar_html(
    paths: &VaultPaths,
    result: &DqlQueryResult,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    if result.rows.is_empty() {
        return "<p>No calendar entries.</p>".to_string();
    }
    let file_column = result.columns.get(1).map_or("File", String::as_str);
    let mut current_date = None::<String>;
    let mut rendered = String::from("<div class=\"dql-calendar-groups\">");
    for row in &result.rows {
        let date = row["date"].as_str().unwrap_or_default().to_string();
        if current_date.as_deref() != Some(date.as_str()) {
            if current_date.is_some() {
                rendered.push_str("</ul></section>");
            }
            current_date = Some(date.clone());
            write!(
                rendered,
                "<section class=\"dql-calendar-group\"><h4>{}</h4><ul>",
                escape_html_text(&date)
            )
            .expect("writing to string cannot fail");
        }
        let text = render_json_value_html(
            paths,
            &row[file_column],
            source_path,
            env,
            state,
            diagnostics,
        );
        write!(rendered, "<li>{text}</li>").expect("writing to string cannot fail");
    }
    if current_date.is_some() {
        rendered.push_str("</ul></section>");
    }
    rendered.push_str("</div>");
    rendered
}

#[allow(clippy::format_collect)]
fn render_dql_diagnostics_html(diagnostics: &[DqlDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return String::new();
    }
    let items = diagnostics
        .iter()
        .map(|diagnostic| format!("<li>{}</li>", escape_html_text(&diagnostic.message)))
        .collect::<String>();
    format!("<section class=\"dql-diagnostics\"><h4>Diagnostics</h4><ul>{items}</ul></section>")
}

fn render_value_table_html(
    paths: &VaultPaths,
    headers: &[String],
    rows: &[Vec<Value>],
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    let cells = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|value| {
                    render_json_value_html(paths, value, source_path, env, state, diagnostics)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    render_html_table_from_cells(headers, &cells, "dataview-table")
}

fn render_value_list_html(
    paths: &VaultPaths,
    values: &[Value],
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    let mut rendered = String::from("<ul class=\"dataview-list\">");
    for value in values {
        write!(
            rendered,
            "<li>{}</li>",
            render_json_value_html(paths, value, source_path, env, state, diagnostics)
        )
        .expect("writing to string cannot fail");
    }
    rendered.push_str("</ul>");
    rendered
}

fn render_task_values_html(
    paths: &VaultPaths,
    tasks: &[Value],
    group_by_file: bool,
    source_path: Option<&str>,
    env: &HtmlRenderEnvironment<'_>,
    state: &mut HtmlRenderState,
    diagnostics: &mut Vec<HtmlRenderDiagnostic>,
) -> String {
    if tasks.is_empty() {
        return String::new();
    }
    let mut rendered = String::new();
    let mut current_file = None::<String>;
    if group_by_file {
        rendered.push_str("<div class=\"dataview-task-groups\">");
    } else {
        rendered.push_str("<ul class=\"dataview-task-list\">");
    }
    for task in tasks {
        let file = task
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| {
                task.get("file")
                    .and_then(|file| file.get("path"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("<unknown>")
            .to_string();
        if group_by_file && current_file.as_deref() != Some(file.as_str()) {
            if current_file.is_some() {
                rendered.push_str("</ul></section>");
            }
            current_file = Some(file.clone());
            write!(
                rendered,
                "<section class=\"dataview-task-group\"><h4>{}</h4><ul>",
                escape_html_text(&file)
            )
            .expect("writing to string cannot fail");
        }
        let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
        let text = task.get("text").cloned().unwrap_or(Value::Null);
        let text_html = render_json_value_html(paths, &text, source_path, env, state, diagnostics);
        write!(
            rendered,
            "<li><span class=\"task-status\">[{status}]</span> {text_html}</li>"
        )
        .expect("writing to string cannot fail");
    }
    if group_by_file {
        if current_file.is_some() {
            rendered.push_str("</ul></section>");
        }
        rendered.push_str("</div>");
    } else {
        rendered.push_str("</ul>");
    }
    rendered
}

fn render_generic_element_html(
    element: &str,
    text: &str,
    attrs: &serde_json::Map<String, Value>,
) -> String {
    let tag = element
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect::<String>();
    if tag.is_empty() {
        return escape_html_text(text);
    }
    let rendered_attrs = attrs
        .iter()
        .filter_map(|(key, value)| {
            let value = value.as_str()?;
            Some(format!(
                " {}=\"{}\"",
                escape_html_text(key),
                escape_html_text(value)
            ))
        })
        .collect::<String>();
    format!("<{tag}{rendered_attrs}>{}</{tag}>", escape_html_text(text))
}

fn render_markdown_html_with_targets(
    source: &str,
    source_path: Option<&str>,
    full_document: bool,
    link_targets: &HtmlLinkTargets,
    raw_html_policy: HtmlRawHtmlPolicy,
) -> RenderedMarkdownHtml {
    let empty_path = String::new();
    let source_path = source_path.unwrap_or(&empty_path);
    let options = if full_document {
        parser_options()
    } else {
        fragment_parser_options()
    };
    let mut rendered_html = RenderedMarkdownHtml::default();
    let parser = MarkdownParser::new_ext(source, options).map(|event| match event {
        MarkdownEvent::Start(MarkdownTag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => MarkdownEvent::Start(MarkdownTag::Link {
            link_type,
            dest_url: rewrite_link_destination(
                source_path,
                &dest_url,
                &link_targets.note_hrefs,
                &link_targets.asset_hrefs,
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
            dest_url: rewrite_image_destination(source_path, &dest_url, &link_targets.asset_hrefs)
                .map(CowStr::from)
                .unwrap_or(dest_url),
            title,
            id,
        }),
        MarkdownEvent::Html(raw) => rewrite_raw_html_event(
            raw,
            raw_html_policy,
            &mut rendered_html.raw_html_events,
            &mut rendered_html.sanitized_raw_html_events,
            &mut rendered_html.stripped_raw_html_events,
            true,
        ),
        MarkdownEvent::InlineHtml(raw) => rewrite_raw_html_event(
            raw,
            raw_html_policy,
            &mut rendered_html.raw_html_events,
            &mut rendered_html.sanitized_raw_html_events,
            &mut rendered_html.stripped_raw_html_events,
            false,
        ),
        other => other,
    });

    html::push_html(&mut rendered_html.html, parser);
    rendered_html
}

fn rewrite_raw_html_event<'a>(
    raw: CowStr<'a>,
    policy: HtmlRawHtmlPolicy,
    raw_html_events: &mut usize,
    sanitized_events: &mut usize,
    stripped_events: &mut usize,
    block: bool,
) -> MarkdownEvent<'a> {
    *raw_html_events += 1;
    match policy {
        HtmlRawHtmlPolicy::Passthrough => {
            if block {
                MarkdownEvent::Html(raw)
            } else {
                MarkdownEvent::InlineHtml(raw)
            }
        }
        HtmlRawHtmlPolicy::Sanitize => {
            let sanitized = sanitize_html_fragment(raw.as_ref());
            if sanitized != raw.as_ref() {
                *sanitized_events += 1;
            }
            if block {
                MarkdownEvent::Html(CowStr::from(sanitized))
            } else {
                MarkdownEvent::InlineHtml(CowStr::from(sanitized))
            }
        }
        HtmlRawHtmlPolicy::Strip => {
            *stripped_events += 1;
            MarkdownEvent::Text(CowStr::from(String::new()))
        }
    }
}

fn rewrite_link_destination(
    source_document_path: &str,
    destination: &str,
    note_targets: &HashMap<String, String>,
    asset_targets: &HashMap<String, String>,
) -> Option<String> {
    if destination.is_empty() || is_external_href(destination) {
        return None;
    }
    let (path_part, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    resolve_note_href(source_document_path, path_part, fragment, note_targets)
        .or_else(|| resolve_asset_href(source_document_path, path_part, fragment, asset_targets))
}

fn rewrite_image_destination(
    source_document_path: &str,
    destination: &str,
    asset_targets: &HashMap<String, String>,
) -> Option<String> {
    if destination.is_empty() || is_external_href(destination) {
        return None;
    }
    let (path_part, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    resolve_asset_href(source_document_path, path_part, fragment, asset_targets)
}

fn collect_headings(parsed: &ParsedDocument) -> Vec<HtmlRenderHeading> {
    let mut seen = HashMap::<String, usize>::new();
    parsed
        .headings
        .iter()
        .map(|heading| {
            let slug = slugify_fragment(&heading.text);
            let count = seen.entry(slug.clone()).or_insert(0);
            *count += 1;
            let id = if *count == 1 {
                slug
            } else {
                format!("{slug}-{}", *count)
            };
            HtmlRenderHeading {
                level: heading.level,
                text: heading.text.clone(),
                id,
            }
        })
        .collect()
}

fn inject_heading_ids(html: &str, headings: &[HtmlRenderHeading]) -> String {
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
            heading.level,
            escape_html_text(&heading.id)
        )
        .expect("writing to string cannot fail");
        cursor = start + needle.len();
    }
    rendered.push_str(&html[cursor..]);
    rendered
}

fn strip_paragraph_wrapper(html: &str) -> String {
    let trimmed = html.trim();
    trimmed
        .strip_prefix("<p>")
        .and_then(|value| value.strip_suffix("</p>"))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn render_html_table_from_cells(
    headers: &[String],
    rows: &[Vec<String>],
    class_name: &str,
) -> String {
    let mut rendered = format!(
        "<table class=\"{}\"><thead><tr>",
        escape_html_text(class_name)
    );
    for header in headers {
        write!(rendered, "<th>{}</th>", escape_html_text(header))
            .expect("writing to string cannot fail");
    }
    rendered.push_str("</tr></thead><tbody>");
    for row in rows {
        rendered.push_str("<tr>");
        for cell in row {
            write!(rendered, "<td>{cell}</td>").expect("writing to string cannot fail");
        }
        rendered.push_str("</tr>");
    }
    rendered.push_str("</tbody></table>");
    rendered
}

fn apply_markdown_replacements(
    source: &str,
    mut replacements: Vec<HtmlMarkdownReplacement>,
) -> String {
    replacements.sort_by(|left, right| left.start.cmp(&right.start).then(right.end.cmp(&left.end)));
    let mut rendered = String::new();
    let mut cursor = 0_usize;
    for replacement in replacements {
        if replacement.start < cursor
            || replacement.end < replacement.start
            || replacement.end > source.len()
        {
            continue;
        }
        rendered.push_str(&source[cursor..replacement.start]);
        rendered.push_str(&replacement.replacement);
        cursor = replacement.end;
    }
    rendered.push_str(&source[cursor..]);
    rendered
}

fn build_resolver_documents(note_index: &HashMap<String, NoteRecord>) -> Vec<ResolverDocument> {
    let mut documents = note_index
        .values()
        .map(|note| ResolverDocument {
            id: note.document_id.clone(),
            path: note.document_path.clone(),
            filename: note.file_name.clone(),
            aliases: note.aliases.clone(),
        })
        .collect::<Vec<_>>();
    documents.sort_by(|left, right| left.path.cmp(&right.path));
    documents
}

fn note_by_path<'a>(
    note_index: &'a HashMap<String, NoteRecord>,
    path: &str,
) -> Option<&'a NoteRecord> {
    note_index.values().find(|note| note.document_path == path)
}

fn resolve_embedded_note_path(
    source_path: &str,
    link: &crate::RawLink,
    env: &HtmlRenderEnvironment<'_>,
) -> Option<String> {
    let note_index = env.note_index?;
    let source_note = note_by_path(note_index, source_path)?;
    let documents = env.resolver_documents?;
    let resolution = resolve_link(
        documents,
        &ResolverLink {
            source_document_id: source_note.document_id.clone(),
            source_path: source_path.to_string(),
            target_path_candidate: link.target_path_candidate.clone(),
            link_kind: LinkKind::Embed,
        },
        env.config.link_resolution,
    );
    let target_id = resolution.resolved_target_id?;
    documents
        .iter()
        .find(|document| document.id == target_id)
        .map(|document| document.path.clone())
}

fn resolve_relative_asset_path(source_document_path: Option<&str>, target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let source_document_path = source_document_path?;
    let source_dir = Path::new(source_document_path).parent()?;
    Some(normalize_target_path(
        &source_dir.join(target).to_string_lossy(),
    ))
}

fn normalize_target_path(path: &str) -> String {
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

fn resolve_lookup_keys(source_document_path: &str, path_part: &str) -> Vec<String> {
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
    let normalized_direct = normalize_target_path(&direct);
    push_key(normalized_direct.clone());

    if trimmed.starts_with('.') || trimmed.contains('/') {
        if let Some(source_dir) = Path::new(source_document_path).parent() {
            let joined = normalize_target_path(&source_dir.join(trimmed).to_string_lossy());
            push_key(joined);
        }
    }

    keys
}

fn resolve_note_href(
    source_document_path: &str,
    path_part: &str,
    fragment: Option<&str>,
    targets: &HashMap<String, String>,
) -> Option<String> {
    for key in resolve_lookup_keys(source_document_path, path_part) {
        let Some(target) = targets
            .get(&key)
            .or_else(|| key.strip_suffix(".md").and_then(|stem| targets.get(stem)))
        else {
            continue;
        };
        let mut rewritten = target.clone();
        if let Some(fragment) = fragment
            .map(slugify_fragment)
            .filter(|value| !value.is_empty())
        {
            rewritten.push('#');
            rewritten.push_str(&fragment);
        }
        return Some(rewritten);
    }
    None
}

fn resolve_asset_href(
    source_document_path: &str,
    path_part: &str,
    fragment: Option<&str>,
    targets: &HashMap<String, String>,
) -> Option<String> {
    for key in resolve_lookup_keys(source_document_path, path_part) {
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

fn slugify_fragment(text: &str) -> String {
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

fn is_external_href(destination: &str) -> bool {
    destination.starts_with('#')
        || destination.starts_with("mailto:")
        || destination.starts_with("tel:")
        || destination.starts_with("obsidian:")
        || destination.contains("://")
}

fn render_message_html(title: &str, message: &str) -> String {
    format!(
        "<div class=\"render-message\"><strong>{}</strong> {}</div>",
        escape_html_text(title),
        escape_html_text(message)
    )
}

fn escape_html_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn render_plain_json_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn parse_diagnostic_kind_name(kind: ParseDiagnosticKind) -> &'static str {
    match kind {
        ParseDiagnosticKind::HtmlLink => "html_link",
        ParseDiagnosticKind::LinkInComment => "link_in_comment",
        ParseDiagnosticKind::MalformedFrontmatter => "malformed_frontmatter",
        ParseDiagnosticKind::UnsupportedSyntax => "unsupported_syntax",
    }
}

fn select_render_title(
    source_path: Option<&str>,
    headings: &[HtmlRenderHeading],
) -> Option<String> {
    headings
        .iter()
        .find(|heading| heading.level == 1)
        .map(|heading| heading.text.clone())
        .or_else(|| {
            source_path.and_then(|path| {
                Path::new(path)
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(ToOwned::to_owned)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::{
        render_note_fragment_html, render_note_html, render_vault_html, HtmlDataviewJsPolicy,
        HtmlRawHtmlPolicy, HtmlRenderOptions,
    };
    use crate::{scan_vault, ScanMode, VaultPaths};
    use std::fs;
    use tempfile::TempDir;

    fn build_render_vault() -> (TempDir, VaultPaths) {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should exist");
        fs::create_dir_all(vault_root.join("assets")).expect("assets dir should exist");
        fs::write(
            vault_root.join("Dashboard.md"),
            concat!(
                "---\nstatus: draft\n---\n\n",
                "# Dashboard\n\n",
                "owner:: [[People/Alice]]\n\n",
                "![[assets/logo.png]]\n\n",
                "```dataview\n",
                "TABLE file.name AS Name FROM \"People\"\n",
                "```\n\n",
                "```tasks\n",
                "not done\n",
                "```\n",
            ),
        )
        .expect("dashboard should write");
        fs::create_dir_all(vault_root.join("People")).expect("people dir");
        fs::write(
            vault_root.join("People/Alice.md"),
            "# Alice\n\n- [ ] Follow up\n",
        )
        .expect("alice should write");
        fs::write(vault_root.join("assets/logo.png"), b"logo").expect("asset should write");
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        (temp_dir, paths)
    }

    #[test]
    fn note_html_renders_headings_assets_and_dynamic_blocks() {
        let (_temp_dir, paths) = build_render_vault();
        let source = fs::read_to_string(paths.vault_root().join("Dashboard.md"))
            .expect("dashboard should read");

        let rendered = render_note_html(&paths, "Dashboard.md", &source);

        assert!(rendered
            .html
            .contains("<h1 id=\"dashboard\">Dashboard</h1>"));
        assert!(rendered
            .html
            .contains("class=\"asset-embed asset-embed-image\""));
        assert!(rendered.html.contains("Alice"));
        assert!(rendered.html.contains("class=\"tasks-query-list\""));
        assert!(
            !rendered.html.contains("status: draft"),
            "frontmatter should stay out of rendered html"
        );
    }

    #[test]
    fn fragment_html_keeps_fragment_semantics() {
        let (_temp_dir, paths) = build_render_vault();
        let rendered = render_note_fragment_html(&paths, Some("Dashboard.md"), "---\n\nAfter");

        assert!(rendered.html.contains("<hr />"));
        assert!(rendered.html.contains("<p>After</p>"));
    }

    #[test]
    fn dataviewjs_static_policy_can_render_js_output() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should exist");
        fs::write(
            vault_root.join("Home.md"),
            concat!(
                "# Home\n\n",
                "```dataviewjs\n",
                "dv.paragraph('Hello from JS')\n",
                "```\n",
            ),
        )
        .expect("home should write");
        let paths = VaultPaths::new(&vault_root);
        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
        let source =
            fs::read_to_string(paths.vault_root().join("Home.md")).expect("home should read");

        let rendered = render_vault_html(
            &paths,
            &source,
            &HtmlRenderOptions {
                source_path: Some("Home.md"),
                dataview_js_policy: HtmlDataviewJsPolicy::Static,
                ..HtmlRenderOptions::default()
            },
        );

        assert!(rendered.html.contains("Hello from JS"));
    }

    #[test]
    fn raw_html_passthrough_is_the_default_policy() {
        let (_temp_dir, paths) = build_render_vault();
        let rendered = render_vault_html(
            &paths,
            "<aside class=\"callout\">Visible</aside><script>alert('x')</script>",
            &HtmlRenderOptions::default(),
        );

        assert!(rendered
            .html
            .contains("<aside class=\"callout\">Visible</aside>"));
        assert!(rendered.html.contains("<script>alert('x')</script>"));
        assert!(rendered.diagnostics.is_empty());
    }

    #[test]
    fn raw_html_sanitize_policy_strips_unsafe_tags_and_reports_it() {
        let (_temp_dir, paths) = build_render_vault();
        let rendered = render_vault_html(
            &paths,
            "<div class=\"callout\">Visible</div><script>alert('x')</script>",
            &HtmlRenderOptions {
                raw_html_policy: HtmlRawHtmlPolicy::Sanitize,
                ..HtmlRenderOptions::default()
            },
        );

        assert!(rendered.html.contains("Visible"));
        assert!(!rendered.html.contains("<script>alert('x')</script>"));
        assert!(rendered
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == "raw_html_sanitized"));
    }

    #[test]
    fn raw_html_strip_policy_removes_fragments_and_reports_it() {
        let (_temp_dir, paths) = build_render_vault();
        let rendered = render_vault_html(
            &paths,
            "Before <div class=\"callout\">Visible</div> After",
            &HtmlRenderOptions {
                raw_html_policy: HtmlRawHtmlPolicy::Strip,
                ..HtmlRenderOptions::default()
            },
        );

        assert!(rendered.html.contains("Before"));
        assert!(rendered.html.contains("After"));
        assert!(!rendered.html.contains("callout"));
        assert!(rendered.html.contains("Visible"));
        assert!(rendered
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == "raw_html_stripped"));
    }
}
