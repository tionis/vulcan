//! Shared evaluation of a note's dynamic content.
//!
//! Inline Dataview expressions, `dataview`/`dataviewjs`/`tasks` blocks, and
//! `.base` embeds are evaluated here once, under one `DataviewJS` policy and one
//! read authority. Renderers only decide how each [`DynamicOutcome`] looks:
//! the HTML renderer turns it into markup, and [`render_evaluated_markdown`]
//! turns it into plain Markdown for terminals, previews, and agents.

use crate::bases::{BasesEvalReport, BasesEvaluatedView, BasesRow};
use crate::config::{load_vault_config, JsRuntimeSandbox, VaultConfig};
use crate::dataview_js::{DataviewJsOutput, DataviewJsResult};
use crate::dql::{
    evaluate_dql_with_guard, evaluate_dql_with_note_index_and_config, DqlDiagnostic,
    DqlQueryResult, DqlQueryType,
};
use crate::parser::{LinkKind, ParsedDocument};
use crate::paths::VaultPaths;
use crate::permissions::PermissionGuard;
use crate::properties::{
    evaluate_note_inline_expressions, load_note_index, load_note_index_with_guard, NoteRecord,
};
use crate::resolver::ResolverDocument;
use crate::tasks::{evaluate_tasks_query_with_note_index, TasksQueryResult};
use crate::{
    evaluate_base_file, evaluate_base_file_with_guard, evaluate_dataview_js_with_options,
    evaluate_dql_with_filter, evaluate_tasks_query, parse_document,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

/// How a render treats `dataviewjs` blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataviewJsRenderPolicy {
    /// Leave blocks unevaluated and render a visible "disabled" notice.
    #[default]
    Off,
    /// Evaluate in the strict sandbox with determinism constraints, as static
    /// publication does.
    Static,
    /// Evaluate with the vault's configured sandbox tier and the caller's
    /// permission profile, as `dataview eval` does, except that writes and
    /// host execution are denied: rendering never changes the vault.
    On,
}

impl DataviewJsRenderPolicy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Static => "static",
            Self::On => "on",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderDiagnostic {
    pub kind: String,
    pub message: String,
}

/// Everything dynamic evaluation reads besides the note itself.
#[derive(Clone, Copy)]
pub(crate) struct DynamicContentContext<'a> {
    pub paths: &'a VaultPaths,
    pub config: &'a VaultConfig,
    /// The caller's note universe; loaded under `guard` when one is set.
    pub note_index: Option<&'a HashMap<String, NoteRecord>>,
    pub guard: Option<&'a dyn PermissionGuard>,
    pub permission_profile: Option<&'a str>,
    pub dataview_js: DataviewJsRenderPolicy,
}

/// The evaluated result of one dynamic region.
#[derive(Debug, Clone)]
pub(crate) enum DynamicOutcome {
    InlineValue(Value),
    /// An inline expression that failed to evaluate.
    InlineError(String),
    /// An inline expression that could not be evaluated because the note is
    /// not indexed.
    Unevaluated(String),
    /// The result of an inline `DataviewJS` expression.
    InlineJs(DataviewJsResult),
    /// An inline `DataviewJS` expression while `DataviewJS` is turned off.
    InlineJsDisabled,
    /// An Obsidian comment, which renders as nothing.
    Hidden,
    Dql(DqlQueryResult),
    DataviewJs(DataviewJsResult),
    Tasks(TasksQueryResult),
    Base(BasesEvalReport),
    Disabled {
        title: String,
        message: String,
    },
    Error {
        title: String,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct DynamicRegion {
    pub start: usize,
    pub end: usize,
    pub outcome: DynamicOutcome,
}

pub(crate) fn evaluate_dynamic_regions(
    ctx: &DynamicContentContext<'_>,
    source_path: Option<&str>,
    parsed: &ParsedDocument,
    diagnostics: &mut Vec<RenderDiagnostic>,
) -> Vec<DynamicRegion> {
    let mut regions = Vec::new();

    let note = source_path.zip(ctx.note_index).and_then(|(path, index)| {
        index
            .values()
            .find(|note| note.document_path == path)
            .map(|note| (note, index))
    });
    // Results are keyed by expression text rather than position so a
    // partial selection of the note still pairs each expression correctly.
    let inline_results = note.map(|(note, index)| evaluate_note_inline_expressions(note, index));
    let results_by_expression = inline_results
        .iter()
        .flatten()
        .map(|result| (result.expression.as_str(), result))
        .collect::<HashMap<_, _>>();
    for expression in &parsed.inline_expressions {
        let outcome = match results_by_expression.get(expression.expression.as_str()) {
            Some(evaluated) => evaluated.error.as_ref().map_or_else(
                || DynamicOutcome::InlineValue(evaluated.value.clone()),
                |error| DynamicOutcome::InlineError(error.clone()),
            ),
            None => DynamicOutcome::Unevaluated(expression.expression.clone()),
        };
        regions.push(DynamicRegion {
            start: expression.byte_range.start,
            end: expression.byte_range.end,
            outcome,
        });
    }

    for comment in &parsed.comment_regions {
        regions.push(DynamicRegion {
            start: comment.start,
            end: comment.end,
            outcome: DynamicOutcome::Hidden,
        });
    }

    push_inline_js_regions(ctx, source_path, parsed, diagnostics, &mut regions);

    for block in &parsed.dataview_blocks {
        regions.push(DynamicRegion {
            start: block.byte_range.start,
            end: block.byte_range.end,
            outcome: evaluate_dataview_block(
                ctx,
                source_path,
                &block.language,
                &block.text,
                diagnostics,
            ),
        });
    }

    for block in &parsed.tasks_blocks {
        regions.push(DynamicRegion {
            start: block.byte_range.start,
            end: block.byte_range.end,
            outcome: evaluate_tasks_block(ctx, &block.text),
        });
    }

    for link in &parsed.links {
        if link.link_kind != LinkKind::Embed {
            continue;
        }
        let Some(candidate) = link.target_path_candidate.as_deref() else {
            continue;
        };
        if !is_base_path(candidate) {
            continue;
        }
        regions.push(DynamicRegion {
            start: link.byte_offset,
            end: link.byte_offset + link.raw_text.len(),
            outcome: evaluate_base_embed(
                ctx,
                source_path,
                candidate,
                link.target_heading.as_deref(),
            ),
        });
    }

    regions
}

/// An inline field as Dataview's reading view shows it: a bold key and its
/// value, or only the value for a parenthesized field.
fn inline_field_markdown(source: &str, field: &crate::parser::RawInlineField) -> String {
    if field.kind == crate::parser::types::InlineFieldKind::Parenthesized {
        return field.value_text.clone();
    }
    // The parser lowercases keys; show the key as written.
    let written = source
        .get(field.byte_range.start..field.value_byte_range.start)
        .map(|text| {
            text.trim()
                .trim_start_matches(['[', '('])
                .trim_end_matches(':')
                .trim()
        })
        .filter(|key| !key.is_empty())
        .unwrap_or(&field.key);
    if written.starts_with("**") || written.starts_with("__") {
        format!("{written}: {}", field.value_text)
    } else {
        format!("**{written}**: {}", field.value_text)
    }
}

fn has_dynamic_content(parsed: &ParsedDocument) -> bool {
    !parsed.inline_fields.is_empty()
        || !parsed.inline_expressions.is_empty()
        || !parsed.inline_js_expressions.is_empty()
        || !parsed.comment_regions.is_empty()
        || !parsed.dataview_blocks.is_empty()
        || !parsed.tasks_blocks.is_empty()
        || parsed.links.iter().any(|link| {
            link.link_kind == LinkKind::Embed
                && (link.is_note_embed
                    || link
                        .target_path_candidate
                        .as_deref()
                        .is_some_and(is_base_path))
        })
}

/// Evaluate inline `DataviewJS` expressions. Like Dataview, they run only
/// where the vault enables them.
fn push_inline_js_regions(
    ctx: &DynamicContentContext<'_>,
    source_path: Option<&str>,
    parsed: &ParsedDocument,
    diagnostics: &mut Vec<RenderDiagnostic>,
    regions: &mut Vec<DynamicRegion>,
) {
    if !ctx.config.dataview.enable_inline_dataview_js {
        return;
    }
    for expression in &parsed.inline_js_expressions {
        let outcome = match dataview_js_options(ctx) {
            None => {
                diagnostics.push(RenderDiagnostic {
                    kind: "dataviewjs_disabled".to_string(),
                    message: "DataviewJS rendering is disabled for this render".to_string(),
                });
                DynamicOutcome::InlineJsDisabled
            }
            Some(options) => evaluate_dataview_js_with_options(
                ctx.paths,
                &expression.expression,
                source_path,
                options,
            )
            .map_or_else(
                |error| DynamicOutcome::InlineError(error.to_string()),
                DynamicOutcome::InlineJs,
            ),
        };
        regions.push(DynamicRegion {
            start: expression.byte_range.start,
            end: expression.byte_range.end,
            outcome,
        });
    }
}

pub(crate) fn is_base_path(candidate: &str) -> bool {
    Path::new(candidate)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
}

fn evaluate_dataview_block(
    ctx: &DynamicContentContext<'_>,
    source_path: Option<&str>,
    language: &str,
    source: &str,
    diagnostics: &mut Vec<RenderDiagnostic>,
) -> DynamicOutcome {
    match language {
        "dataview" => {
            let result = match (ctx.guard, ctx.note_index) {
                (Some(guard), _) => evaluate_dql_with_guard(ctx.paths, source, source_path, guard),
                (None, Some(note_index)) => evaluate_dql_with_note_index_and_config(
                    ctx.paths,
                    source,
                    source_path,
                    None,
                    ctx.config,
                    note_index,
                ),
                (None, None) => evaluate_dql_with_filter(ctx.paths, source, source_path, None),
            };
            result.map_or_else(
                |error| DynamicOutcome::Error {
                    title: "Dataview error:".to_string(),
                    message: error.to_string(),
                },
                DynamicOutcome::Dql,
            )
        }
        "dataviewjs" => evaluate_dataview_js_block(ctx, source_path, source, diagnostics),
        other => DynamicOutcome::Error {
            title: "Dataview error:".to_string(),
            message: format!("unsupported block language `{other}`"),
        },
    }
}

fn evaluate_dataview_js_block(
    ctx: &DynamicContentContext<'_>,
    source_path: Option<&str>,
    source: &str,
    diagnostics: &mut Vec<RenderDiagnostic>,
) -> DynamicOutcome {
    let Some(options) = dataview_js_options(ctx) else {
        diagnostics.push(RenderDiagnostic {
            kind: "dataviewjs_disabled".to_string(),
            message: "DataviewJS rendering is disabled for this render".to_string(),
        });
        return DynamicOutcome::Disabled {
            title: "DataviewJS disabled:".to_string(),
            message: "DataviewJS evaluation is turned off for this render".to_string(),
        };
    };
    evaluate_dataview_js_with_options(ctx.paths, source, source_path, options).map_or_else(
        |error| DynamicOutcome::Error {
            title: "DataviewJS error:".to_string(),
            message: error.to_string(),
        },
        DynamicOutcome::DataviewJs,
    )
}

/// Evaluation options for the render's `DataviewJS` policy; `None` when it
/// is off. Renders are always read-only.
fn dataview_js_options(ctx: &DynamicContentContext<'_>) -> Option<crate::DataviewJsEvalOptions> {
    match ctx.dataview_js {
        DataviewJsRenderPolicy::Off => None,
        DataviewJsRenderPolicy::Static => Some(crate::DataviewJsEvalOptions {
            sandbox: Some(JsRuntimeSandbox::Strict),
            deterministic_static: true,
            read_only: true,
            permission_profile: ctx.permission_profile.map(ToOwned::to_owned),
            ..crate::DataviewJsEvalOptions::default()
        }),
        DataviewJsRenderPolicy::On => Some(crate::DataviewJsEvalOptions {
            permission_profile: ctx.permission_profile.map(ToOwned::to_owned),
            read_only: true,
            ..crate::DataviewJsEvalOptions::default()
        }),
    }
}

fn evaluate_tasks_block(ctx: &DynamicContentContext<'_>, source: &str) -> DynamicOutcome {
    let result = match ctx.note_index {
        Some(note_index) => evaluate_tasks_query_with_note_index(source, note_index),
        // Without a loaded universe a guarded render must not fall back to the
        // whole vault.
        None if ctx.guard.is_some() => {
            return DynamicOutcome::Error {
                title: "Tasks error:".to_string(),
                message: "the note index is unavailable for this permission profile".to_string(),
            }
        }
        None => evaluate_tasks_query(ctx.paths, source),
    };
    result.map_or_else(
        |error| DynamicOutcome::Error {
            title: "Tasks error:".to_string(),
            message: error.to_string(),
        },
        DynamicOutcome::Tasks,
    )
}

fn evaluate_base_embed(
    ctx: &DynamicContentContext<'_>,
    source_path: Option<&str>,
    base_path: &str,
    view_name: Option<&str>,
) -> DynamicOutcome {
    let resolved_path = crate::html::resolve_relative_asset_path(source_path, base_path)
        .unwrap_or_else(|| base_path.to_string());
    let report = match ctx.guard {
        Some(guard) => evaluate_base_file_with_guard(ctx.paths, &resolved_path, guard),
        None => evaluate_base_file(ctx.paths, &resolved_path),
    };
    match report {
        Ok(mut report) => {
            if let Some(view_name) = view_name.map(str::trim).filter(|value| !value.is_empty()) {
                let Some(view) = report
                    .views
                    .iter()
                    .find(|view| view.name.as_deref() == Some(view_name))
                    .cloned()
                else {
                    return DynamicOutcome::Error {
                        title: "Bases error:".to_string(),
                        message: format!("view `{view_name}` was not found in {resolved_path}"),
                    };
                };
                report.views = vec![view];
            }
            DynamicOutcome::Base(report)
        }
        Err(error) => DynamicOutcome::Error {
            title: "Bases error:".to_string(),
            message: error.to_string(),
        },
    }
}

/// Options for [`render_evaluated_markdown`].
#[derive(Clone, Copy, Default)]
pub struct EvaluatedMarkdownOptions<'a> {
    /// Vault-relative path of the note, which anchors `this`, relative
    /// embeds, and inline expressions.
    pub source_path: Option<&'a str>,
    pub dataview_js: DataviewJsRenderPolicy,
    /// Read authority; all queries see only notes this guard can read.
    pub guard: Option<&'a dyn PermissionGuard>,
    /// Permission profile name handed to `DataviewJS`.
    pub permission_profile: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvaluatedMarkdown {
    pub markdown: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<RenderDiagnostic>,
}

/// Holds the vault context so several notes can be rendered cheaply. The
/// note index loads on the first note that has dynamic content, so notes
/// without any never pay for it.
#[derive(Debug, Clone)]
pub struct EvaluatedMarkdownRenderer {
    paths: VaultPaths,
    config: VaultConfig,
    note_index: std::cell::OnceCell<Option<HashMap<String, NoteRecord>>>,
    resolver_documents: std::cell::OnceCell<Vec<ResolverDocument>>,
    guarded: bool,
}

/// How deep note embeds nest before the renderer stops transcluding, as in
/// the HTML renderer.
const MAX_EMBED_DEPTH: usize = 4;

impl EvaluatedMarkdownRenderer {
    /// Prepare a renderer for `guard`'s note universe, or the whole vault.
    /// Every render must pass the same guard.
    #[must_use]
    pub fn load(paths: &VaultPaths, guard: Option<&dyn PermissionGuard>) -> Self {
        Self {
            paths: paths.clone(),
            config: load_vault_config(paths).config,
            note_index: std::cell::OnceCell::new(),
            resolver_documents: std::cell::OnceCell::new(),
            guarded: guard.is_some(),
        }
    }

    /// Render `source` with every dynamic region replaced by its evaluated
    /// result as Markdown. Static content is left byte-for-byte unchanged.
    ///
    /// # Panics
    ///
    /// Panics when `options.guard` disagrees with the guard the renderer was
    /// loaded with, since the loaded note universe would not match.
    #[must_use]
    pub fn render(
        &self,
        source: &str,
        options: &EvaluatedMarkdownOptions<'_>,
    ) -> EvaluatedMarkdown {
        assert!(
            options.guard.is_some() == self.guarded,
            "a render must use the guard the renderer was loaded with"
        );
        self.render_nested(source, options.source_path, options, &mut Vec::new())
    }

    fn render_nested(
        &self,
        source: &str,
        source_path: Option<&str>,
        options: &EvaluatedMarkdownOptions<'_>,
        embed_stack: &mut Vec<String>,
    ) -> EvaluatedMarkdown {
        let parsed = parse_document(source, &self.config);
        if !has_dynamic_content(&parsed) {
            return EvaluatedMarkdown {
                markdown: source.to_string(),
                diagnostics: Vec::new(),
            };
        }
        let note_index = self.note_index.get_or_init(|| match options.guard {
            Some(guard) => load_note_index_with_guard(&self.paths, guard).ok(),
            None => load_note_index(&self.paths).ok(),
        });
        let ctx = DynamicContentContext {
            paths: &self.paths,
            config: &self.config,
            note_index: note_index.as_ref(),
            guard: options.guard,
            permission_profile: options.permission_profile,
            dataview_js: options.dataview_js,
        };
        let mut diagnostics = Vec::new();
        let mut replacements = parsed
            .inline_fields
            .iter()
            .map(|field| {
                (
                    field.byte_range.start,
                    field.byte_range.end,
                    MarkdownReplacement::Inline(inline_field_markdown(source, field)),
                )
            })
            .collect::<Vec<_>>();
        replacements.extend(
            evaluate_dynamic_regions(&ctx, source_path, &parsed, &mut diagnostics)
                .into_iter()
                .filter_map(|region| {
                    outcome_markdown(&region.outcome)
                        .map(|replacement| (region.start, region.end, replacement))
                }),
        );
        if let (Some(source_path), Some(note_index)) = (source_path, note_index.as_ref()) {
            for link in &parsed.links {
                if link.link_kind != LinkKind::Embed
                    || !link.is_note_embed
                    || link
                        .target_path_candidate
                        .as_deref()
                        .is_some_and(is_base_path)
                {
                    continue;
                }
                let body = self.note_embed_markdown(
                    source_path,
                    link,
                    note_index,
                    options,
                    embed_stack,
                    &mut diagnostics,
                );
                replacements.push((
                    link.byte_offset,
                    link.byte_offset + link.raw_text.len(),
                    MarkdownReplacement::Block(body),
                ));
            }
        }
        EvaluatedMarkdown {
            markdown: splice_markdown(source, replacements),
            diagnostics,
        }
    }

    /// Transclude one note embed as a callout holding the embedded note's
    /// evaluated content, or explain why it cannot be shown.
    fn note_embed_markdown(
        &self,
        source_path: &str,
        link: &crate::RawLink,
        note_index: &HashMap<String, NoteRecord>,
        options: &EvaluatedMarkdownOptions<'_>,
        embed_stack: &mut Vec<String>,
        diagnostics: &mut Vec<RenderDiagnostic>,
    ) -> String {
        let fail = |diagnostics: &mut Vec<RenderDiagnostic>, kind: &str, message: String| {
            let rendered = callout("warning", "Embed error", &message);
            diagnostics.push(RenderDiagnostic {
                kind: kind.to_string(),
                message,
            });
            rendered
        };
        let documents = self
            .resolver_documents
            .get_or_init(|| crate::html::build_resolver_documents(note_index));
        let Some(target_path) =
            crate::html::resolve_note_embed(&self.config, note_index, documents, source_path, link)
        else {
            return fail(
                diagnostics,
                "unresolved_embed",
                format!("could not resolve embedded note `{}`", link.raw_text),
            );
        };
        let key = format!(
            "{target_path}#{}^{}",
            link.target_heading.as_deref().unwrap_or_default(),
            link.target_block.as_deref().unwrap_or_default()
        );
        if embed_stack.len() >= MAX_EMBED_DEPTH {
            return fail(
                diagnostics,
                "embed_depth_limit",
                format!("embed depth limit reached at `{}`", link.raw_text),
            );
        }
        if embed_stack.contains(&key) {
            return fail(
                diagnostics,
                "embed_cycle",
                format!("embed cycle detected at `{}`", link.raw_text),
            );
        }
        let Ok(target_source) = std::fs::read_to_string(self.paths.vault_root().join(&target_path))
        else {
            return fail(
                diagnostics,
                "missing_embed_target",
                format!("embedded note `{target_path}` could not be read"),
            );
        };
        let target_parsed = parse_document(&target_source, &self.config);
        let selected = match crate::note::read_note(
            &target_source,
            &target_parsed,
            &crate::note::NoteReadOptions {
                heading: link.target_heading.clone(),
                block_ref: link.target_block.clone(),
                no_frontmatter: true,
                ..crate::note::NoteReadOptions::default()
            },
        ) {
            Ok(selection) => selection.content,
            Err(error) => return fail(diagnostics, "invalid_embed_target", error.to_string()),
        };
        embed_stack.push(key);
        let embedded = self.render_nested(&selected, Some(&target_path), options, embed_stack);
        embed_stack.pop();
        diagnostics.extend(embedded.diagnostics);

        let title = link
            .display_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map_or_else(
                || {
                    let name = target_path.strip_suffix(".md").unwrap_or(&target_path);
                    match (&link.target_heading, &link.target_block) {
                        (Some(heading), _) => format!("{name} > {heading}"),
                        (None, Some(block)) => format!("{name} ^{block}"),
                        (None, None) => name.to_string(),
                    }
                },
                ToOwned::to_owned,
            );
        let mut rendered = format!("> [!embed] {title}");
        for line in embedded.markdown.trim().lines() {
            rendered.push_str("\n>");
            if !line.is_empty() {
                rendered.push(' ');
                rendered.push_str(line);
            }
        }
        rendered
    }
}

/// Render one note's source with its dynamic content evaluated as Markdown.
#[must_use]
pub fn render_evaluated_markdown(
    paths: &VaultPaths,
    source: &str,
    options: &EvaluatedMarkdownOptions<'_>,
) -> EvaluatedMarkdown {
    EvaluatedMarkdownRenderer::load(paths, options.guard).render(source, options)
}

#[cfg(test)]
fn splice_markdown_regions(source: &str, regions: Vec<DynamicRegion>) -> String {
    splice_markdown(
        source,
        regions
            .into_iter()
            .filter_map(|region| {
                outcome_markdown(&region.outcome)
                    .map(|replacement| (region.start, region.end, replacement))
            })
            .collect(),
    )
}

/// Replace `(start, end)` byte ranges of `source`. Block replacements keep
/// the container prefix (quote markers, indentation) of the line they
/// start on.
fn splice_markdown(
    source: &str,
    mut replacements: Vec<(usize, usize, MarkdownReplacement)>,
) -> String {
    replacements.sort_by(|left, right| left.0.cmp(&right.0).then(right.1.cmp(&left.1)));
    let mut rendered = String::with_capacity(source.len());
    let mut cursor = 0_usize;
    for (start, end, replacement) in replacements {
        if start < cursor || end < start || end > source.len() {
            continue;
        }
        rendered.push_str(&source[cursor..start]);
        match replacement {
            MarkdownReplacement::Inline(text) => rendered.push_str(&text),
            MarkdownReplacement::Block(body) => {
                let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
                let lead = &source[line_start..start];
                // Blocks nested in quotes, callouts, or list items keep their
                // container prefix on every line.
                let prefix = if lead.chars().all(|ch| ch == '>' || ch.is_whitespace()) {
                    lead.to_string()
                } else {
                    rendered.push_str("\n\n");
                    String::new()
                };
                rendered.push_str(&indent_continuation(body.trim_end(), &prefix));
                if source[..end].ends_with('\n') {
                    rendered.push('\n');
                } else if !source[end..].starts_with('\n') {
                    rendered.push_str("\n\n");
                }
            }
        }
        cursor = end;
    }
    rendered.push_str(&source[cursor..]);
    rendered
}

fn indent_continuation(body: &str, prefix: &str) -> String {
    let mut lines = body.lines();
    let mut indented = lines.next().unwrap_or_default().to_string();
    for line in lines {
        indented.push('\n');
        if line.is_empty() {
            indented.push_str(prefix.trim_end());
        } else {
            indented.push_str(prefix);
            indented.push_str(line);
        }
    }
    indented
}

enum MarkdownReplacement {
    Inline(String),
    Block(String),
}

fn outcome_markdown(outcome: &DynamicOutcome) -> Option<MarkdownReplacement> {
    Some(match outcome {
        DynamicOutcome::Unevaluated(_) => return None,
        DynamicOutcome::InlineValue(value) => MarkdownReplacement::Inline(inline_value(value)),
        DynamicOutcome::InlineError(error) => {
            MarkdownReplacement::Inline(format!("*(Dataview error: {error})*"))
        }
        DynamicOutcome::InlineJs(result) => MarkdownReplacement::Inline(
            dataview_js_markdown(result)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        DynamicOutcome::InlineJsDisabled => {
            MarkdownReplacement::Inline("*(DataviewJS disabled)*".to_string())
        }
        DynamicOutcome::Hidden => MarkdownReplacement::Inline(String::new()),
        DynamicOutcome::Dql(result) => MarkdownReplacement::Block(dql_markdown(result)),
        DynamicOutcome::DataviewJs(result) => {
            MarkdownReplacement::Block(dataview_js_markdown(result))
        }
        DynamicOutcome::Tasks(result) => MarkdownReplacement::Block(tasks_markdown(result)),
        DynamicOutcome::Base(report) => MarkdownReplacement::Block(bases_markdown(report)),
        DynamicOutcome::Disabled { title, message } => {
            MarkdownReplacement::Block(callout("info", title, message))
        }
        DynamicOutcome::Error { title, message } => {
            MarkdownReplacement::Block(callout("warning", title, message))
        }
    })
}

fn callout(kind: &str, title: &str, message: &str) -> String {
    let mut rendered = format!("> [!{kind}] {}", title.trim_end_matches(':'));
    for line in message.lines() {
        rendered.push_str("\n> ");
        rendered.push_str(line);
    }
    rendered
}

/// Dataview renders null as a dash.
const NULL_MARKDOWN: &str = "-";

/// A value as inline Markdown. Strings are already Markdown (links are
/// carried as `[[...]]` text), so they pass through.
fn inline_value(value: &Value) -> String {
    match value {
        Value::Null => NULL_MARKDOWN.to_string(),
        Value::String(text) if text.is_empty() => NULL_MARKDOWN.to_string(),
        Value::String(text) => text.replace('\n', " "),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => format_number(number),
        Value::Array(items) if items.is_empty() => NULL_MARKDOWN.to_string(),
        Value::Array(items) => items
            .iter()
            .map(inline_value)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn format_number(number: &serde_json::Number) -> String {
    match number.as_f64() {
        Some(float) if number.is_f64() && float.fract() == 0.0 && float.abs() < 1e15 => {
            format!("{float:.0}")
        }
        _ => number.to_string(),
    }
}

fn table_cell(value: &Value) -> String {
    inline_value(value).replace('|', "\\|")
}

fn markdown_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let column_count = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if column_count == 0 {
        return String::new();
    }
    let header_cells = (0..column_count)
        .map(|index| {
            headers
                .get(index)
                .map_or_else(String::new, |header| header.replace('|', "\\|"))
        })
        .collect::<Vec<_>>();
    let mut rendered = format!("| {} |\n", header_cells.join(" | "));
    let _ = writeln!(rendered, "|{}", " --- |".repeat(column_count));
    for row in rows {
        let cells = (0..column_count)
            .map(|index| row.get(index).cloned().unwrap_or_default())
            .collect::<Vec<_>>();
        let _ = writeln!(rendered, "| {} |", cells.join(" | "));
    }
    rendered
}

fn dql_markdown(result: &DqlQueryResult) -> String {
    let mut sections = Vec::new();
    let body = match result.query_type {
        DqlQueryType::Table => markdown_table(
            &result.columns,
            &result
                .rows
                .iter()
                .map(|row| {
                    result
                        .columns
                        .iter()
                        .map(|column| table_cell(&row[column]))
                        .collect()
                })
                .collect::<Vec<_>>(),
        ),
        DqlQueryType::List => result
            .rows
            .iter()
            .map(|row| match result.columns.as_slice() {
                [column] => format!("- {}\n", inline_value(&row[column])),
                [left, right, ..] => format!(
                    "- {}: {}\n",
                    inline_value(&row[left]),
                    inline_value(&row[right])
                ),
                [] => format!("- {}\n", inline_value(row)),
            })
            .collect(),
        DqlQueryType::Task => {
            let file_column = result.columns.first().map_or("File", String::as_str);
            grouped_tasks_markdown(
                result
                    .rows
                    .iter()
                    .map(|row| (inline_value(&row[file_column]), row)),
                true,
            )
        }
        DqlQueryType::Calendar => {
            let file_column = result.columns.get(1).map_or("File", String::as_str);
            let mut rendered = String::new();
            let mut current_date = None::<String>;
            for row in &result.rows {
                let date = inline_value(&row["date"]);
                if current_date.as_deref() != Some(date.as_str()) {
                    let _ = writeln!(rendered, "- **{date}**");
                    current_date = Some(date);
                }
                let _ = writeln!(rendered, "  - {}", inline_value(&row[file_column]));
            }
            rendered
        }
    };
    if result.rows.is_empty() {
        sections.push("*No results.*".to_string());
    } else if !body.is_empty() {
        sections.push(body.trim_end().to_string());
    }
    if let Some(diagnostics) = dql_diagnostics_markdown(&result.diagnostics) {
        sections.push(diagnostics);
    }
    sections.join("\n\n")
}

fn dql_diagnostics_markdown(diagnostics: &[DqlDiagnostic]) -> Option<String> {
    if diagnostics.is_empty() {
        return None;
    }
    let message = diagnostics
        .iter()
        .map(|diagnostic| format!("- {}", diagnostic.message))
        .collect::<Vec<_>>()
        .join("\n");
    Some(callout("warning", "Diagnostics", &message))
}

/// Render task rows as Markdown task lists, optionally grouped under a bold
/// heading per file. Children nest under their parent task.
fn grouped_tasks_markdown<'a>(
    rows: impl Iterator<Item = (String, &'a Value)>,
    group_by_file: bool,
) -> String {
    let mut rendered = String::new();
    let mut current_file = None::<String>;
    for (file, task) in rows {
        if group_by_file && current_file.as_deref() != Some(file.as_str()) {
            if current_file.is_some() {
                rendered.push('\n');
            }
            let _ = writeln!(rendered, "**{file}**\n");
            current_file = Some(file);
        }
        push_task_markdown(&mut rendered, task, 0);
    }
    rendered
}

fn push_task_markdown(rendered: &mut String, task: &Value, depth: usize) {
    let status = task
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| status.chars().count() == 1)
        .unwrap_or(" ");
    let text = task.get("text").map(inline_value).unwrap_or_default();
    let _ = writeln!(rendered, "{}- [{status}] {text}", "  ".repeat(depth));
    if let Some(children) = task.get("children").and_then(Value::as_array) {
        for child in children {
            push_task_markdown(rendered, child, depth + 1);
        }
    }
}

fn task_file(task: &Value) -> String {
    task.get("path")
        .and_then(Value::as_str)
        .or_else(|| {
            task.get("file")
                .and_then(|file| file.get("path"))
                .and_then(Value::as_str)
        })
        .unwrap_or("<unknown>")
        .to_string()
}

fn dataview_js_markdown(result: &DataviewJsResult) -> String {
    if result.outputs.is_empty() {
        return result.value.as_ref().map(inline_value).unwrap_or_default();
    }
    result
        .outputs
        .iter()
        .map(|output| match output {
            DataviewJsOutput::Query { result } => dql_markdown(result),
            DataviewJsOutput::Table { headers, rows } => markdown_table(
                headers,
                &rows
                    .iter()
                    .map(|row| row.iter().map(table_cell).collect())
                    .collect::<Vec<_>>(),
            ),
            DataviewJsOutput::List { items } => {
                items.iter().fold(String::new(), |mut rendered, item| {
                    let _ = writeln!(rendered, "- {}", inline_value(item));
                    rendered
                })
            }
            DataviewJsOutput::TaskList {
                tasks,
                group_by_file,
            } => grouped_tasks_markdown(
                tasks.iter().map(|task| (task_file(task), task)),
                *group_by_file,
            ),
            DataviewJsOutput::Paragraph { text }
            | DataviewJsOutput::Span { text }
            | DataviewJsOutput::Element { text, .. } => text.clone(),
            DataviewJsOutput::Header { level, text } => {
                format!("{} {text}", "#".repeat((*level).clamp(1, 6)))
            }
        })
        .map(|section| section.trim_end().to_string())
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn tasks_markdown(result: &TasksQueryResult) -> String {
    if result.tasks.is_empty() {
        return "*No matching tasks.*".to_string();
    }
    let render_list = |tasks: &[Value]| {
        tasks
            .iter()
            .map(|task| {
                let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
                let text = task.get("text").map(inline_value).unwrap_or_default();
                match task.get("path").and_then(Value::as_str) {
                    Some(path) if !path.is_empty() => format!("- [{status}] {text} ({path})\n"),
                    _ => format!("- [{status}] {text}\n"),
                }
            })
            .collect::<String>()
    };
    if result.groups.is_empty() {
        return render_list(&result.tasks);
    }
    result
        .groups
        .iter()
        .map(|group| {
            format!(
                "**{}: {}**\n\n{}",
                group.field,
                inline_value(&group.key),
                render_list(&group.tasks)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn bases_markdown(report: &BasesEvalReport) -> String {
    let mut sections = report
        .views
        .iter()
        .map(base_view_markdown)
        .collect::<Vec<_>>();
    if !report.diagnostics.is_empty() {
        let message = report
            .diagnostics
            .iter()
            .map(|diagnostic| format!("- {}", diagnostic.message))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(callout("warning", "Diagnostics", &message));
    }
    sections.join("\n\n")
}

fn base_view_markdown(view: &BasesEvaluatedView) -> String {
    let title = view
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&view.view_type);
    let body = if view.view_type.eq_ignore_ascii_case("tasknoteskanban") {
        let mut grouped = std::collections::BTreeMap::<String, Vec<&BasesRow>>::new();
        for row in &view.rows {
            let key = row
                .group_value
                .as_ref()
                .map(inline_value)
                .filter(|value| value != NULL_MARKDOWN)
                .unwrap_or_else(|| "Ungrouped".to_string());
            grouped.entry(key).or_default().push(row);
        }
        grouped
            .into_iter()
            .fold(String::new(), |mut rendered, (key, rows)| {
                let _ = writeln!(rendered, "- **{key}**");
                for row in rows {
                    let _ = write!(rendered, "  {}", base_row_item(row));
                }
                rendered
            })
    } else if view.view_type.eq_ignore_ascii_case("tasknotestasklist") {
        view.rows.iter().map(base_row_item).collect()
    } else {
        markdown_table(
            &view
                .columns
                .iter()
                .map(|column| column.display_name.clone())
                .collect::<Vec<_>>(),
            &view
                .rows
                .iter()
                .map(|row| {
                    view.columns
                        .iter()
                        .map(|column| {
                            table_cell(row.cells.get(&column.key).unwrap_or(&Value::Null))
                        })
                        .collect()
                })
                .collect::<Vec<_>>(),
        )
    };
    let body = if view.rows.is_empty() {
        "*No results.*".to_string()
    } else {
        body.trim_end().to_string()
    };
    format!("**{title}**\n\n{body}")
}

fn base_row_item(row: &BasesRow) -> String {
    let text = row
        .cells
        .values()
        .find(|value| !value.is_null())
        .map(inline_value)
        .filter(|value| value != NULL_MARKDOWN)
        .unwrap_or_else(|| row.file_name.clone());
    format!("- {text} ({})\n", row.document_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn region(start: usize, end: usize, outcome: DynamicOutcome) -> DynamicRegion {
        DynamicRegion {
            start,
            end,
            outcome,
        }
    }

    #[test]
    fn inline_values_render_like_dataview() {
        assert_eq!(inline_value(&Value::Null), "-");
        assert_eq!(inline_value(&json!(1.0)), "1");
        assert_eq!(inline_value(&json!(1.5)), "1.5");
        assert_eq!(inline_value(&json!(["[[A]]", "[[B]]"])), "[[A]], [[B]]");
    }

    #[test]
    fn block_replacements_keep_container_prefixes() {
        let source = "> [!note]\n> ```dataview\n> LIST\n> ```\n\nafter\n";
        let start = source.find("```").unwrap();
        let end = source.find("\n\nafter").unwrap() + 1;
        let rendered = splice_markdown_regions(
            source,
            vec![region(
                start,
                end,
                DynamicOutcome::DataviewJs(DataviewJsResult {
                    outputs: vec![DataviewJsOutput::List {
                        items: vec![json!("[[A]]"), json!("[[B]]")],
                    }],
                    value: None,
                }),
            )],
        );
        assert_eq!(rendered, "> [!note]\n> - [[A]]\n> - [[B]]\n\nafter\n");
    }

    #[test]
    fn block_replacements_after_inline_text_start_a_new_block() {
        let source = "See ![[Tasks.base]] here";
        let start = source.find("![[").unwrap();
        let end = start + "![[Tasks.base]]".len();
        let rendered = splice_markdown_regions(
            source,
            vec![region(
                start,
                end,
                DynamicOutcome::Error {
                    title: "Bases error:".to_string(),
                    message: "missing".to_string(),
                },
            )],
        );
        assert_eq!(
            rendered,
            "See \n\n> [!warning] Bases error\n> missing\n\n here"
        );
    }

    #[test]
    fn unevaluated_inline_expressions_keep_their_source() {
        let source = "Status: `= this.status`";
        let rendered = splice_markdown_regions(
            source,
            vec![region(
                8,
                source.len(),
                DynamicOutcome::Unevaluated("this.status".into()),
            )],
        );
        assert_eq!(rendered, source);
    }

    #[test]
    fn task_rows_nest_children_and_group_by_file() {
        let rows = [json!({
            "File": "[[Dashboard]]",
            "status": " ",
            "text": "Write docs",
            "children": [{ "status": "x", "text": "Ship", "children": [] }]
        })];
        let rendered = grouped_tasks_markdown(
            rows.iter().map(|row| (inline_value(&row["File"]), row)),
            true,
        );
        assert_eq!(
            rendered,
            "**[[Dashboard]]**\n\n- [ ] Write docs\n  - [x] Ship\n"
        );
    }

    fn build_vault() -> (tempfile::TempDir, VaultPaths, String) {
        let temp_dir = tempfile::TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        std::fs::create_dir_all(vault_root.join(".vulcan")).expect(".vulcan dir should exist");
        std::fs::write(
            vault_root.join("Alpha.md"),
            "---\nstatus: active\n---\n- [ ] Alpha task\n",
        )
        .expect("alpha should write");
        let home = concat!(
            "---\nstatus: draft\n---\n",
            "# Home\n\n",
            "Status: `= this.status`\n\n",
            "```dataview\nTABLE status FROM \"Alpha\"\n```\n\n",
            "```dataviewjs\ndv.list([dv.current().status])\n```\n\n",
            "```tasks\nnot done\n```\n",
        );
        std::fs::write(vault_root.join("Home.md"), home).expect("home should write");
        let paths = VaultPaths::new(&vault_root);
        crate::scan_vault(&paths, crate::ScanMode::Full).expect("scan should succeed");
        (temp_dir, paths, home.to_string())
    }

    #[test]
    fn evaluated_markdown_replaces_every_dynamic_region() {
        let (_temp_dir, paths, source) = build_vault();
        let rendered = render_evaluated_markdown(
            &paths,
            &source,
            &EvaluatedMarkdownOptions {
                source_path: Some("Home.md"),
                ..EvaluatedMarkdownOptions::default()
            },
        );

        let markdown = &rendered.markdown;
        assert!(markdown.starts_with("---\nstatus: draft\n---\n# Home\n\n"));
        assert!(markdown.contains("Status: draft\n"), "{markdown}");
        assert!(markdown.contains("| File | status |\n| --- | --- |\n| [[Alpha]] | active |"));
        assert!(
            markdown.contains("- [ ] Alpha task (Alpha.md)"),
            "{markdown}"
        );
        assert!(
            markdown.contains("> [!info] DataviewJS disabled"),
            "{markdown}"
        );
        assert!(!markdown.contains("```"), "{markdown}");
        assert!(rendered
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == "dataviewjs_disabled"));
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn evaluated_markdown_runs_dataviewjs_when_enabled() {
        let (_temp_dir, paths, source) = build_vault();
        for policy in [DataviewJsRenderPolicy::Static, DataviewJsRenderPolicy::On] {
            let rendered = render_evaluated_markdown(
                &paths,
                &source,
                &EvaluatedMarkdownOptions {
                    source_path: Some("Home.md"),
                    dataview_js: policy,
                    ..EvaluatedMarkdownOptions::default()
                },
            );
            assert!(
                rendered.markdown.contains("\n- draft\n"),
                "{policy:?}: {}",
                rendered.markdown
            );
            assert!(rendered.diagnostics.is_empty());
        }
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn rendering_never_lets_dataviewjs_write() {
        let (_temp_dir, paths, _) = build_vault();
        std::fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            "[js_runtime]\ndefault_sandbox = \"fs\"\n",
        )
        .expect("config should write");
        let source = "```dataviewjs\nvault.create('Created.md', '# Created')\n```\n";
        let rendered = render_evaluated_markdown(
            &paths,
            source,
            &EvaluatedMarkdownOptions {
                source_path: Some("Home.md"),
                dataview_js: DataviewJsRenderPolicy::On,
                ..EvaluatedMarkdownOptions::default()
            },
        );

        assert!(!paths.vault_root().join("Created.md").exists());
        assert!(
            rendered
                .markdown
                .contains("not allowed while rendering a note"),
            "{}",
            rendered.markdown
        );
    }

    #[test]
    fn guarded_renders_only_see_readable_notes() {
        let (_temp_dir, paths, source) = build_vault();
        std::fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            "[permissions.profiles.home_only]\nread = { allow = [\"Home.md\"] }\n",
        )
        .expect("config should write");
        let profile = crate::resolve_permission_profile(&paths, Some("home_only"))
            .expect("profile should resolve");
        let guard = crate::ProfilePermissionGuard::new(&paths, profile);
        let rendered = EvaluatedMarkdownRenderer::load(&paths, Some(&guard)).render(
            &source,
            &EvaluatedMarkdownOptions {
                source_path: Some("Home.md"),
                guard: Some(&guard),
                ..EvaluatedMarkdownOptions::default()
            },
        );

        assert!(
            rendered.markdown.contains("Status: draft"),
            "{}",
            rendered.markdown
        );
        assert!(
            !rendered.markdown.contains("[[Alpha]]"),
            "{}",
            rendered.markdown
        );
        assert!(
            !rendered.markdown.contains("Alpha task"),
            "{}",
            rendered.markdown
        );
    }

    #[test]
    fn note_embeds_are_transcluded_with_their_content_evaluated() {
        let temp_dir = tempfile::TempDir::new().expect("temp dir should be created");
        let root = temp_dir.path().join("vault");
        std::fs::create_dir_all(root.join(".vulcan")).expect(".vulcan dir should exist");
        let home = "# Home\n\n![[Card]]\n\n![[Card#Details]]\n\n![[Loop]]\n\n![[Missing]]\n";
        for (path, contents) in [
            ("Home.md", home),
            (
                "Card.md",
                "---\nstatus: active\n---\nCard status: `= this.status`\n\n## Details\nOnly details\n",
            ),
            ("Loop.md", "Loop body\n\n![[Loop]]\n"),
        ] {
            std::fs::write(root.join(path), contents).expect("note should write");
        }
        let paths = VaultPaths::new(&root);
        crate::scan_vault(&paths, crate::ScanMode::Full).expect("scan should succeed");

        let rendered = render_evaluated_markdown(
            &paths,
            home,
            &EvaluatedMarkdownOptions {
                source_path: Some("Home.md"),
                ..EvaluatedMarkdownOptions::default()
            },
        );
        let markdown = &rendered.markdown;

        assert!(
            markdown.contains(
                "> [!embed] Card\n> Card status: active\n>\n> ## Details\n> Only details"
            ),
            "{markdown}"
        );
        assert!(
            markdown.contains("> [!embed] Card > Details\n> ## Details\n> Only details"),
            "{markdown}"
        );
        assert!(
            markdown.contains("> [!embed] Loop\n> Loop body"),
            "{markdown}"
        );
        assert!(markdown.contains("embed cycle detected"), "{markdown}");
        assert!(
            markdown.contains("could not resolve embedded note `![[Missing]]`"),
            "{markdown}"
        );
        let kinds = rendered
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.kind.as_str())
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"embed_cycle"), "{kinds:?}");
        assert!(kinds.contains(&"unresolved_embed"), "{kinds:?}");
    }

    #[test]
    fn inline_fields_render_like_dataview_reading_view() {
        let (_temp_dir, paths, _) = build_vault();
        let source = "**Due Date**:: 2026-04\nOwner:: [[Bob]]\nInline (hidden:: only value) and [shown:: both].\n";
        let rendered =
            render_evaluated_markdown(&paths, source, &EvaluatedMarkdownOptions::default());
        assert_eq!(
            rendered.markdown,
            "**Due Date**: 2026-04\n**Owner**: [[Bob]]\nInline only value and **shown**: both.\n"
        );
    }

    #[test]
    fn html_line_breaks_follow_obsidian_strict_line_breaks() {
        let (_temp_dir, paths, _) = build_vault();
        let html = crate::render_note_html(&paths, "Home.md", "one\ntwo\n").html;
        assert!(html.contains("one<br />\ntwo"), "{html}");

        std::fs::create_dir_all(paths.vault_root().join(".obsidian"))
            .expect(".obsidian should exist");
        std::fs::write(
            paths.vault_root().join(".obsidian/app.json"),
            r#"{"strictLineBreaks": true}"#,
        )
        .expect("app.json should write");
        let html = crate::render_note_html(&paths, "Home.md", "one\ntwo\n").html;
        assert!(html.contains("one\ntwo"), "{html}");
        assert!(!html.contains("<br"), "{html}");
    }

    #[test]
    fn obsidian_comments_are_hidden() {
        let (_temp_dir, paths, _) = build_vault();
        let source = "Visible %%inline secret%% text\n\n%%\nblock secret\n%%\n\nAfter\n";
        let rendered = render_evaluated_markdown(
            &paths,
            source,
            &EvaluatedMarkdownOptions {
                source_path: Some("Home.md"),
                ..EvaluatedMarkdownOptions::default()
            },
        );
        assert!(
            !rendered.markdown.contains("secret"),
            "{}",
            rendered.markdown
        );
        assert!(
            rendered.markdown.contains("Visible  text"),
            "{}",
            rendered.markdown
        );
        assert!(rendered.markdown.contains("After"), "{}", rendered.markdown);

        let html = crate::render_note_html(&paths, "Home.md", source).html;
        assert!(!html.contains("secret"), "{html}");
        assert!(html.contains("After"), "{html}");
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn inline_dataviewjs_follows_the_vault_setting_and_render_policy() {
        let (_temp_dir, paths, _) = build_vault();
        let source = "---\nstatus: draft\n---\nJS: `$= dv.current().status`\n";
        let render = |policy| {
            render_evaluated_markdown(
                &paths,
                source,
                &EvaluatedMarkdownOptions {
                    source_path: Some("Home.md"),
                    dataview_js: policy,
                    ..EvaluatedMarkdownOptions::default()
                },
            )
            .markdown
        };
        // Off in the vault, as in Dataview: the code stays as written.
        assert!(render(DataviewJsRenderPolicy::On).contains("`$= dv.current().status`"));

        std::fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            "[dataview]\nenable_inline_dataview_js = true\n",
        )
        .expect("config should write");
        assert!(render(DataviewJsRenderPolicy::On).contains("JS: draft"));
        assert!(render(DataviewJsRenderPolicy::Off).contains("JS: *(DataviewJS disabled)*"));
    }

    #[test]
    fn tables_escape_pipes_in_cells() {
        let table = markdown_table(&["A".into()], &[vec![table_cell(&json!("x|y"))]]);
        assert_eq!(table, "| A |\n| --- |\n| x\\|y |\n");
    }
}
