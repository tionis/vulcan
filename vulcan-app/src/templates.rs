use crate::notes::{note_path_is_mdbase_managed, write_ordinary_note_if_unchanged_with_profile};
use crate::AppError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use vulcan_core::config::TemplatesConfig;
use vulcan_core::expression::functions::{parse_date_like_string, parse_date_with_format};
use vulcan_core::move_note;
use vulcan_core::move_rewrite::move_note_unlocked;
use vulcan_core::parser::parse_document;
use vulcan_core::paths::{
    normalize_relative_input_path, secure_read_to_string, RelativePathOptions,
};
use vulcan_core::Verbosity;
use vulcan_core::{
    load_vault_config, resolve_note_reference, resolve_permission_profile, MoveSummary,
    PermissionFilter, PermissionGuard, PluginEvent, ProfilePermissionGuard,
    TemplaterFileCreationMode, VaultConfig, VaultPaths,
};

mod frontmatter;
mod program;
mod prompt;

pub use frontmatter::{
    apply_template_insertion_mode, find_frontmatter_block, format_frontmatter_block,
    merge_template_frontmatter, parse_frontmatter_document, prepare_template_insertion,
    render_note_from_parts,
};
pub use prompt::{
    with_template_prompter, TemplateInput, TemplatePromptRequest, TemplatePrompter,
    TemplateSuggestRequest, TEMPLATE_INPUT_REQUIRED_CODE,
};

pub type CliError = AppError;
pub type YamlMapping = serde_yaml::Mapping;
pub type YamlValue = serde_yaml::Value;

#[derive(Debug, Clone, Default)]
pub(crate) struct TemplateStage {
    creates: BTreeMap<String, String>,
    moved: Option<vulcan_core::move_rewrite::OrdinaryNoteMovePlan>,
}

pub(crate) type StagedTemplateCreates = Arc<Mutex<TemplateStage>>;

pub(crate) fn staged_template_creates() -> StagedTemplateCreates {
    Arc::new(Mutex::new(TemplateStage::default()))
}

pub(crate) fn staged_template_create_snapshot(
    staged: &StagedTemplateCreates,
) -> Result<BTreeMap<String, String>, AppError> {
    staged
        .lock()
        .map_err(|_| AppError::operation("template create staging lock poisoned"))
        .and_then(|changes| {
            if changes.moved.is_some() {
                Err(AppError::operation(
                    "note create cannot move an existing note",
                ))
            } else {
                Ok(changes.creates.clone())
            }
        })
}

fn write_template_result_with_staged_creates(
    paths: &VaultPaths,
    path: &str,
    before: Option<&str>,
    after: &str,
    staged: &StagedTemplateCreates,
    permission_profile: Option<&str>,
    operation: &str,
) -> Result<(), AppError> {
    let staged = staged
        .lock()
        .map_err(|_| AppError::operation("template staging lock poisoned"))?
        .clone();
    if staged.creates.is_empty() && staged.moved.is_none() {
        return write_ordinary_note_if_unchanged_with_profile(
            paths,
            path,
            before,
            after,
            operation,
            permission_profile,
        );
    }
    if staged.creates.contains_key(path) {
        return Err(AppError::operation(format!(
            "template side effect conflicts with final note path: {path}"
        )));
    }
    let mut changes = staged
        .creates
        .into_iter()
        .map(
            |(path, content)| vulcan_core::ordinary_write::OrdinaryWriteChange {
                path,
                before: None,
                after: Some(content),
            },
        )
        .collect::<Vec<_>>();
    let mut renames = BTreeMap::new();
    let refactor_paths = staged
        .moved
        .as_ref()
        .map(|plan| {
            plan.changes
                .iter()
                .map(|change| change.path.clone())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if let Some(mut moved) = staged.moved {
        if moved.summary.destination_path != path {
            return Err(AppError::operation(
                "template final path differs from its staged move",
            ));
        }
        let destination = moved
            .changes
            .iter_mut()
            .find(|change| change.path == path)
            .ok_or_else(|| AppError::operation("staged template move has no destination"))?;
        if destination.after.as_deref() != before {
            return Err(AppError::operation(
                "template moved target changed during rendering",
            ));
        }
        destination.after = Some(after.to_string());
        renames.insert(moved.summary.destination_path, moved.summary.source_path);
        changes.extend(moved.changes);
    } else {
        changes.push(vulcan_core::ordinary_write::OrdinaryWriteChange {
            path: path.to_string(),
            before: before.map(str::to_string),
            after: Some(after.to_string()),
        });
    }
    vulcan_core::ordinary_write::apply_ordinary_write_batch_with_renames_and_preflight(
        paths,
        &changes,
        &renames,
        || validate_template_batch(paths, &changes, &refactor_paths, permission_profile),
    )
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    Ok(())
}

/// A template's change to an existing note.
struct TemplateNoteWrite<'a> {
    path: &'a str,
    before: &'a str,
    after: &'a str,
    /// The previous content `on_note_write` hooks receive.
    hook_previous: Option<&'a str>,
    operation: &'a str,
    hook_operation: &'a str,
}

/// Write a template's change to an existing note. A collection record goes
/// through the managed mdbase write, which validates it, runs lifecycle
/// policies, and dispatches its own `on_note_write` hooks; template side
/// effects cannot accompany it. Any other note runs the hooks and is written
/// with the template's side effects.
fn write_template_note(
    paths: &VaultPaths,
    write: &TemplateNoteWrite<'_>,
    staged: &StagedTemplateCreates,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<(), AppError> {
    if note_path_is_mdbase_managed(paths, write.path, permission_profile)? {
        let side_effects = staged
            .lock()
            .map_err(|_| AppError::operation("template staging lock poisoned"))?;
        if !side_effects.creates.is_empty() || side_effects.moved.is_some() {
            return Err(AppError::operation(
                "template side effects cannot be combined with an mdbase-managed note",
            ));
        }
        drop(side_effects);
        crate::notes::apply_mdbase_note_content_change(
            paths,
            &crate::mdbase::MdbaseManagedNoteWriteRequest {
                path: write.path,
                before: Some(write.before),
                after: Some(write.after),
                operation: crate::mdbase::MdbaseWriteOperation::Update,
                mode: crate::mdbase::MdbaseManagedWriteMode::Validated,
                dry_run: false,
                permission_profile,
                verbosity,
            },
        )?;
        return Ok(());
    }
    crate::plugins::dispatch_plugin_event(
        paths,
        permission_profile,
        PluginEvent::OnNoteWrite,
        &json!({
            "kind": PluginEvent::OnNoteWrite,
            "path": write.path,
            "operation": write.hook_operation,
            "existed_before": true,
            "previous_content": write.hook_previous,
            "content": write.after,
        }),
        verbosity,
    )?;
    write_template_result_with_staged_creates(
        paths,
        write.path,
        Some(write.before),
        write.after,
        staged,
        permission_profile,
        write.operation,
    )
}

fn validate_template_batch(
    paths: &VaultPaths,
    changes: &[vulcan_core::ordinary_write::OrdinaryWriteChange],
    refactor_paths: &BTreeSet<String>,
    permission_profile: Option<&str>,
) -> Result<(), String> {
    let guard = permission_profile
        .map(|profile| {
            resolve_permission_profile(paths, Some(profile))
                .map(|selection| ProfilePermissionGuard::new(paths, selection))
                .map_err(|error| error.to_string())
        })
        .transpose()?;
    for change in changes {
        if note_path_is_mdbase_managed(paths, &change.path, permission_profile)
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "template write target became an mdbase-managed note: {}",
                change.path
            ));
        }
        if let Some(guard) = guard.as_ref() {
            guard
                .check_write_path(&change.path)
                .map_err(|error| error.to_string())?;
            if refactor_paths.contains(&change.path) {
                guard
                    .check_refactor_path(&change.path)
                    .map_err(|error| error.to_string())?;
                if change.before.is_some() {
                    guard
                        .check_read_path(&change.path)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    }
    Ok(())
}

fn stage_template_create(
    staged: &StagedTemplateCreates,
    paths: &VaultPaths,
    path: &str,
    content: String,
) -> Result<(), String> {
    let mut changes = staged
        .lock()
        .map_err(|_| "template create staging lock poisoned".to_string())?;
    if changes.creates.contains_key(path)
        || changes
            .moved
            .as_ref()
            .is_some_and(|plan| plan.changes.iter().any(|change| change.path == path))
        || paths.vault_root().join(path).exists()
    {
        return Err(format!("destination note already exists: {path}"));
    }
    changes.creates.insert(path.to_string(), content);
    Ok(())
}

enum StagedTemplatePath {
    Content(String),
    Deleted,
}

fn staged_template_path_state(
    staged: Option<&StagedTemplateCreates>,
    path: &str,
) -> Option<StagedTemplatePath> {
    let staged = staged?.lock().ok()?;
    if let Some(content) = staged.creates.get(path) {
        return Some(StagedTemplatePath::Content(content.clone()));
    }
    staged
        .moved
        .as_ref()?
        .changes
        .iter()
        .find(|change| change.path == path)
        .map(|change| {
            change
                .after
                .clone()
                .map_or(StagedTemplatePath::Deleted, StagedTemplatePath::Content)
        })
}

fn template_path_exists(
    paths: &VaultPaths,
    staged: Option<&StagedTemplateCreates>,
    path: &str,
) -> bool {
    staged_template_path_state(staged, path).map_or_else(
        || secure_read_to_string(paths.vault_root(), Path::new(path)).is_ok(),
        |content| matches!(content, StagedTemplatePath::Content(_)),
    )
}

fn template_path_content(
    paths: &VaultPaths,
    staged: Option<&StagedTemplateCreates>,
    path: &str,
) -> Result<String, String> {
    match staged_template_path_state(staged, path) {
        Some(StagedTemplatePath::Content(content)) => Ok(content),
        Some(StagedTemplatePath::Deleted) => Err(format!("File {path} doesn't exist")),
        None => secure_read_to_string(paths.vault_root(), Path::new(path))
            .map_err(|error| error.to_string()),
    }
}

fn stage_template_move(
    staged: &StagedTemplateCreates,
    paths: &VaultPaths,
    current: &str,
    destination: &str,
    expected_source: Option<&str>,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<Option<MoveSummary>, String> {
    let previous = staged
        .lock()
        .map_err(|_| "template staging lock poisoned".to_string())?
        .clone();
    let source = previous
        .moved
        .as_ref()
        .map_or(current, |plan| plan.summary.source_path.as_str());
    if previous.moved.is_none() && !paths.vault_root().join(source).is_file() {
        if let Some(guard) = guard {
            guard
                .check_write_path(destination)
                .map_err(|error| error.to_string())?;
        }
        return Ok(None);
    }
    let expected_source =
        expected_source.ok_or_else(|| "template create target already exists".to_string())?;
    let plan = vulcan_core::move_rewrite::plan_ordinary_note_move(paths, source, destination)
        .map_err(|error| error.to_string())?;
    if source == destination {
        staged
            .lock()
            .map_err(|_| "template staging lock poisoned".to_string())?
            .moved = None;
        return Ok(Some(plan.summary));
    }
    if plan
        .changes
        .iter()
        .find(|change| change.path == source)
        .and_then(|change| change.before.as_deref())
        != Some(expected_source)
    {
        return Err("template source changed before move staging".to_string());
    }
    for change in &plan.changes {
        if previous.creates.contains_key(&change.path) {
            return Err(format!(
                "template move conflicts with staged create: {}",
                change.path
            ));
        }
        if let Some(old) = previous.moved.as_ref().and_then(|old| {
            old.changes
                .iter()
                .find(|old| old.path == change.path && old.before.is_some())
        }) {
            if old.before != change.before {
                return Err(format!(
                    "template move input changed during rendering: {}",
                    change.path
                ));
            }
        }
        if let Some(guard) = guard {
            guard
                .check_write_path(&change.path)
                .and_then(|()| guard.check_refactor_path(&change.path))
                .map_err(|error| error.to_string())?;
            if change.before.is_some() {
                guard
                    .check_read_path(&change.path)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    let summary = plan.summary.clone();
    staged
        .lock()
        .map_err(|_| "template staging lock poisoned".to_string())?
        .moved = Some(plan);
    Ok(Some(summary))
}

const MAX_TEMPLATE_INCLUDE_DEPTH: usize = 10;
const MAX_QUICKADD_EXPANSION_DEPTH: usize = 10;
const MAX_FILE_CREATION_TRIGGER_BYTES: usize = 100_000;
const DEFAULT_FILE_DATE_FORMAT: &str = "YYYY-MM-DD HH:mm";
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TemplateEngineKind {
    #[default]
    Auto,
    Native,
    Templater,
}

impl TemplateEngineKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::Templater => "templater",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateRunMode {
    Create,
    Append,
    Dynamic,
}

impl TemplateRunMode {
    fn config_code(self) -> i64 {
        match self {
            Self::Create => 0,
            Self::Append => 1,
            Self::Dynamic => 5,
        }
    }
}

/// Template variable that, when set, makes `tp.file.cursor()` emit its
/// value instead of nothing, so an editor can find and remove the marker and
/// place the cursor there. Outside editors the call stays a no-op.
pub const TEMPLATE_CURSOR_MARKER_VAR: &str = "__vulcan_cursor_marker";

#[derive(Debug, Clone)]
pub struct TemplateRenderRequest<'a> {
    pub paths: &'a VaultPaths,
    pub vault_config: &'a VaultConfig,
    pub templates: &'a [TemplateCandidate],
    pub template_path: Option<&'a Path>,
    pub template_text: &'a str,
    pub target_path: &'a str,
    pub target_contents: Option<&'a str>,
    pub engine: TemplateEngineKind,
    pub vars: &'a HashMap<String, String>,
    pub allow_mutations: bool,
    pub run_mode: TemplateRunMode,
    /// Date (`YYYY-MM-DD`) the note is about, e.g. a back-dated daily note.
    /// Drives `{{date}}`/`{{time}}`-style built-ins; `tp.date.now()` stays the
    /// wall clock. `None` uses the current time.
    pub reference_date: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRenderOutput {
    pub content: String,
    pub target_path: String,
    pub engine: TemplateEngineKind,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<String>,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateSummary {
    pub name: String,
    pub source: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateListReport {
    pub templates: Vec<TemplateSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateShowReport {
    pub name: String,
    pub source: String,
    pub path: String,
    pub content: String,
    /// Prompts the template asks, with the `--var` key answering each.
    pub inputs: Vec<TemplateInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplatePreviewRequest {
    pub template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(default)]
    pub engine: TemplateEngineKind,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vars: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplatePreviewReport {
    pub template: String,
    pub template_source: String,
    pub path: String,
    pub engine: String,
    pub content: String,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateCreateRequest {
    pub template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(default)]
    pub engine: TemplateEngineKind,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vars: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateCreateReport {
    pub template: String,
    pub template_source: String,
    pub path: String,
    pub engine: String,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<String>,
    #[serde(skip)]
    pub absolute_path: PathBuf,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateCreationTriggerReport {
    pub path: String,
    pub triggered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<String>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct RenderedCreationTrigger {
    pub content: String,
    pub target_path: String,
    pub template: Option<String>,
    pub engine: TemplateEngineKind,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<String>,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInsertRequest {
    pub template: String,
    pub note: String,
    pub mode: TemplateInsertMode,
    #[serde(default)]
    pub engine: TemplateEngineKind,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vars: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateInsertReport {
    pub template: String,
    pub template_source: String,
    pub note: String,
    pub mode: String,
    pub engine: String,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<String>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LoadedTemplateSource {
    pub template: TemplateCandidate,
    pub templates: Vec<TemplateCandidate>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct LoadedTemplateRenderRequest<'a> {
    pub target_path: &'a str,
    pub target_contents: Option<&'a str>,
    pub engine: TemplateEngineKind,
    pub vars: &'a HashMap<String, String>,
    pub allow_mutations: bool,
    pub run_mode: TemplateRunMode,
    /// Date (`YYYY-MM-DD`) the note is about, e.g. a back-dated daily note.
    /// Drives `{{date}}`/`{{time}}`-style built-ins; `tp.date.now()` stays the
    /// wall clock. `None` uses the current time.
    pub reference_date: Option<&'a str>,
}

pub fn parse_template_var_bindings(vars: &[String]) -> Result<HashMap<String, String>, CliError> {
    let mut parsed = HashMap::new();
    for binding in vars {
        let Some((key, value)) = binding.split_once('=') else {
            return Err(CliError::operation(format!(
                "template variable bindings must use key=value syntax: {binding}"
            )));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(CliError::operation(format!(
                "template variable bindings need a non-empty key: {binding}"
            )));
        }
        parsed.insert(key.to_string(), value.to_string());
    }
    Ok(parsed)
}

#[must_use]
pub fn detect_template_engine(source: &str, requested: TemplateEngineKind) -> TemplateEngineKind {
    match requested {
        TemplateEngineKind::Auto => {
            if source.contains("<%") {
                TemplateEngineKind::Templater
            } else {
                TemplateEngineKind::Native
            }
        }
        explicit => explicit,
    }
}

pub fn render_template_request(
    request: TemplateRenderRequest<'_>,
) -> Result<TemplateRenderOutput, CliError> {
    render_template_request_with_filter(request, None)
}

pub fn render_template_request_with_filter(
    request: TemplateRenderRequest<'_>,
    read_filter: Option<&PermissionFilter>,
) -> Result<TemplateRenderOutput, CliError> {
    render_template_request_with_authority(request, read_filter, None, None)
}

fn render_template_request_with_authority(
    request: TemplateRenderRequest<'_>,
    read_filter: Option<&PermissionFilter>,
    mutation_guard: Option<&ProfilePermissionGuard>,
    staged_creates: Option<StagedTemplateCreates>,
) -> Result<TemplateRenderOutput, CliError> {
    render_template_request_at_depth(request, read_filter, mutation_guard, staged_creates, 0)
}

fn render_template_request_at_depth(
    request: TemplateRenderRequest<'_>,
    read_filter: Option<&PermissionFilter>,
    mutation_guard: Option<&ProfilePermissionGuard>,
    staged_creates: Option<StagedTemplateCreates>,
    include_depth: usize,
) -> Result<TemplateRenderOutput, CliError> {
    let engine = detect_template_engine(request.template_text, request.engine);
    let template_text = request.template_text;
    let source_name = template_source_name(&request);
    let mut session =
        TemplateSession::new(request, engine, read_filter, mutation_guard, staged_creates);
    session.include_depth = include_depth;
    let content = session.render_source(template_text, engine, include_depth, &source_name)?;
    let content = session.merge_pending_frontmatter(&content)?;
    #[cfg(feature = "js_runtime")]
    let content = session.run_post_render_hooks(content)?;
    if let Some(staged) = session.staged_creates.as_ref() {
        let staged = staged
            .lock()
            .map_err(|_| AppError::operation("template staging lock poisoned"))?;
        session.changed_paths = staged
            .creates
            .keys()
            .cloned()
            .chain(
                staged
                    .moved
                    .iter()
                    .flat_map(|plan| plan.changes.iter().map(|change| change.path.clone())),
            )
            .collect();
    }
    Ok(TemplateRenderOutput {
        content,
        target_path: session.target_path,
        engine,
        warnings: session.warnings,
        diagnostics: session.diagnostics,
        changed_paths: session.changed_paths.into_iter().collect(),
    })
}

fn guarded_template_move(
    paths: &VaultPaths,
    source: &str,
    destination: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<MoveSummary, String> {
    let Some(guard) = guard else {
        return move_note(paths, source, destination, false).map_err(|error| error.to_string());
    };
    guard
        .check_read_path(source)
        .and_then(|()| guard.check_write_path(source))
        .and_then(|()| guard.check_refactor_path(source))
        .and_then(|()| guard.check_write_path(destination))
        .and_then(|()| guard.check_refactor_path(destination))
        .map_err(|error| error.to_string())?;
    let _lock =
        vulcan_core::write_lock::acquire_write_lock(paths).map_err(|error| error.to_string())?;
    let plan =
        move_note_unlocked(paths, source, destination, true).map_err(|error| error.to_string())?;
    for rewrite in &plan.rewritten_files {
        guard
            .check_read_path(&rewrite.path)
            .and_then(|()| guard.check_write_path(&rewrite.path))
            .and_then(|()| guard.check_refactor_path(&rewrite.path))
            .map_err(|error| error.to_string())?;
    }
    move_note_unlocked(paths, source, destination, false).map_err(|error| error.to_string())
}

pub fn build_template_list_report(paths: &VaultPaths) -> Result<TemplateListReport, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    let config = load_vault_config(paths).config;
    let templates = discover_templates(
        paths,
        config.templates.obsidian_folder.as_deref(),
        config.templates.templater_folder.as_deref(),
    )?;
    Ok(TemplateListReport {
        templates: templates
            .templates
            .into_iter()
            .map(|template| TemplateSummary {
                name: template.name,
                source: template.source.to_string(),
                path: template.display_path,
            })
            .collect(),
        warnings: templates.warnings,
    })
}

pub fn load_named_template(
    paths: &VaultPaths,
    vault_config: &VaultConfig,
    template_name: &str,
) -> Result<LoadedTemplateSource, AppError> {
    let templates = discover_templates(
        paths,
        vault_config.templates.obsidian_folder.as_deref(),
        vault_config.templates.templater_folder.as_deref(),
    )?;
    let template = resolve_template_file(paths, &templates.templates, template_name)?;
    let source = fs::read_to_string(&template.absolute_path).map_err(AppError::operation)?;
    Ok(LoadedTemplateSource {
        template,
        templates: templates.templates,
        source,
    })
}

pub(crate) fn load_named_template_with_guard(
    paths: &VaultPaths,
    config: &VaultConfig,
    name: &str,
    guard: &ProfilePermissionGuard,
) -> Result<LoadedTemplateSource, AppError> {
    let mut templates: Vec<TemplateCandidate> = Vec::new();
    let roots = [
        (Some(Path::new(".vulcan/templates")), "vulcan"),
        (config.templates.templater_folder.as_deref(), "templater"),
        (config.templates.obsidian_folder.as_deref(), "obsidian"),
    ];
    for (root, source) in roots {
        let Some(root) = root.filter(|root| !root.as_os_str().is_empty()) else {
            continue;
        };
        for candidate in list_templates_in_directory(
            &paths.vault_root().join(root),
            &root.to_string_lossy(),
            source,
        )? {
            // Remove inaccessible candidates before source-precedence resolution.
            // An unreadable local template cannot shadow a readable imported one.
            if guard.check_read_path(&candidate.display_path).is_ok()
                && !templates
                    .iter()
                    .any(|existing| existing.name == candidate.name)
            {
                templates.push(candidate);
            }
        }
    }
    templates.sort_by(|left, right| left.name.cmp(&right.name));
    let template = resolve_template_file(paths, &templates, name).map_err(|_| {
        if guard.grant().read.is_unrestricted() && !guard.has_policy_hook() {
            AppError::operation_with_code("template_not_found", "template not found")
        } else {
            AppError::operation("template not found in readable scope")
        }
    })?;
    guard
        .check_read_path(&template.display_path)
        .map_err(AppError::operation)?;
    let source = secure_read_to_string(paths.vault_root(), Path::new(&template.display_path))
        .map_err(AppError::operation)?;
    Ok(LoadedTemplateSource {
        template,
        templates,
        source,
    })
}

pub fn render_loaded_template(
    paths: &VaultPaths,
    vault_config: &VaultConfig,
    loaded: &LoadedTemplateSource,
    request: &LoadedTemplateRenderRequest<'_>,
) -> Result<TemplateRenderOutput, AppError> {
    render_loaded_template_with_filter(paths, vault_config, loaded, request, None)
}

pub fn render_loaded_template_with_filter(
    paths: &VaultPaths,
    vault_config: &VaultConfig,
    loaded: &LoadedTemplateSource,
    request: &LoadedTemplateRenderRequest<'_>,
    read_filter: Option<&PermissionFilter>,
) -> Result<TemplateRenderOutput, AppError> {
    render_loaded_template_with_authority(paths, vault_config, loaded, request, read_filter, None)
}

pub(crate) fn render_loaded_template_with_authority(
    paths: &VaultPaths,
    vault_config: &VaultConfig,
    loaded: &LoadedTemplateSource,
    request: &LoadedTemplateRenderRequest<'_>,
    read_filter: Option<&PermissionFilter>,
    mutation_guard: Option<&ProfilePermissionGuard>,
) -> Result<TemplateRenderOutput, AppError> {
    render_loaded_template_with_staged_creates(
        paths,
        vault_config,
        loaded,
        request,
        read_filter,
        mutation_guard,
        None,
    )
}

pub(crate) fn render_loaded_template_with_staged_creates(
    paths: &VaultPaths,
    vault_config: &VaultConfig,
    loaded: &LoadedTemplateSource,
    request: &LoadedTemplateRenderRequest<'_>,
    read_filter: Option<&PermissionFilter>,
    mutation_guard: Option<&ProfilePermissionGuard>,
    staged_creates: Option<StagedTemplateCreates>,
) -> Result<TemplateRenderOutput, AppError> {
    render_template_request_with_authority(
        TemplateRenderRequest {
            paths,
            vault_config,
            templates: &loaded.templates,
            template_path: Some(&loaded.template.absolute_path),
            template_text: &loaded.source,
            target_path: request.target_path,
            target_contents: request.target_contents,
            engine: request.engine,
            vars: request.vars,
            allow_mutations: request.allow_mutations,
            run_mode: request.run_mode,
            reference_date: request.reference_date,
        },
        read_filter,
        mutation_guard,
        staged_creates,
    )
}

pub fn apply_template_creation_trigger(
    paths: &VaultPaths,
    relative_path: &str,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
    read_filter: Option<&PermissionFilter>,
) -> Result<TemplateCreationTriggerReport, AppError> {
    let relative_path = template_output_path(
        relative_path,
        Some(relative_path),
        &TemplateTimestamp::current(),
    )?;
    let config = load_vault_config(paths).config;
    if !config.templates.trigger_on_file_creation {
        return Ok(TemplateCreationTriggerReport {
            path: relative_path,
            triggered: false,
            template: None,
            engine: None,
            warnings: Vec::new(),
            diagnostics: Vec::new(),
            changed_paths: Vec::new(),
        });
    }
    let previous = secure_read_to_string(paths.vault_root(), Path::new(&relative_path))
        .map_err(AppError::operation)?;
    let mutation_guard = permission_profile
        .map(|profile| {
            resolve_permission_profile(paths, Some(profile))
                .map(|selection| ProfilePermissionGuard::new(paths, selection))
                .map_err(AppError::operation)
        })
        .transpose()?;
    let staged = staged_template_creates();
    let Some(rendered) = render_creation_trigger_with_staged_creates(
        paths,
        &config,
        &relative_path,
        &previous,
        read_filter,
        mutation_guard.as_ref(),
        Some(staged.clone()),
    )?
    else {
        return Ok(TemplateCreationTriggerReport {
            path: relative_path,
            triggered: false,
            template: None,
            engine: None,
            warnings: Vec::new(),
            diagnostics: Vec::new(),
            changed_paths: Vec::new(),
        });
    };
    if let Some(guard) = mutation_guard.as_ref() {
        guard
            .check_write_path(&rendered.target_path)
            .map_err(AppError::operation)?;
    }
    let expected_final = if rendered.target_path == relative_path {
        previous.clone()
    } else {
        template_path_content(paths, Some(&staged), &rendered.target_path)
            .map_err(AppError::operation)?
    };

    write_template_note(
        paths,
        &TemplateNoteWrite {
            path: &rendered.target_path,
            before: &expected_final,
            after: &rendered.content,
            hook_previous: Some(&previous),
            operation: "template trigger",
            hook_operation: "template-trigger",
        },
        &staged,
        permission_profile,
        verbosity,
    )?;

    let mut changed_paths = rendered.changed_paths;
    changed_paths.push(rendered.target_path.clone());
    changed_paths.sort();
    changed_paths.dedup();
    Ok(TemplateCreationTriggerReport {
        path: rendered.target_path,
        triggered: true,
        template: rendered.template,
        engine: Some(rendered.engine.as_str().to_string()),
        warnings: rendered.warnings,
        diagnostics: rendered.diagnostics,
        changed_paths,
    })
}

pub(crate) fn render_creation_trigger_with_staged_creates(
    paths: &VaultPaths,
    config: &VaultConfig,
    relative_path: &str,
    contents: &str,
    read_filter: Option<&PermissionFilter>,
    mutation_guard: Option<&ProfilePermissionGuard>,
    staged_creates: Option<StagedTemplateCreates>,
) -> Result<Option<RenderedCreationTrigger>, AppError> {
    if !config.templates.trigger_on_file_creation
        || creation_trigger_path_is_excluded(config, relative_path)
    {
        return Ok(None);
    }

    let mode = effective_file_creation_mode(&config.templates);
    let body_is_empty = find_frontmatter_block(contents)
        .map_or(contents, |(_, _, body_start)| &contents[body_start..])
        .is_empty();
    let template_name = if body_is_empty {
        creation_template_for_path(&config.templates, relative_path, mode)?
    } else {
        None
    };

    if let Some(template_name) = template_name {
        let loaded = load_named_template(paths, config, template_name)?;
        let vars = HashMap::new();
        let rendered = render_loaded_template_with_staged_creates(
            paths,
            config,
            &loaded,
            &LoadedTemplateRenderRequest {
                target_path: relative_path,
                target_contents: Some(contents),
                engine: TemplateEngineKind::Auto,
                vars: &vars,
                allow_mutations: true,
                run_mode: TemplateRunMode::Create,
                reference_date: None,
            },
            read_filter,
            mutation_guard,
            staged_creates,
        )?;
        let (target_frontmatter, _) =
            parse_frontmatter_document(contents, false).map_err(AppError::operation)?;
        let (template_frontmatter, template_body) =
            parse_frontmatter_document(&rendered.content, true).map_err(AppError::operation)?;
        let content = render_note_from_parts(
            merge_template_frontmatter(target_frontmatter, template_frontmatter).as_ref(),
            &template_body,
        )
        .map_err(AppError::operation)?;
        let mut warnings = loaded.template.warning.into_iter().collect::<Vec<_>>();
        warnings.extend(rendered.warnings);
        return Ok(Some(RenderedCreationTrigger {
            content,
            target_path: rendered.target_path,
            template: Some(template_name.to_string()),
            engine: rendered.engine,
            warnings,
            diagnostics: rendered.diagnostics,
            changed_paths: rendered.changed_paths,
        }));
    }

    if contents.len() > MAX_FILE_CREATION_TRIGGER_BYTES
        || (!contents.contains("<%") && !contents.contains("{{"))
    {
        return Ok(None);
    }

    let discovery = discover_templates(
        paths,
        config.templates.obsidian_folder.as_deref(),
        config.templates.templater_folder.as_deref(),
    )?;
    let vars = HashMap::new();
    let rendered = render_template_request_with_authority(
        TemplateRenderRequest {
            paths,
            vault_config: config,
            templates: &discovery.templates,
            template_path: None,
            template_text: contents,
            target_path: relative_path,
            target_contents: Some(contents),
            engine: TemplateEngineKind::Auto,
            vars: &vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
            reference_date: None,
        },
        read_filter,
        mutation_guard,
        staged_creates,
    )?;
    Ok(Some(RenderedCreationTrigger {
        content: rendered.content,
        target_path: rendered.target_path,
        template: None,
        engine: rendered.engine,
        warnings: discovery
            .warnings
            .into_iter()
            .chain(rendered.warnings)
            .collect(),
        diagnostics: rendered.diagnostics,
        changed_paths: rendered.changed_paths,
    }))
}

fn effective_file_creation_mode(config: &TemplatesConfig) -> TemplaterFileCreationMode {
    config.trigger_on_file_creation_mode.unwrap_or({
        if config.enable_folder_templates {
            TemplaterFileCreationMode::Folder
        } else if config.enable_file_templates {
            TemplaterFileCreationMode::Regex
        } else {
            TemplaterFileCreationMode::None
        }
    })
}

fn creation_template_for_path<'a>(
    config: &'a TemplatesConfig,
    relative_path: &str,
    mode: TemplaterFileCreationMode,
) -> Result<Option<&'a str>, AppError> {
    match mode {
        TemplaterFileCreationMode::None => Ok(None),
        TemplaterFileCreationMode::Folder => {
            let mut folder = Path::new(relative_path).parent();
            while let Some(candidate) = folder {
                if let Some(mapping) = config
                    .folder_templates
                    .iter()
                    .find(|mapping| mapping.folder == candidate)
                {
                    return Ok(Some(mapping.template.as_str()));
                }
                folder = candidate.parent();
            }
            Ok(None)
        }
        TemplaterFileCreationMode::Regex => {
            for mapping in &config.file_templates {
                let regex = Regex::new(&mapping.regex).map_err(|error| {
                    AppError::operation(format!(
                        "invalid template file-creation regex `{}`: {error}",
                        mapping.regex
                    ))
                })?;
                if regex.is_match(relative_path) {
                    return Ok(Some(mapping.template.as_str()));
                }
            }
            Ok(None)
        }
    }
}

fn creation_trigger_path_is_excluded(config: &VaultConfig, relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    path.starts_with(Path::new(".vulcan/templates"))
        || config
            .templates
            .templater_folder
            .iter()
            .chain(config.templates.obsidian_folder.iter())
            .any(|folder| path.starts_with(folder))
        || config
            .templates
            .ignore_folders_on_creation
            .iter()
            .any(|ignored| path.starts_with(&ignored.folder))
}

pub fn build_template_show_report(
    paths: &VaultPaths,
    name: &str,
) -> Result<TemplateShowReport, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    let config = load_vault_config(paths).config;
    let loaded = load_named_template(paths, &config, name)?;
    Ok(TemplateShowReport {
        name: loaded.template.name,
        source: loaded.template.source.to_string(),
        path: loaded.template.display_path,
        inputs: prompt::discover_template_inputs(&loaded.source),
        content: loaded.source,
    })
}

pub fn build_template_preview_report(
    paths: &VaultPaths,
    request: &TemplatePreviewRequest,
) -> Result<TemplatePreviewReport, AppError> {
    build_template_preview_report_with_filter(paths, request, None)
}

pub fn build_template_preview_report_with_filter(
    paths: &VaultPaths,
    request: &TemplatePreviewRequest,
    read_filter: Option<&PermissionFilter>,
) -> Result<TemplatePreviewReport, AppError> {
    let _read_guard = vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    let config = load_vault_config(paths).config;
    let loaded = load_named_template(paths, &config, &request.template)?;
    let output_path = template_output_path(
        &loaded.template.name,
        request.output_path.as_deref(),
        &TemplateTimestamp::current(),
    )?;
    let rendered = render_loaded_template_with_filter(
        paths,
        &config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path: &output_path,
            target_contents: None,
            engine: request.engine,
            vars: &request.vars,
            allow_mutations: false,
            run_mode: TemplateRunMode::Dynamic,
            reference_date: None,
        },
        read_filter,
    )?;
    Ok(TemplatePreviewReport {
        template: loaded.template.name,
        template_source: loaded.template.source.to_string(),
        path: rendered.target_path,
        engine: rendered.engine.as_str().to_string(),
        content: rendered.content,
        warnings: loaded
            .template
            .warning
            .into_iter()
            .chain(rendered.warnings)
            .collect(),
        diagnostics: rendered.diagnostics,
    })
}

pub fn apply_template_create(
    paths: &VaultPaths,
    request: &TemplateCreateRequest,
) -> Result<TemplateCreateReport, AppError> {
    apply_template_create_with_filter(paths, request, None)
}

pub fn apply_template_create_with_filter(
    paths: &VaultPaths,
    request: &TemplateCreateRequest,
    read_filter: Option<&PermissionFilter>,
) -> Result<TemplateCreateReport, AppError> {
    let config = load_vault_config(paths).config;
    let loaded = load_named_template(paths, &config, &request.template)?;
    let output_path = template_output_path(
        &loaded.template.name,
        request.output_path.as_deref(),
        &TemplateTimestamp::current(),
    )?;
    let staged = staged_template_creates();
    let rendered = render_loaded_template_with_staged_creates(
        paths,
        &config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path: &output_path,
            target_contents: None,
            engine: request.engine,
            vars: &request.vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
            reference_date: None,
        },
        read_filter,
        None,
        Some(staged.clone()),
    )?;
    let absolute_output = paths.vault_root().join(&rendered.target_path);
    write_template_result_with_staged_creates(
        paths,
        &rendered.target_path,
        None,
        &rendered.content,
        &staged,
        None,
        "template create",
    )?;

    let mut changed_paths = vec![rendered.target_path.clone()];
    changed_paths.extend(rendered.changed_paths.iter().cloned());
    changed_paths.sort();
    changed_paths.dedup();

    Ok(TemplateCreateReport {
        template: loaded.template.name,
        template_source: loaded.template.source.to_string(),
        path: rendered.target_path,
        engine: rendered.engine.as_str().to_string(),
        warnings: loaded
            .template
            .warning
            .into_iter()
            .chain(rendered.warnings)
            .collect(),
        diagnostics: rendered.diagnostics,
        absolute_path: absolute_output,
        changed_paths,
    })
}

pub fn apply_template_insert(
    paths: &VaultPaths,
    request: &TemplateInsertRequest,
) -> Result<TemplateInsertReport, AppError> {
    apply_template_insert_with_filter(paths, request, None, None, Verbosity::Quiet)
}

/// Insert a rendered template into a note. Like every note write, the
/// blocking `on_note_write` hooks run first and may veto it; the write and
/// the hooks use `permission_profile`.
pub fn apply_template_insert_with_filter(
    paths: &VaultPaths,
    request: &TemplateInsertRequest,
    read_filter: Option<&PermissionFilter>,
    permission_profile: Option<&str>,
    verbosity: Verbosity,
) -> Result<TemplateInsertReport, AppError> {
    let config = load_vault_config(paths).config;
    let loaded = load_named_template(paths, &config, &request.template)?;
    let resolved = resolve_note_reference(paths, &request.note).map_err(AppError::operation)?;
    let target_path = resolved.path;
    let target_absolute = paths.vault_root().join(&target_path);
    let target_source = fs::read_to_string(&target_absolute).map_err(AppError::operation)?;
    let staged = staged_template_creates();
    let rendered = render_loaded_template_with_staged_creates(
        paths,
        &config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path: &target_path,
            target_contents: Some(&target_source),
            engine: request.engine,
            vars: &request.vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Append,
            reference_date: None,
        },
        read_filter,
        None,
        Some(staged.clone()),
    )?;
    let expected_final = if rendered.target_path == target_path {
        target_source.clone()
    } else {
        template_path_content(paths, Some(&staged), &rendered.target_path)
            .map_err(AppError::operation)?
    };
    let prepared = prepare_template_insertion(&expected_final, &rendered.content)
        .map_err(AppError::operation)?;
    let updated =
        apply_template_insertion_mode(&prepared, request.mode).map_err(AppError::operation)?;
    write_template_note(
        paths,
        &TemplateNoteWrite {
            path: &rendered.target_path,
            before: &expected_final,
            after: &updated,
            hook_previous: Some(&expected_final),
            operation: "template insert",
            hook_operation: "template-insert",
        },
        &staged,
        permission_profile,
        verbosity,
    )?;

    let mut changed_paths = vec![rendered.target_path.clone()];
    changed_paths.extend(rendered.changed_paths.iter().cloned());
    changed_paths.sort();
    changed_paths.dedup();

    Ok(TemplateInsertReport {
        template: loaded.template.name,
        template_source: loaded.template.source.to_string(),
        note: rendered.target_path,
        mode: request.mode.as_str().to_string(),
        engine: rendered.engine.as_str().to_string(),
        warnings: loaded
            .template
            .warning
            .into_iter()
            .chain(rendered.warnings)
            .collect(),
        diagnostics: rendered.diagnostics,
        changed_paths,
    })
}

struct TemplateSession<'a> {
    request: TemplateRenderRequest<'a>,
    target_path: String,
    target_contents: String,
    /// The note's reference moment for built-in date variables.
    timestamp: TemplateTimestamp,
    /// Wall clock for `tp.date.*` and other "now" semantics.
    now: TemplateTimestamp,
    warnings: Vec<String>,
    diagnostics: Vec<String>,
    changed_paths: BTreeSet<String>,
    pending_frontmatter: Option<YamlMapping>,
    prompt_count: usize,
    suggester_count: usize,
    quickadd_value_vars: HashMap<String, String>,
    quickadd_date_vars: HashMap<String, i64>,
    read_filter: Option<PermissionFilter>,
    mutation_guard: Option<ProfilePermissionGuard>,
    staged_creates: Option<StagedTemplateCreates>,
    /// Include nesting of this render; nested renders start one deeper.
    include_depth: usize,
    #[cfg(feature = "js_runtime")]
    js_runtime: Option<JsTemplateRuntime>,
    /// The last JS program run, kept to locate errors from hooks it registered.
    #[cfg(feature = "js_runtime")]
    last_js_program: Option<(String, String, program::CompiledProgram)>,
}

impl<'a> TemplateSession<'a> {
    fn new(
        request: TemplateRenderRequest<'a>,
        _engine: TemplateEngineKind,
        read_filter: Option<&PermissionFilter>,
        mutation_guard: Option<&ProfilePermissionGuard>,
        staged_creates: Option<StagedTemplateCreates>,
    ) -> Self {
        let now = TemplateTimestamp::current();
        Self {
            target_path: request.target_path.to_string(),
            target_contents: request.target_contents.unwrap_or_default().to_string(),
            timestamp: request
                .reference_date
                .and_then(|date| now.on_date(date))
                .unwrap_or(now),
            now,
            request,
            warnings: Vec::new(),
            diagnostics: Vec::new(),
            changed_paths: BTreeSet::new(),
            pending_frontmatter: None,
            prompt_count: 0,
            suggester_count: 0,
            quickadd_value_vars: HashMap::new(),
            quickadd_date_vars: HashMap::new(),
            read_filter: read_filter.cloned(),
            mutation_guard: mutation_guard.cloned(),
            staged_creates,
            include_depth: 0,
            #[cfg(feature = "js_runtime")]
            js_runtime: None,
            #[cfg(feature = "js_runtime")]
            last_js_program: None,
        }
    }

    fn templates_config(&self) -> &TemplatesConfig {
        &self.request.vault_config.templates
    }

    fn builtins_for_path(&self, path: &str) -> TemplateVariables {
        template_variables_for_path(path, self.timestamp)
    }

    fn current_builtins(&self) -> TemplateVariables {
        self.builtins_for_path(&self.target_path)
    }

    fn render_native_text(&mut self, template: &str) -> Result<String, CliError> {
        self.render_native_text_with_depth(template, 0)
    }

    fn render_native_text_with_depth(
        &mut self,
        template: &str,
        depth: usize,
    ) -> Result<String, CliError> {
        if depth > MAX_QUICKADD_EXPANSION_DEPTH {
            return Err(CliError::operation(format!(
                "Reached QuickAdd expansion depth limit (max = {MAX_QUICKADD_EXPANSION_DEPTH})"
            )));
        }

        let mut rendered = String::with_capacity(template.len());
        let mut remaining = template;

        while let Some(start) = remaining.find("{{") {
            rendered.push_str(&remaining[..start]);
            let rest = &remaining[start + 2..];
            let Some(end) = rest.find("}}") else {
                rendered.push_str(&remaining[start..]);
                return Ok(rendered);
            };

            let expression = rest[..end].trim();
            let original = &remaining[start..start + end + 4];
            let replacement = self
                .render_quickadd_variable(expression, depth)?
                .or_else(|| {
                    render_template_variable(
                        expression,
                        &self.current_builtins(),
                        self.templates_config(),
                    )
                })
                .unwrap_or_else(|| original.to_string());
            rendered.push_str(&replacement);
            remaining = &rest[end + 2..];
        }

        rendered.push_str(remaining);
        Ok(rendered)
    }

    fn render_quickadd_variable(
        &mut self,
        expression: &str,
        depth: usize,
    ) -> Result<Option<String>, CliError> {
        let trimmed = expression.trim();

        if let Some(rest) = quickadd_strip_keyword(trimmed, "DATE", &[':', '+']) {
            return Ok(Some(self.render_quickadd_date(rest)));
        }
        if let Some(rest) = quickadd_strip_keyword(trimmed, "TIME", &[':']) {
            return Ok(Some(self.render_quickadd_time(rest)));
        }
        if let Some(rest) = quickadd_strip_keyword(trimmed, "VDATE", &[':']) {
            return self.render_quickadd_vdate(rest);
        }
        if let Some(rest) = quickadd_strip_keyword(trimmed, "GLOBAL_VAR", &[':']) {
            return self.render_quickadd_global_var(rest, depth);
        }
        if let Some(rest) = quickadd_strip_keyword(trimmed, "VALUE", &[':', '|']) {
            return self.render_quickadd_value(rest, "value");
        }
        if let Some(rest) = quickadd_strip_keyword(trimmed, "NAME", &[':', '|']) {
            return self.render_quickadd_value(rest, "name");
        }

        match trimmed {
            value if value.eq_ignore_ascii_case("TITLE") => Ok(Some(self.current_builtins().title)),
            value
                if value.eq_ignore_ascii_case("FILE_NAME")
                    || value.eq_ignore_ascii_case("FILENAMECURRENT") =>
            {
                Ok(Some(current_file_stem(&self.target_path)))
            }
            value if value.eq_ignore_ascii_case("FILE_PATH") => Ok(Some(self.target_path.clone())),
            value if value.eq_ignore_ascii_case("LINKCURRENT") => {
                Ok(Some(current_file_wikilink(&self.target_path)))
            }
            value if value.eq_ignore_ascii_case("RANDOM") => Ok(None),
            _ => Ok(None),
        }
    }

    fn render_quickadd_date(&self, rest: &str) -> String {
        let (format, offset_days) = parse_quickadd_format_and_offset(rest, "YYYY-MM-DD");
        self.timestamp_with_day_offset(offset_days)
            .format_obsidian(&format)
    }

    fn render_quickadd_time(&self, rest: &str) -> String {
        let format = rest
            .strip_prefix(':')
            .map(str::trim)
            .filter(|format| !format.is_empty())
            .unwrap_or("HH:mm");
        self.timestamp.format_obsidian(format)
    }

    fn render_quickadd_vdate(&mut self, rest: &str) -> Result<Option<String>, CliError> {
        let Some(body) = rest.strip_prefix(':') else {
            return Ok(None);
        };
        let Some((name_part, format_part)) = split_once_unescaped(body, ',') else {
            return Ok(None);
        };
        let variable_name = name_part.trim();
        if variable_name.is_empty() {
            return Ok(None);
        }

        let (format_part, default_value) = split_once_unescaped(format_part, '|')
            .map_or((format_part, None), |(format, default)| {
                (format, Some(default.trim()))
            });
        let format = format_part.trim();
        if format.is_empty() {
            return Ok(None);
        }

        let key = quickadd_storage_key(variable_name, "date");
        let millis = if let Some(millis) = self.quickadd_date_vars.get(&key) {
            *millis
        } else {
            let value = lookup_template_var(
                self.request.vars,
                &PromptLookupKey::named(variable_name),
                Some(variable_name),
            )
            .map_or_else(
                || {
                    prompt::resolve_prompt_text(
                        &format!("{{{{VDATE:{variable_name},{format}}}}}"),
                        variable_name,
                        default_value,
                        "<date>",
                    )
                },
                Ok,
            )
            .map_err(|error| match error {
                prompt::PromptError::InputRequired(message) => {
                    CliError::operation_with_code(TEMPLATE_INPUT_REQUIRED_CODE, message)
                }
                prompt::PromptError::Failed(message) => CliError::operation(message),
            })?;
            let millis = quickadd_parse_date_input(&value, self.current_timestamp_millis())
                .ok_or_else(|| {
                    CliError::operation(format!(
                        "failed to parse QuickAdd date input `{value}` for `{variable_name}`"
                    ))
                })?;
            self.quickadd_date_vars.insert(key.clone(), millis);
            millis
        };

        Ok(Some(
            TemplateTimestamp::from_millis(millis).format_obsidian(format),
        ))
    }

    fn render_quickadd_global_var(
        &mut self,
        rest: &str,
        depth: usize,
    ) -> Result<Option<String>, CliError> {
        let Some(name) = rest.strip_prefix(':').map(str::trim) else {
            return Ok(None);
        };
        if name.is_empty() {
            return Ok(None);
        }

        let Some(value) = self.lookup_quickadd_global_var(name) else {
            return Ok(Some(String::new()));
        };
        self.render_native_text_with_depth(&value, depth + 1)
            .map(Some)
    }

    fn render_quickadd_value(
        &mut self,
        rest: &str,
        default_prompt_name: &str,
    ) -> Result<Option<String>, CliError> {
        let spec = parse_quickadd_value_spec(rest);
        let variable_name = spec
            .variable_name
            .as_deref()
            .unwrap_or(default_prompt_name)
            .trim();
        let key = quickadd_storage_key(variable_name, default_prompt_name);

        let mut raw_value = if let Some(value) = self.quickadd_value_vars.get(&key) {
            value.clone()
        } else if let Some(value) =
            self.lookup_quickadd_value_var(variable_name, default_prompt_name)
        {
            value
        } else {
            prompt::resolve_quickadd_value(
                variable_name,
                &spec.choices,
                spec.default_value.as_deref(),
            )
            .map_err(|error| match error {
                prompt::PromptError::InputRequired(message) => {
                    CliError::operation_with_code(TEMPLATE_INPUT_REQUIRED_CODE, message)
                }
                prompt::PromptError::Failed(message) => CliError::operation(message),
            })?
        };

        if raw_value.is_empty() {
            if let Some(default_value) = &spec.default_value {
                raw_value.clone_from(default_value);
            }
        }

        self.quickadd_value_vars.insert(key, raw_value.clone());
        let rendered = spec
            .case_style
            .as_deref()
            .map_or(raw_value.clone(), |case_style| {
                apply_quickadd_case_style(&raw_value, case_style)
            });
        Ok(Some(rendered))
    }

    fn lookup_quickadd_value_var(
        &self,
        variable_name: &str,
        default_prompt_name: &str,
    ) -> Option<String> {
        let lookup = PromptLookupKey::named(variable_name);
        lookup_template_var(self.request.vars, &lookup, Some(variable_name)).or_else(|| {
            if variable_name.eq_ignore_ascii_case(default_prompt_name) {
                None
            } else {
                let fallback = PromptLookupKey::named(default_prompt_name);
                lookup_template_var(self.request.vars, &fallback, Some(default_prompt_name))
            }
        })
    }

    fn lookup_quickadd_global_var(&self, variable_name: &str) -> Option<String> {
        let lookup = PromptLookupKey::named(variable_name);
        self.request
            .vault_config
            .quickadd
            .global_variables
            .get(&lookup.raw)
            .cloned()
            .or_else(|| {
                self.request
                    .vault_config
                    .quickadd
                    .global_variables
                    .iter()
                    .find(|(key, _)| {
                        key.eq_ignore_ascii_case(variable_name)
                            || slugify_var_key(key) == lookup.slug
                    })
                    .map(|(_, value)| value.clone())
            })
    }

    fn timestamp_with_day_offset(&self, offset_days: i64) -> TemplateTimestamp {
        TemplateTimestamp::from_millis(self.timestamp.to_millis() + offset_days * DAY_MS)
    }

    fn render_source(
        &mut self,
        template: &str,
        engine: TemplateEngineKind,
        include_depth: usize,
        source_name: &str,
    ) -> Result<String, CliError> {
        if include_depth > MAX_TEMPLATE_INCLUDE_DEPTH {
            return Err(CliError::operation(format!(
                "Reached inclusion depth limit (max = {MAX_TEMPLATE_INCLUDE_DEPTH})"
            )));
        }

        if engine == TemplateEngineKind::Native {
            return self.render_native_text(template);
        }

        let segments = program::split_templater_segments(template).map_err(|error| {
            located_template_error(
                &CliError::operation_with_code(
                    "template_syntax_error",
                    "unterminated Templater tag: `<%` has no closing `%>`",
                ),
                source_name,
                Some(error.line),
                template,
            )
        })?;

        #[cfg(feature = "js_runtime")]
        if segments.iter().any(|segment| {
            matches!(segment, program::TemplateSegment::Tag { tag, .. } if tag_requires_js(tag))
        }) {
            return self.render_js_program(template, &segments, source_name);
        }

        let mut output = String::with_capacity(template.len());
        for segment in &segments {
            match segment {
                program::TemplateSegment::Text(text) => {
                    output.push_str(&self.render_native_text(text)?);
                }
                program::TemplateSegment::Tag { tag, line } => {
                    let replacement = self.evaluate_tag(tag, include_depth).map_err(|error| {
                        located_template_error(&error, source_name, Some(*line), template)
                    })?;
                    output.push_str(&replacement);
                }
            }
        }
        Ok(output)
    }

    /// Evaluates one tag with the native interpreter. With `js_runtime`,
    /// templates that need JavaScript never reach this; without it, those
    /// tags render empty and leave a diagnostic.
    fn evaluate_tag(
        &mut self,
        tag: &TemplaterTag<'_>,
        include_depth: usize,
    ) -> Result<String, CliError> {
        if tag.dynamic {
            self.push_diagnostic(
                "Templater dynamic tags (`<%+ %>`) are evaluated eagerly in CLI templates"
                    .to_string(),
            );
        }

        if tag.execution {
            self.push_diagnostic(
                "Templater execution tags (`<%* %>`) require the `js_runtime` feature".to_string(),
            );
            return Ok(String::new());
        }

        match self.evaluate_native_expression(tag.body, include_depth) {
            Ok(value) => Ok(template_value_to_string(&value)),
            Err(NativeExpressionError::RequiresJsRuntime(message)) => {
                self.push_diagnostic(message);
                Ok(String::new())
            }
            Err(NativeExpressionError::Message(message)) => Err(CliError::operation(message)),
            Err(NativeExpressionError::InputRequired(message)) => Err(
                CliError::operation_with_code(TEMPLATE_INPUT_REQUIRED_CODE, message),
            ),
        }
    }

    fn evaluate_native_expression(
        &mut self,
        source: &str,
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let expression = parse_native_expression(source)?;
        self.eval_native_expression(&expression, include_depth)
    }

    fn eval_native_expression(
        &mut self,
        expression: &NativeExpression,
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        match expression {
            NativeExpression::String(value) => Ok(TemplateValue::String(value.clone())),
            NativeExpression::Number(value) => Ok(TemplateValue::Number(*value)),
            NativeExpression::Bool(value) => Ok(TemplateValue::Bool(*value)),
            NativeExpression::Null => Ok(TemplateValue::Null),
            NativeExpression::Path(path) => self.eval_native_path(path),
            NativeExpression::Call { callee, args } => {
                self.eval_native_call(callee, args, include_depth)
            }
        }
    }

    fn eval_native_path(
        &mut self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(first) = path.first() else {
            return Ok(TemplateValue::Null);
        };
        let NativePathPart::Name(root) = first else {
            return Err(NativeExpressionError::Message(
                "templater expressions must start with an identifier".to_string(),
            ));
        };
        if root == "tp" {
            return self.resolve_tp_path(path);
        }

        if path.len() == 1 {
            return Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater expression `{root}` requires the `js_runtime` feature"
            )));
        }

        Err(NativeExpressionError::RequiresJsRuntime(
            "templater expression requires the `js_runtime` feature".to_string(),
        ))
    }

    fn resolve_tp_path(
        &mut self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        if path.len() < 2 {
            return Ok(TemplateValue::Null);
        }

        match path_name(path, 1) {
            Some("file") => self.resolve_tp_file_path(path),
            Some("frontmatter") => Ok(self.resolve_tp_frontmatter_path(path)),
            Some("config") => Ok(self.resolve_tp_config_path(path)),
            Some("obsidian") => Err(NativeExpressionError::RequiresJsRuntime(
                "tp.obsidian helpers require the `js_runtime` feature".to_string(),
            )),
            Some("app") => {
                self.push_diagnostic("tp.app.* is not available in CLI templates".to_string());
                Ok(TemplateValue::Null)
            }
            Some("date" | "system") => Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater function `{}` must be called, not accessed as a value",
                join_native_path(path)
            ))),
            _ => Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater path `{}` requires the `js_runtime` feature",
                join_native_path(path)
            ))),
        }
    }

    fn resolve_tp_file_path(
        &mut self,
        path: &[NativePathPart],
    ) -> Result<TemplateValue, NativeExpressionError> {
        if path.len() < 3 {
            return Ok(TemplateValue::Object(self.current_file_object()));
        }
        match path_name(path, 2) {
            Some("title") => Ok(TemplateValue::String(self.current_title())),
            Some("content") => Ok(TemplateValue::String(self.target_contents.clone())),
            Some("tags") => Ok(TemplateValue::Array(
                self.current_tags()
                    .into_iter()
                    .map(TemplateValue::String)
                    .collect(),
            )),
            Some("path") if path.len() == 3 => Ok(TemplateValue::String(self.target_path.clone())),
            Some("folder") if path.len() == 3 => Ok(TemplateValue::String(self.relative_folder())),
            Some(name) => Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater path `tp.file.{name}` requires the `js_runtime` feature"
            ))),
            None => Ok(TemplateValue::Null),
        }
    }

    fn resolve_tp_frontmatter_path(&self, path: &[NativePathPart]) -> TemplateValue {
        let frontmatter = current_frontmatter_mapping(&self.target_contents).unwrap_or_default();
        let mut value = JsonValue::Object(yaml_mapping_to_json_object(&frontmatter));

        for part in path.iter().skip(2) {
            let key = match part {
                NativePathPart::Name(name) | NativePathPart::Index(name) => name,
            };
            value = value
                .as_object()
                .and_then(|object| object.get(key))
                .cloned()
                .unwrap_or(JsonValue::Null);
        }

        TemplateValue::from_json(value)
    }

    fn resolve_tp_config_path(&self, path: &[NativePathPart]) -> TemplateValue {
        let config = self.config_json_value();
        let mut value = config;
        for part in path.iter().skip(2) {
            let key = match part {
                NativePathPart::Name(name) | NativePathPart::Index(name) => name,
            };
            value = value
                .as_object()
                .and_then(|object| object.get(key))
                .cloned()
                .unwrap_or(JsonValue::Null);
        }
        TemplateValue::from_json(value)
    }

    fn eval_native_call(
        &mut self,
        callee: &[NativePathPart],
        args: &[NativeExpression],
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let signature = join_native_path(callee);
        self.eval_native_call_inner(&signature, args, include_depth)
            .map_err(|error| match error {
                NativeExpressionError::Message(message) => {
                    NativeExpressionError::Message(format!("{signature}(): {message}"))
                }
                other @ (NativeExpressionError::RequiresJsRuntime(_)
                | NativeExpressionError::InputRequired(_)) => other,
            })
    }

    fn eval_native_call_inner(
        &mut self,
        signature: &str,
        args: &[NativeExpression],
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let args = self.eval_native_call_args(args, include_depth)?;
        let args = args.as_slice();
        match signature {
            "tp.date.now" => self.tp_date_now(args, false),
            "tp.date.tomorrow" => self.tp_date_fixed_offset(args, 1),
            "tp.date.yesterday" => self.tp_date_fixed_offset(args, -1),
            "tp.date.weekday" => self.tp_date_weekday(args),
            "tp.file.path" => Ok(self.tp_file_path_call(args)),
            "tp.file.folder" => Ok(self.tp_file_folder_call(args)),
            "tp.file.creation_date" => Ok(self.tp_file_timestamp_call(args, true)),
            "tp.file.last_modified_date" => Ok(self.tp_file_timestamp_call(args, false)),
            "tp.file.exists" => Ok(self.tp_file_exists_call(args)),
            "tp.file.include" => self.tp_file_include_call(args, include_depth),
            "tp.file.create_new" => self.tp_file_create_new_call(args),
            "tp.file.move" => self.tp_file_move_call(args),
            "tp.file.rename" => self.tp_file_rename_call(args),
            "tp.file.cursor" => Ok(TemplateValue::String(
                self.request
                    .vars
                    .get(TEMPLATE_CURSOR_MARKER_VAR)
                    .cloned()
                    .unwrap_or_default(),
            )),
            "tp.file.find_tfile" => Ok(self.tp_file_find_tfile_call(args)),
            "tp.system.prompt" => self.tp_system_prompt_call(args),
            "tp.system.suggester" => self.tp_system_suggester_call(args),
            "tp.system.clipboard" => Ok(TemplateValue::String(read_clipboard_best_effort())),
            signature
                if signature.starts_with("tp.web.")
                    || signature.starts_with("tp.user.")
                    || signature.starts_with("tp.obsidian.") =>
            {
                Err(NativeExpressionError::RequiresJsRuntime(format!(
                    "templater function `{signature}` requires the `js_runtime` feature"
                )))
            }
            _ => Err(NativeExpressionError::RequiresJsRuntime(format!(
                "templater function `{signature}` requires the `js_runtime` feature"
            ))),
        }
    }

    /// Templater evaluates call arguments like JavaScript does, so an argument
    /// such as `tp.file.title` must reach the callee as its value. Non-scalar
    /// results keep their original expression.
    fn eval_native_call_args(
        &mut self,
        args: &[NativeExpression],
        include_depth: usize,
    ) -> Result<Vec<NativeExpression>, NativeExpressionError> {
        args.iter()
            .map(|arg| {
                if !matches!(
                    arg,
                    NativeExpression::Path(_) | NativeExpression::Call { .. }
                ) {
                    return Ok(arg.clone());
                }
                Ok(match self.eval_native_expression(arg, include_depth)? {
                    TemplateValue::String(value) => NativeExpression::String(value),
                    TemplateValue::Number(value) => NativeExpression::Number(value),
                    TemplateValue::Bool(value) => NativeExpression::Bool(value),
                    TemplateValue::Null => NativeExpression::Null,
                    TemplateValue::Array(_) | TemplateValue::Object(_) => arg.clone(),
                })
            })
            .collect()
    }

    fn tp_date_now(
        &self,
        args: &[NativeExpression],
        weekday_mode: bool,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let format = string_arg(args, 0).unwrap_or_else(|| "YYYY-MM-DD".to_string());
        let offset = args.get(1);
        let reference = string_arg(args, 2);
        let reference_format = string_arg(args, 3);
        let mut millis =
            self.reference_timestamp(reference.as_deref(), reference_format.as_deref())?;
        if !weekday_mode {
            millis = apply_date_offset(millis, offset)?;
        }
        Ok(TemplateValue::String(format_date(millis, &format)))
    }

    fn tp_date_fixed_offset(
        &self,
        args: &[NativeExpression],
        day_offset: i64,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let format = string_arg(args, 0).unwrap_or_else(|| "YYYY-MM-DD".to_string());
        let millis = self.reference_timestamp(None, None)? + day_offset * 86_400_000;
        Ok(TemplateValue::String(format_date(millis, &format)))
    }

    fn tp_date_weekday(
        &self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let format = string_arg(args, 0).unwrap_or_else(|| "YYYY-MM-DD".to_string());
        let weekday = number_arg(args, 1).ok_or_else(|| {
            NativeExpressionError::Message("tp.date.weekday requires a weekday number".to_string())
        })?;
        let reference = string_arg(args, 2);
        let reference_format = string_arg(args, 3);
        let millis = self.reference_timestamp(reference.as_deref(), reference_format.as_deref())?;
        let target = weekday_timestamp(millis, weekday);
        Ok(TemplateValue::String(format_date(target, &format)))
    }

    fn tp_file_path_call(&self, args: &[NativeExpression]) -> TemplateValue {
        TemplateValue::String(templater_file_path(
            self.request.paths,
            &self.target_path,
            bool_arg(args, 0).unwrap_or(false),
        ))
    }

    fn tp_file_folder_call(&self, args: &[NativeExpression]) -> TemplateValue {
        TemplateValue::String(templater_file_folder(
            &self.target_path,
            bool_arg(args, 0).unwrap_or(false),
        ))
    }

    fn tp_file_timestamp_call(&self, args: &[NativeExpression], creation: bool) -> TemplateValue {
        let format = string_arg(args, 0).unwrap_or_else(|| DEFAULT_FILE_DATE_FORMAT.to_string());
        let absolute = self.request.paths.vault_root().join(&self.target_path);
        let timestamp = if absolute.is_file() {
            file_timestamp_millis(&absolute, creation)
        } else {
            Some(self.current_timestamp_millis())
        }
        .unwrap_or_else(|| self.current_timestamp_millis());
        TemplateValue::String(format_date(timestamp, &format))
    }

    fn tp_file_exists_call(&self, args: &[NativeExpression]) -> TemplateValue {
        let Some(path) = string_arg(args, 0) else {
            return TemplateValue::Bool(false);
        };
        let exists = self.resolve_vault_path(&path).is_some();
        TemplateValue::Bool(exists)
    }

    fn tp_file_include_call(
        &mut self,
        args: &[NativeExpression],
        include_depth: usize,
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(path) = string_arg(args, 0) else {
            return Ok(TemplateValue::Null);
        };
        let include_path = self.resolve_include_target(&path)?;
        let source = template_path_content(
            self.request.paths,
            self.staged_creates.as_ref(),
            &include_path,
        )
        .map_err(NativeExpressionError::Message)?;
        let source = match split_include_link(&path).1 {
            Some(subpath) => extract_include_subpath(&source, subpath, self.request.vault_config)
                .map_err(NativeExpressionError::Message)?,
            None => source,
        };
        let rendered = self
            .render_source(
                &source,
                TemplateEngineKind::Templater,
                include_depth + 1,
                &include_path,
            )
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        self.capture_frontmatter_from_rendered_include(&rendered)?;
        let (_, body) = parse_frontmatter_document(&rendered, true)
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        Ok(TemplateValue::String(body))
    }

    fn tp_file_create_new_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        if !self.request.allow_mutations {
            self.push_diagnostic(
                "tp.file.create_new() is disabled during template preview".to_string(),
            );
            return Ok(TemplateValue::Null);
        }

        let Some(template_arg) = args.first() else {
            return Ok(TemplateValue::Null);
        };
        let filename = string_arg(args, 1).unwrap_or_else(|| "Untitled".to_string());
        let folder = string_arg(args, 3).unwrap_or_default();
        let relative_path = if folder.trim().is_empty() {
            format!("{filename}.md")
        } else {
            format!("{}/{}.md", folder.trim_matches('/'), filename)
        };
        let normalized =
            normalize_note_output_path(&relative_path).map_err(NativeExpressionError::Message)?;
        // Templater semantics: a TFile renders that template file for the new
        // note; a string is template content rendered for the new note.
        let (template_path, source) = match self.eval_native_expression(template_arg, 0)? {
            TemplateValue::Object(file) => {
                let identifier = file
                    .get("path")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_string();
                let target = self.resolve_vault_path(&identifier).ok_or_else(|| {
                    NativeExpressionError::Message(format!("template {identifier} doesn't exist"))
                })?;
                let source = template_path_content(
                    self.request.paths,
                    self.staged_creates.as_ref(),
                    &target,
                )
                .map_err(NativeExpressionError::Message)?;
                (Some(self.request.paths.vault_root().join(target)), source)
            }
            other => (None, template_value_to_string(&other)),
        };
        let content = self
            .render_nested_template(template_path.as_deref(), &source, &normalized)
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?
            .content;
        if let Some(guard) = self.mutation_guard.as_ref() {
            guard
                .check_write_path(&normalized)
                .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        }
        if let Some(staged) = self.staged_creates.as_ref() {
            stage_template_create(staged, self.request.paths, &normalized, content)
                .map_err(NativeExpressionError::Message)?;
        } else {
            write_ordinary_note_if_unchanged_with_profile(
                self.request.paths,
                &normalized,
                None,
                &content,
                "template file create",
                self.mutation_guard
                    .as_ref()
                    .map(|guard| guard.selection().name.as_str()),
            )
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        }
        self.changed_paths.insert(normalized.clone());
        Ok(TemplateValue::Object(file_object_json(&normalized)))
    }

    fn tp_file_move_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(new_path) = string_arg(args, 0) else {
            return Ok(TemplateValue::Null);
        };
        let normalized =
            normalize_note_output_path(&new_path).map_err(NativeExpressionError::Message)?;
        if !self.request.allow_mutations {
            self.push_diagnostic("tp.file.move() is disabled during template preview".to_string());
            return Ok(TemplateValue::Null);
        }

        if let Some(staged) = self.staged_creates.as_ref() {
            if let Some(summary) = stage_template_move(
                staged,
                self.request.paths,
                &self.target_path,
                &normalized,
                self.request.target_contents,
                self.mutation_guard.as_ref(),
            )
            .map_err(NativeExpressionError::Message)?
            {
                self.record_move_summary(&summary);
            }
        } else if self
            .request
            .paths
            .vault_root()
            .join(&self.target_path)
            .is_file()
        {
            let summary = guarded_template_move(
                self.request.paths,
                &self.target_path,
                &normalized,
                self.mutation_guard.as_ref(),
            )
            .map_err(NativeExpressionError::Message)?;
            self.record_move_summary(&summary);
        } else if let Some(guard) = self.mutation_guard.as_ref() {
            guard
                .check_write_path(&normalized)
                .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        }
        self.target_path = normalized;
        Ok(TemplateValue::String(String::new()))
    }

    fn tp_file_rename_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        let Some(new_name) = string_arg(args, 0) else {
            return Ok(TemplateValue::Null);
        };
        if new_name.contains(['\\', '/', ':']) {
            return Err(NativeExpressionError::Message(
                "File name cannot contain any of these characters: \\ / :".to_string(),
            ));
        }
        let folder = self.relative_folder();
        let path = if folder.is_empty() {
            format!("{new_name}.md")
        } else {
            format!("{folder}/{new_name}.md")
        };
        self.tp_file_move_call(&[NativeExpression::String(path)])
    }

    fn tp_file_find_tfile_call(&self, args: &[NativeExpression]) -> TemplateValue {
        let Some(identifier) = string_arg(args, 0) else {
            return TemplateValue::Null;
        };
        let Some(path) = self.resolve_vault_path(&identifier) else {
            return TemplateValue::Null;
        };
        TemplateValue::Object(file_object_json(&path))
    }

    fn tp_system_prompt_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        self.prompt_count += 1;
        let message = string_arg(args, 0).unwrap_or_default();
        let default = string_arg(args, 1);
        let answer = prompt::resolve_prompt(
            self.request.vars,
            &prompt::PromptCall {
                message: &message,
                default: default.as_deref(),
                throw_on_cancel: bool_arg(args, 2).unwrap_or(false),
                multiline: bool_arg(args, 3).unwrap_or(false),
                ordinal: self.prompt_count,
            },
        )?;
        Ok(answer.map_or(TemplateValue::Null, TemplateValue::String))
    }

    fn tp_system_suggester_call(
        &mut self,
        args: &[NativeExpression],
    ) -> Result<TemplateValue, NativeExpressionError> {
        self.suggester_count += 1;
        let mut evaluate = |index: usize| {
            args.get(index)
                .map(|expression| self.eval_native_expression(expression, 0))
                .transpose()
                .map(|value| value.unwrap_or(TemplateValue::Array(Vec::new())))
        };
        let options = evaluate(0)?.as_string_list();
        let items = match evaluate(1)? {
            TemplateValue::Array(items) => items,
            TemplateValue::Null => Vec::new(),
            item => vec![item],
        };
        if options.len() != items.len() {
            return Err(NativeExpressionError::Message(format!(
                "text_items has {} entries but items has {}",
                options.len(),
                items.len()
            )));
        }
        let item_keys: Vec<String> = items.iter().map(template_value_to_string).collect();
        let placeholder = string_arg(args, 3).unwrap_or_default();
        let choice = prompt::resolve_suggestion(
            self.request.vars,
            &prompt::SuggestCall {
                options: &options,
                item_keys: &item_keys,
                throw_on_cancel: bool_arg(args, 2).unwrap_or(false),
                placeholder: &placeholder,
                limit: number_arg(args, 4).and_then(|limit| usize::try_from(limit).ok()),
                multiple: false,
                ordinal: self.suggester_count,
            },
        )?;
        Ok(choice
            .and_then(|indices| indices.first().copied())
            .and_then(|index| items.get(index).cloned())
            .unwrap_or(TemplateValue::Null))
    }

    /// Renders another template for `target_path` with this render's inputs
    /// and authority, one include level deeper.
    fn render_nested_template(
        &mut self,
        template_path: Option<&Path>,
        template_text: &str,
        target_path: &str,
    ) -> Result<TemplateRenderOutput, CliError> {
        let output = render_template_request_at_depth(
            TemplateRenderRequest {
                paths: self.request.paths,
                vault_config: self.request.vault_config,
                templates: self.request.templates,
                template_path,
                template_text,
                target_path,
                target_contents: None,
                engine: TemplateEngineKind::Templater,
                vars: self.request.vars,
                allow_mutations: self.request.allow_mutations,
                run_mode: self.request.run_mode,
                reference_date: self.request.reference_date,
            },
            self.read_filter.as_ref(),
            self.mutation_guard.as_ref(),
            self.staged_creates.clone(),
            self.include_depth + 1,
        )?;
        self.changed_paths
            .extend(output.changed_paths.iter().cloned());
        for diagnostic in &output.diagnostics {
            self.push_diagnostic(diagnostic.clone());
        }
        Ok(output)
    }

    fn record_move_summary(&mut self, summary: &vulcan_core::MoveSummary) {
        self.changed_paths.insert(summary.destination_path.clone());
        for rewritten in &summary.rewritten_files {
            self.changed_paths.insert(rewritten.path.clone());
        }
    }

    fn resolve_include_target(&self, include: &str) -> Result<String, NativeExpressionError> {
        let (target, _) = split_include_link(include);
        self.resolve_vault_path(target)
            .ok_or_else(|| NativeExpressionError::Message(format!("File {include} doesn't exist")))
    }

    fn resolve_vault_path(&self, identifier: &str) -> Option<String> {
        if let Ok(reference) = resolve_note_reference(self.request.paths, identifier) {
            let normalized = normalize_template_include_path(&reference.path)?;
            return self
                .include_path_is_allowed(&normalized)
                .then(|| normalized.clone())
                .filter(|path| {
                    template_path_exists(self.request.paths, self.staged_creates.as_ref(), path)
                });
        }
        let path = Path::new(identifier);
        let candidate = if path.extension().is_none() {
            PathBuf::from(format!("{identifier}.md"))
        } else {
            path.to_path_buf()
        };
        let normalized = normalize_template_include_path(&candidate.to_string_lossy())?;
        self.include_path_is_allowed(&normalized)
            .then(|| normalized.clone())
            .filter(|path| {
                template_path_exists(self.request.paths, self.staged_creates.as_ref(), path)
            })
    }

    fn include_path_is_allowed(&self, path: &str) -> bool {
        self.read_filter
            .as_ref()
            .is_none_or(|filter| filter.is_allowed(path))
    }

    fn capture_frontmatter_from_rendered_include(
        &mut self,
        rendered: &str,
    ) -> Result<(), NativeExpressionError> {
        let (frontmatter, _) = parse_frontmatter_document(rendered, true)
            .map_err(|error| NativeExpressionError::Message(error.to_string()))?;
        if let Some(frontmatter) = frontmatter {
            merge_pending_frontmatter_mapping(&mut self.pending_frontmatter, &frontmatter);
        }
        Ok(())
    }

    fn merge_pending_frontmatter(&mut self, rendered: &str) -> Result<String, CliError> {
        let Some(pending) = self.pending_frontmatter.take() else {
            return Ok(rendered.to_string());
        };
        let (frontmatter, body) =
            parse_frontmatter_document(rendered, true).map_err(CliError::operation)?;
        let merged = match frontmatter {
            Some(mut existing) => {
                merge_yaml_mappings(&mut existing, &pending);
                existing
            }
            None => pending,
        };
        with_frontmatter(&merged, &body).map_err(CliError::operation)
    }

    fn current_title(&self) -> String {
        Path::new(&self.target_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled")
            .to_string()
    }

    fn relative_folder(&self) -> String {
        Path::new(&self.target_path)
            .parent()
            .map(|parent| parent.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    }

    fn current_tags(&self) -> Vec<String> {
        let parsed = parse_document(&self.target_contents, self.request.vault_config);
        parsed.tags.iter().map(|tag| tag.tag_text.clone()).collect()
    }

    fn current_file_object(&self) -> JsonMap<String, JsonValue> {
        file_object_json(&self.target_path)
    }

    fn current_timestamp_millis(&self) -> i64 {
        self.now.to_millis()
    }

    fn reference_timestamp(
        &self,
        reference: Option<&str>,
        reference_format: Option<&str>,
    ) -> Result<i64, NativeExpressionError> {
        if let Some(reference) = reference {
            if let Some(format) = reference_format {
                return parse_date_with_format(reference, format).ok_or_else(|| {
                    NativeExpressionError::Message(format!(
                        "failed to parse reference date `{reference}` with format `{format}`"
                    ))
                });
            }
            return parse_date_like_string(reference).ok_or_else(|| {
                NativeExpressionError::Message(format!(
                    "failed to parse reference date `{reference}`"
                ))
            });
        }
        Ok(self.current_timestamp_millis())
    }

    fn config_json_value(&self) -> JsonValue {
        JsonValue::Object(JsonMap::from_iter([
            (
                "template_file".to_string(),
                self.request
                    .template_path
                    .map(|path| path_to_relative_file_json(self.request.paths, path))
                    .map_or(JsonValue::Null, JsonValue::Object),
            ),
            (
                "target_file".to_string(),
                JsonValue::Object(self.current_file_object()),
            ),
            (
                "run_mode".to_string(),
                JsonValue::Number(self.request.run_mode.config_code().into()),
            ),
            (
                "active_file".to_string(),
                JsonValue::Object(self.current_file_object()),
            ),
        ]))
    }

    fn push_diagnostic(&mut self, message: String) {
        if !self.diagnostics.iter().any(|existing| existing == &message) {
            self.diagnostics.push(message);
        }
    }
}

#[cfg(feature = "js_runtime")]
impl TemplateSession<'_> {
    /// Hands session state to the JS runtime before it runs, so a runtime
    /// reused across includes never acts on a stale target or path set.
    fn sync_to_js_runtime(&self) {
        let Some(runtime) = self.js_runtime.as_ref() else {
            return;
        };
        if let Ok(mut state) = runtime.state.lock() {
            state.target_path.clone_from(&self.target_path);
            state.target_contents.clone_from(&self.target_contents);
            state.changed_paths.clone_from(&self.changed_paths);
            state.diagnostics.clone_from(&self.diagnostics);
        }
    }

    fn sync_from_js_runtime(&mut self) {
        let Some(runtime) = self.js_runtime.as_ref() else {
            return;
        };
        let Ok(mut state) = runtime.state.lock() else {
            return;
        };
        self.target_path.clone_from(&state.target_path);
        self.target_contents.clone_from(&state.target_contents);
        self.changed_paths.clone_from(&state.changed_paths);
        self.diagnostics.clone_from(&state.diagnostics);
        if let Some(frontmatter) = state.pending_frontmatter.take() {
            merge_pending_frontmatter_mapping(&mut self.pending_frontmatter, &frontmatter);
        }
    }

    fn take_js_input_required(&self) -> bool {
        self.js_runtime
            .as_ref()
            .and_then(|runtime| runtime.state.lock().ok())
            .is_some_and(|mut state| std::mem::take(&mut state.input_required))
    }

    fn js_runtime(&mut self) -> Result<&mut JsTemplateRuntime, CliError> {
        if self.js_runtime.is_none() {
            self.js_runtime = Some(JsTemplateRuntime::new(self)?);
        }
        self.js_runtime
            .as_mut()
            .ok_or_else(|| CliError::operation("failed to initialize templater runtime"))
    }

    /// Runs a template that needs JavaScript as one async program, the way
    /// Templater does, so control flow and declarations can span tags.
    fn render_js_program(
        &mut self,
        template: &str,
        segments: &[program::TemplateSegment<'_>],
        source_name: &str,
    ) -> Result<String, CliError> {
        let mut pieces = Vec::with_capacity(segments.len());
        for segment in segments {
            match segment {
                program::TemplateSegment::Text(text) => {
                    pieces.push(program::ProgramPiece::Text(self.render_native_text(text)?));
                }
                program::TemplateSegment::Tag { tag, line } => {
                    if tag.dynamic {
                        self.push_diagnostic(
                            "Templater dynamic tags (`<%+ %>`) are evaluated eagerly in CLI templates"
                                .to_string(),
                        );
                    }
                    if tag.execution {
                        pieces.push(program::ProgramPiece::Code {
                            body: tag.body,
                            line: *line,
                        });
                    } else {
                        let body = tag.body.trim_end_matches(';').trim_end();
                        if !body.is_empty() {
                            pieces.push(program::ProgramPiece::Expression { body, line: *line });
                        }
                    }
                }
            }
        }
        let compiled = program::compile_templater_program(&pieces);
        self.sync_to_js_runtime();
        let result = self
            .js_runtime()?
            .run_async(&compiled.source, program::TEMPLATE_PROGRAM_FILENAME);
        let input_required = self.take_js_input_required();
        self.sync_from_js_runtime();
        let failure_context = (source_name.to_string(), template.to_string(), compiled);
        let output = result.map_err(|mut failure| {
            failure.input_required = input_required;
            failure.into_error(
                &failure_context.0,
                &failure_context.1,
                Some(&failure_context.2),
            )
        });
        self.last_js_program = Some(failure_context);
        output
    }

    fn run_post_render_hooks(&mut self, rendered: String) -> Result<String, CliError> {
        let Some(runtime) = self.js_runtime.as_mut() else {
            return Ok(rendered);
        };
        let result = runtime.run_post_render_hooks(&rendered);
        let input_required = self.take_js_input_required();
        self.sync_from_js_runtime();
        result
            .map_err(|mut failure| {
                failure.input_required = input_required;
                failure
            })
            .map_err(|failure| match self.last_js_program.as_ref() {
                Some((source_name, template, compiled)) => {
                    failure.into_error(source_name, template, Some(compiled))
                }
                None => failure.into_error("template hooks", "", None),
            })
    }
}

/// Moment.js 2.30.1, which Obsidian exposes to templates as `moment`.
#[cfg(feature = "js_runtime")]
const MOMENT_JS: &str = include_str!("templates/vendor/moment.min.js");

#[cfg(feature = "js_runtime")]
const TEMPLATER_PRELUDE_FILENAME: &str = "templater-prelude.js";

#[cfg(feature = "js_runtime")]
const TEMPLATER_HOOKS_SOURCE: &str = "(async () => { const hooks = globalThis.__tpHooks ?? []; globalThis.__tpHooks = []; for (const hook of hooks) { if (typeof hook === 'function') { await hook(); } } return ''; })()";

/// A JavaScript failure while running a template, kept raw until the
/// session can point it at a template line.
#[cfg(feature = "js_runtime")]
#[derive(Debug, Clone, Default)]
struct JsFailure {
    name: String,
    message: String,
    stack: String,
    timeout: Option<std::time::Duration>,
    input_required: bool,
}

#[cfg(feature = "js_runtime")]
impl JsFailure {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    fn from_caught(error: &rquickjs::CaughtError<'_>) -> Self {
        match error {
            rquickjs::CaughtError::Exception(exception) => Self {
                name: exception
                    .as_object()
                    .get::<_, Option<String>>("name")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                message: exception.message().unwrap_or_default(),
                stack: exception.stack().unwrap_or_default(),
                ..Self::default()
            },
            rquickjs::CaughtError::Value(value) => Self::message(
                value
                    .as_string()
                    .and_then(|text| text.to_string().ok())
                    .unwrap_or_else(|| format!("uncaught non-Error value {value:?}")),
            ),
            rquickjs::CaughtError::Error(error) => Self::message(error.to_string()),
        }
    }

    fn summary(&self) -> String {
        let message = self.message.trim();
        // A plain `Error` adds nothing; specific names like `TypeError` do.
        let mut summary =
            if self.name.is_empty() || self.name == "Error" || message.starts_with(&self.name) {
                message.to_string()
            } else if message.is_empty() {
                self.name.clone()
            } else {
                format!("{}: {message}", self.name)
            };
        if let Some(frame) = program::first_external_frame(&self.stack) {
            summary = format!("{summary} ({frame})");
        }
        summary
    }

    fn into_error(
        self,
        source_name: &str,
        template: &str,
        compiled: Option<&program::CompiledProgram>,
    ) -> CliError {
        let line = compiled.and_then(|compiled| {
            program::program_line_from_stack(&self.stack)
                .and_then(|line| compiled.template_line(line))
        });
        let error = match self.timeout {
            Some(timeout) => CliError::operation_with_code(
                "template_script_timeout",
                format!(
                    "template script did not finish within {}s; raise [js_runtime] default_timeout_seconds if it legitimately needs longer",
                    timeout.as_secs()
                ),
            ),
            None if self.input_required => {
                CliError::operation_with_code(TEMPLATE_INPUT_REQUIRED_CODE, self.message.trim())
            }
            None => CliError::operation_with_code("template_script_error", self.summary()),
        };
        located_template_error(&error, source_name, line, template)
    }
}

#[cfg(feature = "js_runtime")]
type JsDeadline = Arc<Mutex<Option<std::time::Instant>>>;

#[cfg(feature = "js_runtime")]
struct JsTemplateRuntime {
    runtime: rquickjs::Runtime,
    context: rquickjs::Context,
    state: Arc<Mutex<JsTemplateState>>,
    deadline: JsDeadline,
    timeout: Option<std::time::Duration>,
}

#[cfg(feature = "js_runtime")]
impl JsTemplateRuntime {
    fn new(session: &mut TemplateSession<'_>) -> Result<Self, CliError> {
        use rquickjs::function::Func;
        use rquickjs::{CatchResultExt, Context, Ctx, Runtime};

        let runtime = Runtime::new().map_err(CliError::operation)?;
        let context = Context::full(&runtime).map_err(CliError::operation)?;
        let deadline: JsDeadline = Arc::new(Mutex::new(None));
        let timeout_seconds = session
            .request
            .vault_config
            .js_runtime
            .default_timeout_seconds;
        let timeout = (timeout_seconds > 0).then(|| {
            std::time::Duration::from_secs(u64::try_from(timeout_seconds).unwrap_or(u64::MAX))
        });
        runtime.set_interrupt_handler(Some(Box::new({
            let deadline = Arc::clone(&deadline);
            move || {
                deadline
                    .lock()
                    .ok()
                    .and_then(|deadline| *deadline)
                    .is_some_and(|deadline| std::time::Instant::now() >= deadline)
            }
        })));

        let mut initial_state = JsTemplateState::from_session(session);
        initial_state.deadline = Some(Arc::clone(&deadline));
        let state = Arc::new(Mutex::new(initial_state));
        context.with(|ctx| -> Result<(), CliError> {
            let globals = ctx.globals();
            globals
                .set(
                    "__tp_call_json",
                    Func::from({
                        let state = Arc::clone(&state);
                        move |ctx: Ctx<'_>,
                              name: String,
                              args_json: String|
                              -> rquickjs::Result<String> {
                            dispatch_js_call(&ctx, &Arc::clone(&state), &name, &args_json)
                        }
                    }),
                )
                .map_err(CliError::operation)?;
            ctx.eval_with_options::<(), _>(MOMENT_JS, js_eval_options("moment.min.js"))
                .catch(&ctx)
                .map_err(|error| {
                    CliError::operation(format!(
                        "failed to load moment.js: {}",
                        JsFailure::from_caught(&error).summary()
                    ))
                })?;
            ctx.eval_with_options::<(), _>(
                TEMPLATER_JS_PRELUDE,
                js_eval_options(TEMPLATER_PRELUDE_FILENAME),
            )
            .catch(&ctx)
            .map_err(|error| {
                CliError::operation(format!(
                    "failed to initialize the Templater runtime: {}",
                    JsFailure::from_caught(&error).summary()
                ))
            })?;
            load_user_scripts(&ctx, &state)?;
            Ok(())
        })?;

        Ok(Self {
            runtime,
            context,
            state,
            deadline,
            timeout,
        })
    }

    /// Evaluates `source`, which must produce a promise, and waits for it.
    fn run_async(&mut self, source: &str, filename: &str) -> Result<String, JsFailure> {
        if let Ok(mut deadline) = self.deadline.lock() {
            *deadline = self
                .timeout
                .map(|timeout| std::time::Instant::now() + timeout);
        }
        let result = self.run_async_unbounded(source, filename);
        let timed_out = self
            .deadline
            .lock()
            .ok()
            .and_then(|mut deadline| deadline.take())
            .is_some_and(|deadline| std::time::Instant::now() >= deadline);
        result.map_err(|mut failure| {
            if timed_out {
                failure.timeout = self.timeout;
            }
            failure
        })
    }

    fn run_async_unbounded(&mut self, source: &str, filename: &str) -> Result<String, JsFailure> {
        use rquickjs::CatchResultExt;

        self.context.with(|ctx| -> Result<(), JsFailure> {
            let promise: rquickjs::Value<'_> = ctx
                .eval_with_options(source, js_eval_options(filename))
                .catch(&ctx)
                .map_err(|error| JsFailure::from_caught(&error))?;
            let settle: rquickjs::Function<'_> = ctx
                .globals()
                .get("__vulcanTemplaterSettle")
                .map_err(|error| JsFailure::message(error.to_string()))?;
            settle
                .call::<_, ()>((promise,))
                .catch(&ctx)
                .map_err(|error| JsFailure::from_caught(&error))
        })?;
        while self.runtime.is_job_pending() {
            if let Err(error) = self.runtime.execute_pending_job() {
                return Err(error.0.with(|ctx| {
                    JsFailure::from_caught(&rquickjs::CaughtError::from_error(
                        &ctx,
                        rquickjs::Error::Exception,
                    ))
                }));
            }
        }
        let outcome = self
            .context
            .with(|ctx| {
                ctx.eval::<String, _>("JSON.stringify(globalThis.__vulcanTemplaterOutcome ?? null)")
            })
            .map_err(|error| JsFailure::message(error.to_string()))?;
        let outcome: JsonValue = serde_json::from_str(&outcome)
            .map_err(|error| JsFailure::message(error.to_string()))?;
        match outcome.get("ok").and_then(JsonValue::as_bool) {
            Some(true) => Ok(outcome
                .get("value")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string()),
            Some(false) => {
                let error = outcome.get("error");
                let field = |name: &str| {
                    error
                        .and_then(|error| error.get(name))
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                Err(JsFailure {
                    name: field("name"),
                    message: field("message"),
                    stack: field("stack"),
                    ..JsFailure::default()
                })
            }
            None => Err(JsFailure::message(
                "template script never finished: it awaits a promise that never settles",
            )),
        }
    }

    fn run_post_render_hooks(&mut self, rendered: &str) -> Result<String, JsFailure> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| JsFailure::message("templater lock poisoned"))?;
            state.target_contents = rendered.to_string();
            state.in_hooks = true;
        }
        let result = self.run_async(TEMPLATER_HOOKS_SOURCE, "templater-hooks.js");
        let mut state = self
            .state
            .lock()
            .map_err(|_| JsFailure::message("templater lock poisoned"))?;
        state.in_hooks = false;
        result.map(|_| state.target_contents.clone())
    }
}

#[cfg(feature = "js_runtime")]
fn js_eval_options(filename: &str) -> rquickjs::context::EvalOptions {
    let mut options = rquickjs::context::EvalOptions::default();
    options.global = true;
    options.strict = false;
    options.backtrace_barrier = true;
    options.filename = Some(filename.to_string());
    options
}

#[cfg(feature = "js_runtime")]
#[derive(Debug, Clone)]
struct JsTemplateState {
    paths: VaultPaths,
    vault_config: VaultConfig,
    templates: Vec<TemplateCandidate>,
    template_path: Option<PathBuf>,
    target_path: String,
    target_contents: String,
    vars: HashMap<String, String>,
    allow_mutations: bool,
    run_mode_code: i64,
    reference_date: Option<String>,
    include_depth: usize,
    changed_paths: BTreeSet<String>,
    diagnostics: Vec<String>,
    pending_frontmatter: Option<YamlMapping>,
    read_filter: Option<PermissionFilter>,
    mutation_guard: Option<ProfilePermissionGuard>,
    staged_creates: Option<StagedTemplateCreates>,
    original_target_contents: Option<String>,
    deadline: Option<JsDeadline>,
    prompt_count: usize,
    suggester_count: usize,
    /// Set when a prompt had no answer, so the failure keeps its own code.
    input_required: bool,
    /// True while `on_all_templates_executed` hooks run.
    in_hooks: bool,
}

#[cfg(feature = "js_runtime")]
impl JsTemplateState {
    fn from_session(session: &TemplateSession<'_>) -> Self {
        Self {
            paths: session.request.paths.clone(),
            vault_config: session.request.vault_config.clone(),
            templates: session.request.templates.to_vec(),
            template_path: session.request.template_path.map(Path::to_path_buf),
            target_path: session.target_path.clone(),
            target_contents: session.target_contents.clone(),
            vars: session.request.vars.clone(),
            allow_mutations: session.request.allow_mutations,
            run_mode_code: session.request.run_mode.config_code(),
            reference_date: session.request.reference_date.map(str::to_string),
            include_depth: session.include_depth,
            changed_paths: session.changed_paths.clone(),
            diagnostics: session.diagnostics.clone(),
            pending_frontmatter: None,
            read_filter: session.read_filter.clone(),
            mutation_guard: session.mutation_guard.clone(),
            staged_creates: session.staged_creates.clone(),
            original_target_contents: session.request.target_contents.map(str::to_string),
            deadline: None,
            prompt_count: session.prompt_count,
            suggester_count: session.suggester_count,
            input_required: false,
            in_hooks: false,
        }
    }

    fn push_diagnostic(&mut self, message: String) {
        if !self.diagnostics.iter().any(|existing| existing == &message) {
            self.diagnostics.push(message);
        }
    }

    /// Renders another template (an include or a `create_new` source) with
    /// this render's inputs and authority, one include level deeper.
    fn render_nested(
        &self,
        template_path: Option<&Path>,
        template_text: &str,
        target_path: &str,
        target_contents: Option<&str>,
    ) -> Result<TemplateRenderOutput, String> {
        render_template_request_at_depth(
            TemplateRenderRequest {
                paths: &self.paths,
                vault_config: &self.vault_config,
                templates: &self.templates,
                template_path,
                template_text,
                target_path,
                target_contents,
                engine: TemplateEngineKind::Templater,
                vars: &self.vars,
                allow_mutations: self.allow_mutations,
                run_mode: template_run_mode_from_code(self.run_mode_code),
                reference_date: self.reference_date.as_deref(),
            },
            self.read_filter.as_ref(),
            self.mutation_guard.as_ref(),
            self.staged_creates.clone(),
            self.include_depth + 1,
        )
        .map_err(|error| error.to_string())
    }

    /// Runs an interactive prompt without letting the time a person spends
    /// answering count against the script timeout.
    fn while_waiting_for_user<T>(&self, ask: impl FnOnce() -> T) -> T {
        let started = std::time::Instant::now();
        let result = ask();
        if let Some(deadline) = self.deadline.as_ref() {
            if let Ok(mut deadline) = deadline.lock() {
                if let Some(at) = deadline.as_mut() {
                    *at += started.elapsed();
                }
            }
        }
        result
    }

    fn prompt_result<T>(&mut self, result: Result<T, prompt::PromptError>) -> Result<T, String> {
        result.map_err(|error| {
            if matches!(error, prompt::PromptError::InputRequired(_)) {
                self.input_required = true;
            }
            error.message().to_string()
        })
    }

    fn absorb_nested(&mut self, nested: &TemplateRenderOutput) {
        self.changed_paths
            .extend(nested.changed_paths.iter().cloned());
        for diagnostic in &nested.diagnostics {
            self.push_diagnostic(diagnostic.clone());
        }
    }
}

#[cfg(feature = "js_runtime")]
const TEMPLATER_JS_PRELUDE: &str = r#"
function __tpCall(name, ...args) {
  const response = JSON.parse(__tp_call_json(name, JSON.stringify(args)));
  if (response.error) {
    throw new Error(response.error);
  }
  return response.value;
}
function __tpPathOf(value) {
  if (value && typeof value === "object" && typeof value.path === "string") return value.path;
  return String(value);
}
function __tpItemKey(item) {
  if (item && typeof item === "object") {
    if (typeof item.path === "string") return item.path;
    try { return JSON.stringify(item); } catch (_) { return String(item); }
  }
  return item == null ? "" : String(item);
}
function __tpSuggestion(textItems, items) {
  const list = Array.from(items ?? []);
  const labels = typeof textItems === "function"
    ? list.map((item, index) => String(textItems(item, index)))
    : Array.from(textItems ?? []).map((label) => String(label));
  if (labels.length !== list.length) {
    throw new Error(`text_items has ${labels.length} entries but items has ${list.length}`);
  }
  return [labels, list];
}
function __vulcanTemplaterStringify(value) {
  if (value === undefined || value === null) return "";
  if (Array.isArray(value)) return value.map(__vulcanTemplaterStringify).join(",");
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}
function __vulcanTemplaterDescribeError(error) {
  if (error instanceof Error) {
    return { name: String(error.name ?? ""), message: String(error.message ?? ""), stack: String(error.stack ?? "") };
  }
  let message;
  try { message = typeof error === "string" ? error : JSON.stringify(error); } catch (_) { message = String(error); }
  return { name: "", message: `uncaught value: ${message}`, stack: "" };
}
function __vulcanTemplaterSettle(promise) {
  globalThis.__vulcanTemplaterOutcome = undefined;
  Promise.resolve(promise).then(
    (value) => { globalThis.__vulcanTemplaterOutcome = { ok: true, value: value == null ? "" : String(value) }; },
    (error) => { globalThis.__vulcanTemplaterOutcome = { ok: false, error: __vulcanTemplaterDescribeError(error) }; },
  );
}
function require(name) {
  throw new Error(`require(${JSON.stringify(String(name))}) is not available in Vulcan templates; Node and Obsidian modules cannot be loaded`);
}
const tp = {
  date: {
    now: (...args) => __tpCall("date.now", ...args),
    tomorrow: (...args) => __tpCall("date.tomorrow", ...args),
    yesterday: (...args) => __tpCall("date.yesterday", ...args),
    weekday: (...args) => __tpCall("date.weekday", ...args),
  },
  file: {
    get title() { return __tpCall("file.title"); },
    path: (relative = false) => __tpCall("file.path", Boolean(relative)),
    folder: (absolute = false) => __tpCall("file.folder", Boolean(absolute)),
    creation_date: (format) => __tpCall("file.creation_date", format ?? null),
    last_modified_date: (format) => __tpCall("file.last_modified_date", format ?? null),
    get content() { return __tpCall("file.content"); },
    get tags() { return __tpCall("file.tags"); },
    exists: (path) => __tpCall("file.exists", __tpPathOf(path)),
    include: (link) => __tpCall("file.include", __tpPathOf(link)),
    create_new: (template, filename, openNew = false, folder = "") => __tpCall(
      "file.create_new",
      template && typeof template === "object" ? { template: __tpPathOf(template) } : String(template ?? ""),
      filename ?? "",
      Boolean(openNew),
      folder == null ? "" : __tpPathOf(folder),
    ),
    move: (path) => __tpCall("file.move", String(path)),
    rename: (name) => __tpCall("file.rename", String(name)),
    cursor: (order = null) => __tpCall("file.cursor", order),
    cursor_append: (content) => {
      __tpCall("diagnostic", "tp.file.cursor_append() has no editor cursor in Vulcan; its content was not inserted");
      return "";
    },
    selection: () => "",
    find_tfile: (path) => __tpCall("file.find_tfile", String(path)),
  },
  get frontmatter() { return __tpCall("frontmatter"); },
  system: {
    prompt: async (promptText = "", defaultValue = null, throwOnCancel = false, multiLine = false) =>
      __tpCall("system.prompt", String(promptText ?? ""), defaultValue == null ? null : String(defaultValue), Boolean(throwOnCancel), Boolean(multiLine)),
    suggester: async (textItems, items, throwOnCancel = false, placeholder = "", limit = undefined) => {
      const [labels, list] = __tpSuggestion(textItems, items);
      const index = __tpCall("system.suggester", labels, list.map(__tpItemKey), Boolean(throwOnCancel), String(placeholder ?? ""), limit ?? null);
      return index == null ? null : list[index];
    },
    multi_suggester: async (textItems, items, throwOnCancel = false, title = "", limit = undefined) => {
      const [labels, list] = __tpSuggestion(textItems, items);
      const indices = __tpCall("system.multi_suggester", labels, list.map(__tpItemKey), Boolean(throwOnCancel), String(title ?? ""), limit ?? null);
      return indices == null ? null : indices.map((index) => list[index]);
    },
    clipboard: async () => __tpCall("system.clipboard"),
  },
  web: {
    request: (...args) => __tpCall("web.request", ...args),
    daily_quote: () => __tpCall("web.daily_quote"),
    random_picture: (...args) => __tpCall("web.random_picture", ...args),
  },
  user: {},
  hooks: {
    on_all_templates_executed: (callback) => {
      globalThis.__tpHooks = globalThis.__tpHooks ?? [];
      globalThis.__tpHooks.push(callback);
    },
  },
  get config() { return __tpCall("config"); },
  obsidian: {
    moment: globalThis.moment,
    normalizePath: (path) => __tpCall("obsidian.normalizePath", String(path)),
    htmlToMarkdown: (html) => __tpCall("obsidian.htmlToMarkdown", String(html)),
    parseYaml: (text) => __tpCall("obsidian.parseYaml", String(text)),
    stringifyYaml: (value) => __tpCall("obsidian.stringifyYaml", value),
    request: async (request) => __tpCall("obsidian.requestUrl", __tpRequestUrl(request)),
    requestUrl: async (request) => {
      const text = __tpCall("obsidian.requestUrl", __tpRequestUrl(request));
      return { status: 200, headers: {}, text, get json() { return JSON.parse(text); } };
    },
    Notice: class Notice {
      constructor(message) { __tpCall("diagnostic", `Notice: ${String(message)}`); }
      setMessage(message) { __tpCall("diagnostic", `Notice: ${String(message)}`); return this; }
      hide() {}
    },
  },
};
function __tpRequestUrl(request) {
  if (request && typeof request === "object") {
    if (request.method && String(request.method).toUpperCase() !== "GET") {
      throw new Error(`requestUrl() only supports GET requests in Vulcan templates, not ${request.method}`);
    }
    return String(request.url);
  }
  return String(request);
}
async function __tpProcessFrontMatter(file, update) {
  const path = __tpPathOf(file);
  const frontmatter = __tpCall("app.frontmatter", path);
  await update(frontmatter);
  __tpCall("app.set_frontmatter", path, frontmatter);
}
const __tpAppApi = {
  vault: {
    getName: () => __tpCall("app.vault_name"),
    getMarkdownFiles: () => __tpCall("app.files"),
    getFiles: () => __tpCall("app.files"),
    getAbstractFileByPath: (path) => __tpCall("app.file", String(path)),
    getFileByPath: (path) => __tpCall("app.file", String(path)),
    read: async (file) => __tpCall("app.read", __tpPathOf(file)),
    cachedRead: async (file) => __tpCall("app.read", __tpPathOf(file)),
    create: async (path, data) => __tpCall("app.create", String(path), String(data ?? "")),
    modify: async (file, data) => { __tpCall("app.modify", __tpPathOf(file), String(data ?? "")); },
    process: async (file, update) => {
      const path = __tpPathOf(file);
      const data = String(update(__tpCall("app.read", path)) ?? "");
      __tpCall("app.modify", path, data);
      return data;
    },
    adapter: {
      exists: async (path) => __tpCall("app.exists", String(path)),
      read: async (path) => __tpCall("app.read", String(path)),
    },
  },
  metadataCache: {
    getFileCache: (file) => __tpCall("app.cache", __tpPathOf(file)),
    getCache: (path) => __tpCall("app.cache", String(path)),
    getFirstLinkpathDest: (linkpath) => __tpCall("app.file", String(linkpath)),
  },
  workspace: {
    getActiveFile: () => __tpCall("app.active"),
  },
  fileManager: {
    processFrontMatter: __tpProcessFrontMatter,
    generateMarkdownLink: (file) => `[[${file.basename ?? __tpPathOf(file)}]]`,
  },
};
// Unsupported parts of the Obsidian API fail loudly with their full name
// instead of surfacing later as "cannot read property of undefined".
function __tpGuard(target, path) {
  return new Proxy(target, {
    get(object, key) {
      if (typeof key === "symbol" || key === "then" || key === "toJSON") return object[key];
      if (!(key in object)) {
        throw new Error(`${path}.${String(key)} is not available in Vulcan templates`);
      }
      const value = object[key];
      return value && typeof value === "object" && !Array.isArray(value)
        ? __tpGuard(value, `${path}.${String(key)}`)
        : value;
    },
  });
}
tp.app = __tpGuard(__tpAppApi, "app");
globalThis.app = tp.app;
globalThis.window = globalThis;
globalThis.tp = tp;
"#;

#[cfg(feature = "js_runtime")]
fn load_user_scripts(
    ctx: &rquickjs::Ctx<'_>,
    state: &Arc<Mutex<JsTemplateState>>,
) -> Result<(), CliError> {
    use rquickjs::CatchResultExt;

    let (vault_root, user_scripts_folder, templates_pairs, enable_system_commands) = {
        let state = state
            .lock()
            .map_err(|_| CliError::operation("templater lock poisoned"))?;
        (
            state.paths.vault_root().to_path_buf(),
            state.vault_config.templates.user_scripts_folder.clone(),
            state.vault_config.templates.templates_pairs.clone(),
            state.vault_config.templates.enable_system_commands,
        )
    };
    let globals = ctx.globals();
    let tp: rquickjs::Object<'_> = globals.get("tp").map_err(CliError::operation)?;
    let user: rquickjs::Object<'_> = tp.get("user").map_err(CliError::operation)?;

    if let Some(folder) = user_scripts_folder {
        let absolute = vault_root.join(&folder);
        if absolute.is_dir() {
            let mut scripts = fs::read_dir(&absolute)
                .map_err(CliError::operation)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(CliError::operation)?;
            scripts.sort();
            for path in scripts {
                if path.extension().and_then(|value| value.to_str()) != Some("js") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                let display = path
                    .strip_prefix(&vault_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let script = fs::read_to_string(&path).map_err(CliError::operation)?;
                // The prefix stays on the script's first line so stack
                // frames report the script's own line numbers.
                let wrapper = format!(
                    "(function(){{ const module = {{ exports: {{}} }}; const exports = module.exports; {script}\n; return module.exports; }})()"
                );
                let exported: rquickjs::Value<'_> = ctx
                    .eval_with_options(wrapper, js_eval_options(&display))
                    .catch(ctx)
                    .map_err(|error| {
                        let failure = JsFailure::from_caught(&error);
                        let line = failure
                            .stack
                            .split(&format!("{display}:"))
                            .nth(1)
                            .and_then(|rest| {
                                rest.split(|c: char| !c.is_ascii_digit()).next()?.parse::<usize>().ok()
                            });
                        CliError::operation_with_code(
                            "template_script_error",
                            match line {
                                Some(line) => format!(
                                    "failed to load Templater user script `{display}` (line {line}): {}",
                                    failure.summary()
                                ),
                                None => format!(
                                    "failed to load Templater user script `{display}`: {}",
                                    failure.summary()
                                ),
                            },
                        )
                    })?;
                user.set(stem, exported).map_err(CliError::operation)?;
            }
        }
    }

    if enable_system_commands {
        for pair in templates_pairs {
            let function = ctx
                .eval::<rquickjs::Function<'_>, _>(format!(
                    "(function(...args) {{ return __tpCall({:?}, ...args); }})",
                    pair.name
                ))
                .map_err(CliError::operation)?;
            user.set(pair.name, function).map_err(CliError::operation)?;
        }
    }
    Ok(())
}

#[cfg(feature = "js_runtime")]
fn dispatch_js_call(
    ctx: &rquickjs::Ctx<'_>,
    state: &Arc<Mutex<JsTemplateState>>,
    name: &str,
    args_json: &str,
) -> rquickjs::Result<String> {
    let args: Vec<JsonValue> = serde_json::from_str(args_json)
        .map_err(|error| rquickjs::Exception::throw_message(ctx, &error.to_string()))?;
    let response = js_call_response(state, name, &args).map_err(|error| {
        let message = if error.starts_with("tp.") || error.starts_with("--var ") {
            error
        } else {
            format!("{}: {error}", js_call_display_name(name))
        };
        rquickjs::Exception::throw_message(ctx, &message)
    })?;
    serde_json::to_string(&response)
        .map_err(|error| rquickjs::Exception::throw_message(ctx, &error.to_string()))
}

#[cfg(feature = "js_runtime")]
fn js_call_display_name(name: &str) -> String {
    match name {
        "frontmatter" | "config" | "file.title" | "file.content" | "file.tags" => {
            format!("tp.{name}")
        }
        name if name.contains('.') => format!("tp.{name}()"),
        name => format!("tp.user.{name}()"),
    }
}

#[cfg(feature = "js_runtime")]
#[allow(clippy::too_many_lines)]
fn js_call_response(
    state: &std::sync::Arc<std::sync::Mutex<JsTemplateState>>,
    name: &str,
    args: &[JsonValue],
) -> Result<JsonValue, String> {
    let mut state = state
        .lock()
        .map_err(|_| "templater lock poisoned".to_string())?;
    let value = match name {
        "diagnostic" => {
            if let Some(message) = args.first().and_then(JsonValue::as_str) {
                state.push_diagnostic(message.to_string());
            }
            JsonValue::Null
        }
        "date.now" => JsonValue::String(js_date_now(&state, args)?),
        "date.tomorrow" => JsonValue::String(js_date_fixed_offset(args, 1)),
        "date.yesterday" => JsonValue::String(js_date_fixed_offset(args, -1)),
        "date.weekday" => JsonValue::String(js_date_weekday(&state, args)?),
        "file.title" => JsonValue::String(
            Path::new(&state.target_path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Untitled")
                .to_string(),
        ),
        "file.path" => JsonValue::String(templater_file_path(
            &state.paths,
            &state.target_path,
            args.first().and_then(JsonValue::as_bool).unwrap_or(false),
        )),
        "file.folder" => JsonValue::String(templater_file_folder(
            &state.target_path,
            args.first().and_then(JsonValue::as_bool).unwrap_or(false),
        )),
        "file.creation_date" => JsonValue::String(js_file_timestamp(&state, args, true)),
        "file.last_modified_date" => JsonValue::String(js_file_timestamp(&state, args, false)),
        "file.content" => JsonValue::String(state.target_contents.clone()),
        "file.tags" => JsonValue::Array(
            parse_document(&state.target_contents, &state.vault_config)
                .tags
                .iter()
                .map(|tag| JsonValue::String(tag.tag_text.clone()))
                .collect(),
        ),
        "file.exists" => JsonValue::Bool(
            args.first()
                .and_then(JsonValue::as_str)
                .and_then(|path| resolve_vault_path_from_state(&state, path))
                .is_some(),
        ),
        "file.include" => JsonValue::String(js_file_include(&mut state, args)?),
        "file.create_new" => js_file_create_new(&mut state, args)?,
        "file.move" => {
            js_file_move(&mut state, args)?;
            JsonValue::String(String::new())
        }
        "file.rename" => {
            js_file_rename(&mut state, args)?;
            JsonValue::String(String::new())
        }
        "file.cursor" => JsonValue::String(
            state
                .vars
                .get(TEMPLATE_CURSOR_MARKER_VAR)
                .cloned()
                .unwrap_or_default(),
        ),
        "file.find_tfile" => args
            .first()
            .and_then(JsonValue::as_str)
            .and_then(|path| resolve_vault_path_from_state(&state, path))
            .as_deref()
            .map(file_object_json)
            .map_or(JsonValue::Null, JsonValue::Object),
        "frontmatter" => JsonValue::Object(yaml_mapping_to_json_object(
            &current_frontmatter_mapping(&state.target_contents).unwrap_or_default(),
        )),
        "config" => JsonValue::Object(JsonMap::from_iter([
            (
                "template_file".to_string(),
                state
                    .template_path
                    .as_deref()
                    .map(|path| path_to_relative_file_json(&state.paths, path))
                    .map_or(JsonValue::Null, JsonValue::Object),
            ),
            (
                "target_file".to_string(),
                JsonValue::Object(file_object_json(&state.target_path)),
            ),
            (
                "run_mode".to_string(),
                JsonValue::Number(state.run_mode_code.into()),
            ),
            (
                "active_file".to_string(),
                JsonValue::Object(file_object_json(&state.target_path)),
            ),
        ])),
        "system.prompt" => js_system_prompt(&mut state, args)?,
        "system.suggester" => js_system_suggester(&mut state, args, false)?,
        "system.multi_suggester" => js_system_suggester(&mut state, args, true)?,
        "system.clipboard" => JsonValue::String(read_clipboard_best_effort()),
        "web.request" => {
            // Templater returns the parsed JSON response (or the value at
            // `path`); plain-text responses come back as strings.
            let url = args.first().and_then(JsonValue::as_str).unwrap_or_default();
            let body = js_web_request_string(&state, url, None)?;
            match args.get(1).and_then(JsonValue::as_str) {
                Some(path) if !path.trim().is_empty() => json_path_value(
                    serde_json::from_str(&body).map_err(|error| error.to_string())?,
                    path,
                )?,
                _ => serde_json::from_str(&body).unwrap_or(JsonValue::String(body)),
            }
        }
        "obsidian.requestUrl" => {
            let url = args.first().and_then(JsonValue::as_str).unwrap_or_default();
            JsonValue::String(js_web_request_string(&state, url, None)?)
        }
        "obsidian.parseYaml" => {
            let text = args.first().and_then(JsonValue::as_str).unwrap_or_default();
            let value: YamlValue = serde_yaml::from_str(text).map_err(|error| error.to_string())?;
            yaml_to_json(&value)
        }
        "obsidian.stringifyYaml" => JsonValue::String(
            serde_yaml::to_string(args.first().unwrap_or(&JsonValue::Null))
                .map_err(|error| error.to_string())?,
        ),
        name if name.starts_with("app.") => js_app_call(&mut state, name, args)?,
        "web.daily_quote" => JsonValue::String(js_daily_quote(&state)?),
        "web.random_picture" => JsonValue::String(random_picture_markdown(
            args.first().and_then(JsonValue::as_str),
            args.get(1).and_then(JsonValue::as_str),
            args.get(2).and_then(JsonValue::as_bool).unwrap_or(false),
        )),
        "obsidian.normalizePath" => JsonValue::String(normalize_note_output_path(
            args.first().and_then(JsonValue::as_str).unwrap_or_default(),
        )?),
        "obsidian.htmlToMarkdown" => JsonValue::String(html_to_markdown(
            args.first().and_then(JsonValue::as_str).unwrap_or_default(),
        )),
        other => js_system_user_command(&state, other, args)?,
    };
    Ok(JsonValue::Object(JsonMap::from_iter([
        ("value".to_string(), value),
        ("error".to_string(), JsonValue::Null),
    ])))
}

#[cfg(feature = "js_runtime")]
fn js_reference_timestamp(
    _state: &JsTemplateState,
    reference: Option<&str>,
    reference_format: Option<&str>,
) -> Result<i64, String> {
    if let Some(reference) = reference {
        if let Some(format) = reference_format {
            return parse_date_with_format(reference, format)
                .ok_or_else(|| format!("failed to parse reference date `{reference}`"));
        }
        return parse_date_like_string(reference)
            .ok_or_else(|| format!("failed to parse reference date `{reference}`"));
    }
    Ok(current_timestamp_millis())
}

#[cfg(feature = "js_runtime")]
fn js_apply_offset(millis: i64, offset: Option<&JsonValue>) -> Result<i64, String> {
    match offset {
        None | Some(JsonValue::Null) => Ok(millis),
        Some(JsonValue::Number(value)) => {
            let days = value.as_f64().unwrap_or_default();
            #[allow(clippy::cast_possible_truncation)]
            {
                Ok(millis + (days * 86_400_000.0) as i64)
            }
        }
        Some(JsonValue::String(text)) => apply_offset_string(millis, text),
        Some(_) => Err("invalid templater date offset".to_string()),
    }
}

#[cfg(feature = "js_runtime")]
fn js_web_request_string(
    state: &JsTemplateState,
    url: &str,
    json_path: Option<&str>,
) -> Result<String, String> {
    ensure_allowlisted_url(&state.vault_config.templates, url)?;
    let response = http_get_text(url)?;
    extract_web_response(response, json_path)
}

#[cfg(feature = "js_runtime")]
fn js_date_now(state: &JsTemplateState, args: &[JsonValue]) -> Result<String, String> {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or("YYYY-MM-DD");
    let millis = js_reference_timestamp(
        state,
        args.get(2).and_then(JsonValue::as_str),
        args.get(3).and_then(JsonValue::as_str),
    )?;
    let millis = js_apply_offset(millis, args.get(1))?;
    Ok(format_date(millis, format))
}

#[cfg(feature = "js_runtime")]
fn js_date_fixed_offset(args: &[JsonValue], day_offset: i64) -> String {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or("YYYY-MM-DD");
    format_date(current_timestamp_millis() + day_offset * 86_400_000, format)
}

#[cfg(feature = "js_runtime")]
fn js_date_weekday(state: &JsTemplateState, args: &[JsonValue]) -> Result<String, String> {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or("YYYY-MM-DD");
    let weekday = args.get(1).and_then(JsonValue::as_i64).unwrap_or_default();
    let millis = js_reference_timestamp(
        state,
        args.get(2).and_then(JsonValue::as_str),
        args.get(3).and_then(JsonValue::as_str),
    )?;
    Ok(format_date(weekday_timestamp(millis, weekday), format))
}

/// `tp.file.path(relative)`: the absolute filesystem path by default, the
/// vault-relative path when `relative` is true (Templater semantics).
fn templater_file_path(paths: &VaultPaths, target_path: &str, relative: bool) -> String {
    if relative {
        target_path.to_string()
    } else {
        paths.vault_root().join(target_path).display().to_string()
    }
}

/// `tp.file.folder(absolute)`: the folder's own name by default, its
/// vault-relative path when `absolute` is true (Templater semantics).
fn templater_file_folder(target_path: &str, absolute: bool) -> String {
    let folder = Path::new(target_path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    if absolute {
        folder
    } else {
        folder.rsplit('/').next().unwrap_or_default().to_string()
    }
}

#[cfg(feature = "js_runtime")]
fn js_file_timestamp(state: &JsTemplateState, args: &[JsonValue], creation: bool) -> String {
    let format = args
        .first()
        .and_then(JsonValue::as_str)
        .unwrap_or(DEFAULT_FILE_DATE_FORMAT);
    let millis =
        file_timestamp_millis(&state.paths.vault_root().join(&state.target_path), creation)
            .unwrap_or_else(current_timestamp_millis);
    format_date(millis, format)
}

#[cfg(feature = "js_runtime")]
fn js_file_include(state: &mut JsTemplateState, args: &[JsonValue]) -> Result<String, String> {
    if state.include_depth >= MAX_TEMPLATE_INCLUDE_DEPTH {
        return Err(format!(
            "Reached inclusion depth limit (max = {MAX_TEMPLATE_INCLUDE_DEPTH})"
        ));
    }
    let path = args.first().and_then(JsonValue::as_str).unwrap_or_default();
    let target = resolve_vault_path_from_state(state, path)
        .ok_or_else(|| format!("File {path} doesn't exist"))?;
    let source = template_path_content(&state.paths, state.staged_creates.as_ref(), &target)?;
    let source = match split_include_link(path).1 {
        Some(subpath) => extract_include_subpath(&source, subpath, &state.vault_config)?,
        None => source,
    };
    let template_path = state.paths.vault_root().join(&target);
    let target_path = state.target_path.clone();
    let target_contents = state.target_contents.clone();
    let rendered = state.render_nested(
        Some(&template_path),
        &source,
        &target_path,
        Some(&target_contents),
    )?;
    state.absorb_nested(&rendered);
    let (frontmatter, body) =
        parse_frontmatter_document(&rendered.content, true).map_err(|error| error.to_string())?;
    if let Some(frontmatter) = frontmatter {
        merge_pending_frontmatter_mapping(&mut state.pending_frontmatter, &frontmatter);
    }
    Ok(body)
}

fn normalize_template_include_path(path: &str) -> Option<String> {
    normalize_relative_input_path(
        path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .ok()
}

/// Content for `tp.file.create_new()`, following Templater: a `TFile` renders
/// that template file for the new note, and a string is itself template
/// content rendered for the new note.
#[cfg(feature = "js_runtime")]
fn js_create_new_content(
    state: &mut JsTemplateState,
    template: Option<&JsonValue>,
    new_path: &str,
) -> Result<String, String> {
    let (template_path, source) = match template {
        Some(JsonValue::Object(object)) => {
            let identifier = object
                .get("template")
                .and_then(JsonValue::as_str)
                .unwrap_or_default();
            let target = resolve_vault_path_from_state(state, identifier)
                .ok_or_else(|| format!("template {identifier} doesn't exist"))?;
            (
                Some(state.paths.vault_root().join(&target)),
                template_path_content(&state.paths, state.staged_creates.as_ref(), &target)?,
            )
        }
        Some(JsonValue::String(text)) => (None, text.clone()),
        Some(other) => (None, json_value_to_js_string(other)),
        None => return Ok(String::new()),
    };
    let rendered = state.render_nested(template_path.as_deref(), &source, new_path, None)?;
    state.absorb_nested(&rendered);
    Ok(rendered.content)
}

#[cfg(feature = "js_runtime")]
fn js_file_create_new(
    state: &mut JsTemplateState,
    args: &[JsonValue],
) -> Result<JsonValue, String> {
    if !state.allow_mutations {
        state.push_diagnostic(
            "tp.file.create_new() is disabled during template preview".to_string(),
        );
        return Ok(JsonValue::Null);
    }
    let filename = args
        .get(1)
        .and_then(JsonValue::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Untitled");
    let folder = args.get(3).and_then(JsonValue::as_str).unwrap_or_default();
    let path = if folder.trim().is_empty() {
        format!("{filename}.md")
    } else {
        format!("{}/{}.md", folder.trim_matches('/'), filename)
    };
    let normalized = normalize_note_output_path(&path)?;
    let content = js_create_new_content(state, args.first(), &normalized)?;
    js_write_new_note(state, &normalized, content)
}

/// Writes a new note as a template side effect, staged with the render
/// when a batch is open, and under the active permission profile.
#[cfg(feature = "js_runtime")]
fn js_write_new_note(
    state: &mut JsTemplateState,
    normalized: &str,
    content: String,
) -> Result<JsonValue, String> {
    let normalized = normalized.to_string();
    if let Some(guard) = state.mutation_guard.as_ref() {
        guard
            .check_write_path(&normalized)
            .map_err(|error| error.to_string())?;
    }
    if let Some(staged) = state.staged_creates.as_ref() {
        stage_template_create(staged, &state.paths, &normalized, content)?;
    } else {
        write_ordinary_note_if_unchanged_with_profile(
            &state.paths,
            &normalized,
            None,
            &content,
            "template file create",
            state
                .mutation_guard
                .as_ref()
                .map(|guard| guard.selection().name.as_str()),
        )
        .map_err(|error| error.to_string())?;
    }
    state.changed_paths.insert(normalized.clone());
    Ok(JsonValue::Object(file_object_json(&normalized)))
}

#[cfg(feature = "js_runtime")]
fn js_file_move(state: &mut JsTemplateState, args: &[JsonValue]) -> Result<(), String> {
    if !state.allow_mutations {
        state.push_diagnostic("tp.file.move() is disabled during template preview".to_string());
        return Ok(());
    }
    let new_path = args.first().and_then(JsonValue::as_str).unwrap_or_default();
    let normalized = normalize_note_output_path(new_path)?;
    if let Some(staged) = state.staged_creates.as_ref() {
        if let Some(summary) = stage_template_move(
            staged,
            &state.paths,
            &state.target_path,
            &normalized,
            state.original_target_contents.as_deref(),
            state.mutation_guard.as_ref(),
        )? {
            state.changed_paths.insert(summary.destination_path);
            state
                .changed_paths
                .extend(summary.rewritten_files.into_iter().map(|file| file.path));
        }
    } else if state.paths.vault_root().join(&state.target_path).is_file() {
        let summary = guarded_template_move(
            &state.paths,
            &state.target_path,
            &normalized,
            state.mutation_guard.as_ref(),
        )?;
        state.changed_paths.insert(summary.destination_path.clone());
        for rewritten in summary.rewritten_files {
            state.changed_paths.insert(rewritten.path);
        }
    } else if let Some(guard) = state.mutation_guard.as_ref() {
        guard
            .check_write_path(&normalized)
            .map_err(|error| error.to_string())?;
    }
    state.target_path = normalized;
    Ok(())
}

#[cfg(feature = "js_runtime")]
fn js_file_rename(state: &mut JsTemplateState, args: &[JsonValue]) -> Result<(), String> {
    let name = args.first().and_then(JsonValue::as_str).unwrap_or_default();
    if name.contains(['\\', '/', ':']) {
        return Err("File name cannot contain any of these characters: \\ / :".to_string());
    }
    let folder = Path::new(&state.target_path)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let target = if folder.is_empty() {
        format!("{name}.md")
    } else {
        format!("{folder}/{name}.md")
    };
    js_file_move(state, &[JsonValue::String(target)])
}

#[cfg(feature = "js_runtime")]
fn js_system_prompt(state: &mut JsTemplateState, args: &[JsonValue]) -> Result<JsonValue, String> {
    state.prompt_count += 1;
    let message = args
        .first()
        .map(json_value_to_js_string)
        .unwrap_or_default();
    let default = args
        .get(1)
        .filter(|value| !value.is_null())
        .map(json_value_to_js_string);
    let call = prompt::PromptCall {
        message: &message,
        default: default.as_deref(),
        throw_on_cancel: args.get(2).and_then(JsonValue::as_bool).unwrap_or(false),
        multiline: args.get(3).and_then(JsonValue::as_bool).unwrap_or(false),
        ordinal: state.prompt_count,
    };
    let vars = state.vars.clone();
    let answer = state.while_waiting_for_user(|| prompt::resolve_prompt(&vars, &call));
    state
        .prompt_result(answer)
        .map(|answer| answer.map_or(JsonValue::Null, JsonValue::String))
}

/// Resolves a suggester to the chosen index (or indices for
/// `multi_suggester`); the prelude maps them back onto the original items.
#[cfg(feature = "js_runtime")]
fn js_system_suggester(
    state: &mut JsTemplateState,
    args: &[JsonValue],
    multiple: bool,
) -> Result<JsonValue, String> {
    state.suggester_count += 1;
    let strings = |index: usize| -> Vec<String> {
        args.get(index)
            .and_then(JsonValue::as_array)
            .map(|values| values.iter().map(json_value_to_js_string).collect())
            .unwrap_or_default()
    };
    let options = strings(0);
    let item_keys = strings(1);
    let placeholder = args.get(3).map(json_value_to_js_string).unwrap_or_default();
    let call = prompt::SuggestCall {
        options: &options,
        item_keys: &item_keys,
        throw_on_cancel: args.get(2).and_then(JsonValue::as_bool).unwrap_or(false),
        placeholder: &placeholder,
        limit: args
            .get(4)
            .and_then(JsonValue::as_u64)
            .and_then(|limit| usize::try_from(limit).ok()),
        multiple,
        ordinal: state.suggester_count,
    };
    let vars = state.vars.clone();
    let choice = state.while_waiting_for_user(|| prompt::resolve_suggestion(&vars, &call));
    Ok(match state.prompt_result(choice)? {
        None => JsonValue::Null,
        Some(indices) if multiple => JsonValue::Array(
            indices
                .into_iter()
                .map(|index| JsonValue::from(index as u64))
                .collect(),
        ),
        Some(indices) => indices
            .first()
            .map_or(JsonValue::Null, |index| JsonValue::from(*index as u64)),
    })
}

/// The subset of Obsidian's `app` API templates commonly use, backed by the
/// vault and cache. Writes stay inside the template's authority.
#[cfg(feature = "js_runtime")]
fn js_app_call(
    state: &mut JsTemplateState,
    name: &str,
    args: &[JsonValue],
) -> Result<JsonValue, String> {
    let path_arg = || args.first().and_then(JsonValue::as_str).unwrap_or_default();
    match name {
        "app.vault_name" => Ok(JsonValue::String(
            state
                .paths
                .vault_root()
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        )),
        "app.files" => match vulcan_core::list_note_identities_with_filter(
            &state.paths,
            state.read_filter.as_ref(),
        ) {
            Ok(notes) => Ok(JsonValue::Array(
                notes
                    .iter()
                    .map(|note| JsonValue::Object(file_object_json(&note.path)))
                    .collect(),
            )),
            Err(error) => {
                state.push_diagnostic(format!(
                    "app.vault.getMarkdownFiles() returned no files because the cache is unavailable ({error}); run `vulcan scan`"
                ));
                Ok(JsonValue::Array(Vec::new()))
            }
        },
        "app.active" => Ok(JsonValue::Object(file_object_json(&state.target_path))),
        "app.file" => Ok(
            js_app_resolve(state, path_arg()).map_or(JsonValue::Null, |path| {
                JsonValue::Object(file_object_json(&path))
            }),
        ),
        "app.exists" => Ok(JsonValue::Bool(js_app_resolve(state, path_arg()).is_some())),
        "app.read" => js_app_read(state, path_arg()).map(JsonValue::String),
        "app.cache" => {
            let Some(path) = js_app_resolve(state, path_arg()) else {
                return Ok(JsonValue::Null);
            };
            let content = js_app_read(state, &path)?;
            Ok(js_app_file_cache(&content, &state.vault_config))
        }
        "app.create" => {
            if !state.allow_mutations {
                state.push_diagnostic(
                    "app.vault.create() is disabled during template preview".to_string(),
                );
                return Ok(JsonValue::Null);
            }
            let normalized = normalize_note_output_path(path_arg())?;
            if js_app_resolve(state, &normalized).is_some() {
                return Err(format!("File already exists: {normalized}"));
            }
            let content = args.get(1).map(json_value_to_js_string).unwrap_or_default();
            js_write_new_note(state, &normalized, content)
        }
        "app.modify" => {
            let content = args.get(1).map(json_value_to_js_string).unwrap_or_default();
            js_app_require_target_in_hooks(state, path_arg(), "app.vault.modify()")?;
            state.target_contents = content;
            Ok(JsonValue::Null)
        }
        "app.frontmatter" => {
            let Some(path) = js_app_resolve(state, path_arg())
                .or_else(|| (path_arg() == state.target_path).then(|| state.target_path.clone()))
            else {
                return Err(format!("File {} doesn't exist", path_arg()));
            };
            let content = js_app_read(state, &path)?;
            Ok(JsonValue::Object(yaml_mapping_to_json_object(
                &current_frontmatter_mapping(&content).unwrap_or_default(),
            )))
        }
        "app.set_frontmatter" => {
            js_app_require_target_in_hooks(
                state,
                path_arg(),
                "app.fileManager.processFrontMatter()",
            )?;
            let mapping: YamlMapping =
                serde_yaml::to_value(args.get(1).unwrap_or(&JsonValue::Null))
                    .ok()
                    .and_then(|value| value.as_mapping().cloned())
                    .unwrap_or_default();
            let (_, body) = parse_frontmatter_document(&state.target_contents, true)
                .map_err(|error| error.to_string())?;
            state.target_contents = if mapping.is_empty() {
                body
            } else {
                with_frontmatter(&mapping, &body)?
            };
            Ok(JsonValue::Null)
        }
        other => Err(format!("unsupported templater JS call: {other}")),
    }
}

#[cfg(feature = "js_runtime")]
fn js_app_resolve(state: &JsTemplateState, identifier: &str) -> Option<String> {
    let identifier = identifier.trim().trim_start_matches('/');
    if identifier == state.target_path {
        return Some(state.target_path.clone());
    }
    resolve_vault_path_from_state(state, identifier)
}

/// A note's content; the note being rendered reads as its current contents.
#[cfg(feature = "js_runtime")]
fn js_app_read(state: &JsTemplateState, identifier: &str) -> Result<String, String> {
    let path = js_app_resolve(state, identifier)
        .ok_or_else(|| format!("File {identifier} doesn't exist"))?;
    if path == state.target_path {
        return Ok(state.target_contents.clone());
    }
    template_path_content(&state.paths, state.staged_creates.as_ref(), &path)
}

/// Writes to a note are limited to the note being rendered, after the
/// template output exists, which is when Templater users make them.
#[cfg(feature = "js_runtime")]
fn js_app_require_target_in_hooks(
    state: &JsTemplateState,
    identifier: &str,
    api: &str,
) -> Result<(), String> {
    let identifier = identifier.trim().trim_start_matches('/');
    if identifier != state.target_path {
        return Err(format!(
            "{api} can only change the note being rendered ({}) in Vulcan templates; use `tp.file.create_new()` or Vulcan commands for other notes",
            state.target_path
        ));
    }
    if !state.in_hooks {
        return Err(format!(
            "{api} on the note being rendered must run inside tp.hooks.on_all_templates_executed(), after the template output exists"
        ));
    }
    Ok(())
}

/// The `CachedMetadata` fields templates read: frontmatter, tags,
/// headings, and links.
#[cfg(feature = "js_runtime")]
fn js_app_file_cache(content: &str, config: &VaultConfig) -> JsonValue {
    let parsed = parse_document(content, config);
    json!({
        "frontmatter": current_frontmatter_mapping(content)
            .map(|mapping| JsonValue::Object(yaml_mapping_to_json_object(&mapping))),
        "tags": parsed.tags.iter().map(|tag| json!({
            "tag": if tag.tag_text.starts_with('#') { tag.tag_text.clone() } else { format!("#{}", tag.tag_text) },
        })).collect::<Vec<_>>(),
        "headings": parsed.headings.iter().map(|heading| json!({
            "heading": heading.text,
            "level": heading.level,
        })).collect::<Vec<_>>(),
        "links": parsed.links.iter().filter(|link| !link.is_note_embed).filter_map(|link| {
            link.target_path_candidate.as_ref().map(|target| json!({
                "link": target,
                "original": link.raw_text,
                "displayText": link.display_text.clone().unwrap_or_else(|| target.clone()),
            }))
        }).collect::<Vec<_>>(),
        "embeds": parsed.links.iter().filter(|link| link.is_note_embed).filter_map(|link| {
            link.target_path_candidate.as_ref().map(|target| json!({
                "link": target,
                "original": link.raw_text,
            }))
        }).collect::<Vec<_>>(),
    })
}

#[cfg(feature = "js_runtime")]
fn js_system_user_command(
    state: &JsTemplateState,
    name: &str,
    args: &[JsonValue],
) -> Result<JsonValue, String> {
    let Some(pair) = state
        .vault_config
        .templates
        .templates_pairs
        .iter()
        .find(|pair| pair.name == name)
    else {
        return Err(format!("unsupported templater JS call: {name}"));
    };
    if !state.vault_config.templates.enable_system_commands {
        return Err(format!(
            "templater system command user function `{name}` is disabled in config"
        ));
    }
    let rendered_command = render_template_request(TemplateRenderRequest {
        paths: &state.paths,
        vault_config: &state.vault_config,
        templates: &state.templates,
        template_path: None,
        template_text: &pair.command,
        target_path: &state.target_path,
        target_contents: Some(&state.target_contents),
        engine: TemplateEngineKind::Templater,
        vars: &state.vars,
        allow_mutations: state.allow_mutations,
        run_mode: template_run_mode_from_code(state.run_mode_code),
        reference_date: None,
    })
    .map_err(|error| error.to_string())?;
    let shell = state.vault_config.templates.shell_path.clone();
    let env_json = args.first().cloned().unwrap_or(JsonValue::Null);
    let mut process = ProcessCommand::new(shell.as_deref().unwrap_or(default_system_shell()));
    configure_shell_command(&mut process, shell.as_deref(), &rendered_command.content);
    process.current_dir(state.paths.vault_root());
    if let Some(env) = env_json.as_object() {
        for (key, value) in env {
            process.env(key, json_value_to_js_string(value));
        }
    }
    let output = process.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("templater system command `{name}` failed"));
    }
    Ok(JsonValue::String(
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string(),
    ))
}

#[cfg(feature = "js_runtime")]
fn json_value_to_js_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => String::new(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => value.clone(),
        JsonValue::Array(items) => items
            .iter()
            .map(json_value_to_js_string)
            .collect::<Vec<_>>()
            .join(","),
        JsonValue::Object(object) => JsonValue::Object(object.clone()).to_string(),
    }
}

#[cfg(feature = "js_runtime")]
fn extract_web_response(body: String, json_path: Option<&str>) -> Result<String, String> {
    let Some(path) = json_path.filter(|value| !value.trim().is_empty()) else {
        return Ok(body);
    };
    let value = json_path_value(
        serde_json::from_str(&body).map_err(|error| error.to_string())?,
        path,
    )?;
    Ok(match value {
        JsonValue::String(text) => text,
        other => other.to_string(),
    })
}

/// The value at a dotted `path` (array indices allowed) inside `value`.
#[cfg(feature = "js_runtime")]
fn json_path_value(mut value: JsonValue, path: &str) -> Result<JsonValue, String> {
    for segment in path.split('.') {
        value = match value {
            JsonValue::Array(items) => {
                let index = segment.parse::<usize>().map_err(|_| {
                    format!("json_path segment `{segment}` is not a valid array index")
                })?;
                items.get(index).cloned().unwrap_or(JsonValue::Null)
            }
            JsonValue::Object(object) => object.get(segment).cloned().unwrap_or(JsonValue::Null),
            _ => JsonValue::Null,
        };
    }
    Ok(value)
}

#[cfg(feature = "js_runtime")]
#[derive(Debug, serde::Deserialize)]
struct DailyQuote {
    quote: String,
    author: String,
}

#[derive(Debug, Clone, PartialEq)]
enum TemplateValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<TemplateValue>),
    Object(JsonMap<String, JsonValue>),
}

impl TemplateValue {
    fn from_json(value: JsonValue) -> Self {
        match value {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(value) => Self::Bool(value),
            JsonValue::Number(value) => Self::Number(value.as_f64().unwrap_or_default()),
            JsonValue::String(value) => Self::String(value),
            JsonValue::Array(items) => {
                Self::Array(items.into_iter().map(Self::from_json).collect())
            }
            JsonValue::Object(object) => Self::Object(object),
        }
    }

    fn as_string_list(&self) -> Vec<String> {
        match self {
            Self::Array(items) => items.iter().map(template_value_to_string).collect(),
            Self::String(value) => vec![value.clone()],
            _ => Vec::new(),
        }
    }
}

fn template_value_to_string(value: &TemplateValue) -> String {
    match value {
        TemplateValue::Null => String::new(),
        TemplateValue::Bool(value) => value.to_string(),
        TemplateValue::Number(value) => {
            if value.fract() == 0.0 {
                format!("{value:.0}")
            } else {
                value.to_string()
            }
        }
        TemplateValue::String(value) => value.clone(),
        TemplateValue::Array(values) => values
            .iter()
            .map(template_value_to_string)
            .collect::<Vec<_>>()
            .join(","),
        TemplateValue::Object(value) => JsonValue::Object(value.clone()).to_string(),
    }
}

#[derive(Debug)]
enum NativeExpressionError {
    Message(String),
    RequiresJsRuntime(String),
    /// A prompt nothing could answer; reported with its own error code.
    InputRequired(String),
}

impl From<prompt::PromptError> for NativeExpressionError {
    fn from(error: prompt::PromptError) -> Self {
        match error {
            prompt::PromptError::InputRequired(message) => Self::InputRequired(message),
            prompt::PromptError::Failed(message) => Self::Message(message),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum NativeExpression {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
    Path(Vec<NativePathPart>),
    Call {
        callee: Vec<NativePathPart>,
        args: Vec<NativeExpression>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NativePathPart {
    Name(String),
    Index(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrimMode {
    None,
    Newline,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemplaterTag<'a> {
    body: &'a str,
    execution: bool,
    dynamic: bool,
    left_trim: TrimMode,
    right_trim: TrimMode,
}

fn parse_templater_tag(source: &str, start: usize) -> Option<(TemplaterTag<'_>, usize)> {
    let rest = source.get(start + 2..)?;
    let mut left_trim = TrimMode::None;
    let mut body_start = 0_usize;
    if let Some(marker) = rest.as_bytes().first().copied() {
        left_trim = trim_mode_from_marker(marker);
        if left_trim != TrimMode::None {
            body_start += 1;
        }
    }

    let mut execution = false;
    let mut dynamic = false;
    if let Some(marker) = rest.as_bytes()[body_start..].first().copied() {
        match marker {
            b'*' => {
                execution = true;
                body_start += 1;
            }
            b'+' => {
                dynamic = true;
                body_start += 1;
            }
            _ => {}
        }
    }

    let close = rest[body_start..].find("%>")?;
    let close_index = body_start + close;
    let body = &rest[body_start..close_index];
    let (body, right_trim) = if let Some(last) = body.as_bytes().last().copied() {
        let trim = trim_mode_from_marker(last);
        if trim == TrimMode::None {
            (body.trim(), TrimMode::None)
        } else {
            (body[..body.len().saturating_sub(1)].trim(), trim)
        }
    } else {
        ("", TrimMode::None)
    };
    Some((
        TemplaterTag {
            body,
            execution,
            dynamic,
            left_trim,
            right_trim,
        },
        start + 2 + close_index + 2,
    ))
}

fn trim_mode_from_marker(marker: u8) -> TrimMode {
    match marker {
        b'-' => TrimMode::Newline,
        b'_' => TrimMode::All,
        _ => TrimMode::None,
    }
}

fn parse_native_expression(source: &str) -> Result<NativeExpression, NativeExpressionError> {
    let mut parser = NativeExpressionParser::new(source);
    let expression = parser.parse_expression()?;
    parser.skip_ws();
    if !parser.is_done() {
        return Err(NativeExpressionError::RequiresJsRuntime(
            "templater expression requires the `js_runtime` feature".to_string(),
        ));
    }
    Ok(expression)
}

struct NativeExpressionParser<'a> {
    source: &'a str,
    cursor: usize,
}

impl<'a> NativeExpressionParser<'a> {
    fn new(source: &'a str) -> Self {
        Self { source, cursor: 0 }
    }

    fn is_done(&self) -> bool {
        self.cursor >= self.source.len()
    }

    fn skip_ws(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.cursor += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.cursor..].chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.cursor += ch.len_utf8();
        Some(ch)
    }

    fn parse_expression(&mut self) -> Result<NativeExpression, NativeExpressionError> {
        self.skip_ws();
        let Some(ch) = self.peek_char() else {
            return Ok(NativeExpression::Null);
        };
        match ch {
            '"' | '\'' => self.parse_string().map(NativeExpression::String),
            '-' | '0'..='9' => self.parse_number().map(NativeExpression::Number),
            _ => self.parse_identifier_path_or_call(),
        }
    }

    fn parse_string(&mut self) -> Result<String, NativeExpressionError> {
        let Some(quote) = self.consume_char() else {
            return Ok(String::new());
        };
        let mut result = String::new();
        while let Some(ch) = self.consume_char() {
            if ch == quote {
                return Ok(result);
            }
            if ch == '\\' {
                if let Some(escaped) = self.consume_char() {
                    result.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                }
            } else {
                result.push(ch);
            }
        }
        Err(NativeExpressionError::Message(
            "unterminated string literal".to_string(),
        ))
    }

    fn parse_number(&mut self) -> Result<f64, NativeExpressionError> {
        let start = self.cursor;
        if self.peek_char() == Some('-') {
            self.consume_char();
        }
        while matches!(self.peek_char(), Some('0'..='9' | '.')) {
            self.consume_char();
        }
        self.source[start..self.cursor]
            .parse::<f64>()
            .map_err(|error| NativeExpressionError::Message(error.to_string()))
    }

    fn parse_identifier_path_or_call(&mut self) -> Result<NativeExpression, NativeExpressionError> {
        let path = self.parse_path()?;
        self.skip_ws();
        if self.peek_char() == Some('(') {
            self.consume_char();
            let mut args = Vec::new();
            loop {
                self.skip_ws();
                if self.peek_char() == Some(')') {
                    self.consume_char();
                    break;
                }
                args.push(self.parse_expression()?);
                self.skip_ws();
                match self.peek_char() {
                    Some(',') => {
                        self.consume_char();
                    }
                    Some(')') => {
                        self.consume_char();
                        break;
                    }
                    _ => {
                        return Err(NativeExpressionError::RequiresJsRuntime(
                            "templater expression requires the `js_runtime` feature".to_string(),
                        ));
                    }
                }
            }
            Ok(NativeExpression::Call { callee: path, args })
        } else if matches!(path_name(&path, 0), Some("true")) && path.len() == 1 {
            Ok(NativeExpression::Bool(true))
        } else if matches!(path_name(&path, 0), Some("false")) && path.len() == 1 {
            Ok(NativeExpression::Bool(false))
        } else if matches!(path_name(&path, 0), Some("null")) && path.len() == 1 {
            Ok(NativeExpression::Null)
        } else {
            Ok(NativeExpression::Path(path))
        }
    }

    fn parse_path(&mut self) -> Result<Vec<NativePathPart>, NativeExpressionError> {
        let mut path = vec![NativePathPart::Name(self.parse_identifier()?)];
        loop {
            self.skip_ws();
            match self.peek_char() {
                Some('.') => {
                    self.consume_char();
                    path.push(NativePathPart::Name(self.parse_identifier()?));
                }
                Some('[') => {
                    self.consume_char();
                    self.skip_ws();
                    let key = self.parse_string()?;
                    self.skip_ws();
                    if self.consume_char() != Some(']') {
                        return Err(NativeExpressionError::Message(
                            "unterminated bracket access".to_string(),
                        ));
                    }
                    path.push(NativePathPart::Index(key));
                }
                _ => break,
            }
        }
        Ok(path)
    }

    fn parse_identifier(&mut self) -> Result<String, NativeExpressionError> {
        self.skip_ws();
        let start = self.cursor;
        while let Some(ch) = self.peek_char() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.consume_char();
            } else {
                break;
            }
        }
        if start == self.cursor {
            return Err(NativeExpressionError::RequiresJsRuntime(
                "templater expression requires the `js_runtime` feature".to_string(),
            ));
        }
        Ok(self.source[start..self.cursor].to_string())
    }
}

fn path_name(path: &[NativePathPart], index: usize) -> Option<&str> {
    match path.get(index) {
        Some(NativePathPart::Name(name) | NativePathPart::Index(name)) => Some(name),
        None => None,
    }
}

fn join_native_path(path: &[NativePathPart]) -> String {
    let mut rendered = String::new();
    for (index, part) in path.iter().enumerate() {
        match part {
            NativePathPart::Name(name) => {
                if index > 0 {
                    rendered.push('.');
                }
                rendered.push_str(name);
            }
            NativePathPart::Index(name) => {
                rendered.push_str("[\"");
                rendered.push_str(name);
                rendered.push_str("\"]");
            }
        }
    }
    rendered
}

fn string_arg(args: &[NativeExpression], index: usize) -> Option<String> {
    match args.get(index) {
        Some(NativeExpression::String(value)) => Some(value.clone()),
        _ => None,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn number_arg(args: &[NativeExpression], index: usize) -> Option<i64> {
    match args.get(index) {
        Some(NativeExpression::Number(value)) => Some(*value as i64),
        _ => None,
    }
}

fn bool_arg(args: &[NativeExpression], index: usize) -> Option<bool> {
    match args.get(index) {
        Some(NativeExpression::Bool(value)) => Some(*value),
        _ => None,
    }
}

#[cfg(feature = "js_runtime")]
fn template_run_mode_from_code(code: i64) -> TemplateRunMode {
    match code {
        1 => TemplateRunMode::Append,
        5 => TemplateRunMode::Dynamic,
        _ => TemplateRunMode::Create,
    }
}

fn normalize_note_output_path(path: &str) -> Result<String, String> {
    vulcan_core::paths::normalize_relative_input_path(
        path,
        vulcan_core::paths::RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(|error| error.to_string())
}

fn yaml_mapping_to_json_object(mapping: &YamlMapping) -> JsonMap<String, JsonValue> {
    mapping
        .iter()
        .filter_map(|(key, value)| {
            key.as_str()
                .map(|key| (key.to_string(), yaml_to_json(value)))
        })
        .collect::<JsonMap<_, _>>()
}

fn yaml_to_json(value: &YamlValue) -> JsonValue {
    match value {
        YamlValue::Bool(value) => JsonValue::Bool(*value),
        YamlValue::Number(value) => value
            .as_i64()
            .map(Into::into)
            .map(JsonValue::Number)
            .or_else(|| {
                value
                    .as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map(JsonValue::Number)
            })
            .unwrap_or(JsonValue::Null),
        YamlValue::String(value) => JsonValue::String(value.clone()),
        YamlValue::Sequence(items) => JsonValue::Array(items.iter().map(yaml_to_json).collect()),
        YamlValue::Mapping(mapping) => JsonValue::Object(yaml_mapping_to_json_object(mapping)),
        YamlValue::Null | YamlValue::Tagged(_) => JsonValue::Null,
    }
}

fn current_frontmatter_mapping(source: &str) -> Option<YamlMapping> {
    parse_frontmatter_document(source, false)
        .ok()
        .and_then(|(frontmatter, _)| frontmatter)
}

fn merge_yaml_mappings(target: &mut YamlMapping, source: &YamlMapping) {
    for (key, value) in source {
        if !target.contains_key(key) {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// Splits an include link such as `[[Note#Heading|alias]]`, `[[Note#^id]]`,
/// or `Note` into the note and its optional heading/block subpath.
fn split_include_link(link: &str) -> (&str, Option<&str>) {
    let trimmed = link.trim();
    let inner = trimmed
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
        .unwrap_or(trimmed);
    let inner = inner.split('|').next().unwrap_or(inner);
    match inner.split_once('#') {
        Some((note, subpath)) => (
            note.trim(),
            Some(subpath.trim()).filter(|subpath| !subpath.is_empty()),
        ),
        None => (inner.trim(), None),
    }
}

/// The part of a note an include subpath names, as Obsidian embeds it: a
/// heading with everything up to the next heading of the same or higher
/// level, or a `^block` without its marker.
fn extract_include_subpath(
    source: &str,
    subpath: &str,
    config: &VaultConfig,
) -> Result<String, String> {
    let parsed = parse_document(source, config);
    if let Some(block_id) = subpath.strip_prefix('^') {
        let block = parsed
            .block_refs
            .iter()
            .find(|block| block.block_id_text.trim_start_matches('^') == block_id)
            .ok_or_else(|| format!("block ^{block_id} not found"))?;
        let text = &source[block.target_block_byte_start..block.target_block_byte_end];
        let marker = format!(" ^{block_id}");
        return Ok(text
            .lines()
            .map(|line| line.trim_end().strip_suffix(&marker).unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let wanted = subpath.rsplit('#').next().unwrap_or(subpath).trim();
    let index = parsed
        .headings
        .iter()
        .position(|heading| heading.text.trim().eq_ignore_ascii_case(wanted))
        .ok_or_else(|| format!("heading #{wanted} not found"))?;
    let heading = &parsed.headings[index];
    let end = parsed.headings[index + 1..]
        .iter()
        .find(|next| next.level <= heading.level)
        .map_or(source.len(), |next| next.byte_offset);
    Ok(source[heading.byte_offset..end].to_string())
}

/// `body` under a frontmatter block holding `mapping`.
fn with_frontmatter(mapping: &YamlMapping, body: &str) -> Result<String, String> {
    let yaml = serde_yaml::to_string(&YamlValue::Mapping(mapping.clone()))
        .map_err(|error| error.to_string())?;
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    Ok(format!(
        "---\n{}\n---\n{}",
        yaml.trim_end_matches('\n'),
        body
    ))
}

fn merge_pending_frontmatter_mapping(pending: &mut Option<YamlMapping>, frontmatter: &YamlMapping) {
    match pending {
        Some(existing) => merge_yaml_mappings(existing, frontmatter),
        None => *pending = Some(frontmatter.clone()),
    }
}

/// Name a render reports in errors: the template's vault-relative path when
/// known, otherwise a generic label.
fn template_source_name(request: &TemplateRenderRequest<'_>) -> String {
    request.template_path.map_or_else(
        || "inline template".to_string(),
        |path| {
            path.strip_prefix(request.paths.vault_root())
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        },
    )
}

/// Prefixes an error with the template and line it came from, plus the
/// offending source line, keeping the error code machine-readable.
fn located_template_error(
    error: &CliError,
    source_name: &str,
    line: Option<usize>,
    template: &str,
) -> CliError {
    const MAX_EXCERPT_CHARS: usize = 160;
    let mut message = match line {
        Some(line) => format!(
            "template `{source_name}` failed at line {line}: {}",
            error.message()
        ),
        None => format!("template `{source_name}` failed: {}", error.message()),
    };
    if let Some((line, excerpt)) = line.and_then(|line| {
        template
            .lines()
            .nth(line.checked_sub(1)?)
            .map(|text| (line, text.trim_end()))
    }) {
        let excerpt: String = if excerpt.chars().count() > MAX_EXCERPT_CHARS {
            excerpt
                .chars()
                .take(MAX_EXCERPT_CHARS)
                .chain("…".chars())
                .collect()
        } else {
            excerpt.to_string()
        };
        message = format!("{message}\n  {line} | {excerpt}");
    }
    match error.code() {
        Some(code) => CliError::operation_with_code(code.to_string(), message),
        None => CliError::operation(message),
    }
}

/// Native-interpreter calls; anything else in a tag needs JavaScript.
#[cfg(feature = "js_runtime")]
const NATIVE_TEMPLATER_CALLS: &[&str] = &[
    "tp.date.now",
    "tp.date.tomorrow",
    "tp.date.yesterday",
    "tp.date.weekday",
    "tp.file.path",
    "tp.file.folder",
    "tp.file.creation_date",
    "tp.file.last_modified_date",
    "tp.file.exists",
    "tp.file.include",
    "tp.file.create_new",
    "tp.file.move",
    "tp.file.rename",
    "tp.file.cursor",
    "tp.file.find_tfile",
    "tp.system.prompt",
    "tp.system.suggester",
    "tp.system.clipboard",
];

/// Whether a tag needs the JS runtime. Decided before rendering, so a
/// template never runs half natively and half in JavaScript.
#[cfg(feature = "js_runtime")]
fn tag_requires_js(tag: &TemplaterTag<'_>) -> bool {
    tag.execution
        || parse_native_expression(tag.body).map_or(true, |expression| {
            native_expression_requires_js(&expression)
        })
}

#[cfg(feature = "js_runtime")]
fn native_expression_requires_js(expression: &NativeExpression) -> bool {
    match expression {
        NativeExpression::String(_)
        | NativeExpression::Number(_)
        | NativeExpression::Bool(_)
        | NativeExpression::Null => false,
        NativeExpression::Path(path) => !native_path_supported(path),
        NativeExpression::Call { callee, args } => {
            !NATIVE_TEMPLATER_CALLS.contains(&join_native_path(callee).as_str())
                || args.iter().any(native_expression_requires_js)
        }
    }
}

#[cfg(feature = "js_runtime")]
fn native_path_supported(path: &[NativePathPart]) -> bool {
    match (path_name(path, 0), path_name(path, 1), path_name(path, 2)) {
        (Some("tp"), Some("frontmatter" | "config"), _) => true,
        (Some("tp"), Some("file"), None) => path.len() == 2,
        (Some("tp"), Some("file"), Some("title" | "content" | "tags" | "path" | "folder")) => {
            path.len() == 3
        }
        _ => false,
    }
}

fn prompt_lookup_key(prompt: &str, ordinal: usize) -> PromptLookupKey {
    PromptLookupKey {
        raw: prompt.to_string(),
        slug: if prompt.trim().is_empty() {
            format!("prompt_{ordinal}")
        } else {
            slugify_var_key(prompt)
        },
    }
}

struct PromptLookupKey {
    raw: String,
    slug: String,
}

impl PromptLookupKey {
    fn named(name: &str) -> Self {
        Self {
            raw: name.trim().to_string(),
            slug: quickadd_storage_key(name, "value"),
        }
    }
}

fn lookup_template_var(
    vars: &HashMap<String, String>,
    key: &PromptLookupKey,
    exact_fallback: Option<&str>,
) -> Option<String> {
    vars.get(&key.raw)
        .cloned()
        .or_else(|| exact_fallback.and_then(|value| vars.get(value).cloned()))
        .or_else(|| vars.get(&key.slug).cloned())
}

fn slugify_var_key(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            slug.push('_');
            last_was_separator = true;
        }
    }
    slug.trim_matches('_').to_string()
}

fn quickadd_strip_keyword<'a>(
    input: &'a str,
    keyword: &str,
    allowed_next: &[char],
) -> Option<&'a str> {
    if input.len() < keyword.len() || !input[..keyword.len()].eq_ignore_ascii_case(keyword) {
        return None;
    }
    let rest = &input[keyword.len()..];
    if rest.is_empty()
        || rest
            .chars()
            .next()
            .is_some_and(|character| allowed_next.contains(&character))
    {
        Some(rest)
    } else {
        None
    }
}

fn parse_quickadd_format_and_offset(rest: &str, default_format: &str) -> (String, i64) {
    if rest.is_empty() {
        return (default_format.to_string(), 0);
    }
    if let Some(offset) = rest.strip_prefix('+').and_then(parse_quickadd_offset_days) {
        return (default_format.to_string(), offset);
    }
    if let Some(format_and_offset) = rest.strip_prefix(':') {
        let (format, offset) =
            if let Some((format, offset_text)) = format_and_offset.rsplit_once('+') {
                if let Some(offset) = parse_quickadd_offset_days(offset_text) {
                    (format, offset)
                } else {
                    (format_and_offset, 0)
                }
            } else {
                (format_and_offset, 0)
            };
        return (
            unescape_quickadd_text(format.trim()).if_empty(default_format),
            offset,
        );
    }
    (default_format.to_string(), 0)
}

fn parse_quickadd_offset_days(text: &str) -> Option<i64> {
    text.trim().parse::<i64>().ok()
}

fn split_once_unescaped(input: &str, needle: char) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (index, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == needle {
            return Some((&input[..index], &input[index + ch.len_utf8()..]));
        }
    }
    None
}

fn split_unescaped(input: &str, needle: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (index, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == needle {
            parts.push(&input[start..index]);
            start = index + ch.len_utf8();
        }
    }
    parts.push(&input[start..]);
    parts
}

fn unescape_quickadd_text(value: &str) -> String {
    value
        .replace("\\|", "|")
        .replace("\\,", ",")
        .replace("\\\\", "\\")
}

fn quickadd_storage_key(value: &str, fallback: &str) -> String {
    let slug = slugify_var_key(value);
    if slug.is_empty() {
        fallback.to_string()
    } else {
        slug
    }
}

fn current_file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

fn current_file_wikilink(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let link_target = trimmed
        .strip_suffix(".md")
        .or_else(|| trimmed.strip_suffix(".canvas"))
        .unwrap_or(trimmed);
    format!("[[{link_target}]]")
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct QuickAddValueSpec {
    variable_name: Option<String>,
    default_value: Option<String>,
    label: Option<String>,
    case_style: Option<String>,
    choices: Vec<String>,
}

fn parse_quickadd_value_spec(rest: &str) -> QuickAddValueSpec {
    let mut spec = QuickAddValueSpec::default();
    if rest.is_empty() {
        return spec;
    }

    let (head, option_suffix) = if let Some(body) = rest.strip_prefix(':') {
        split_once_unescaped(body, '|').map_or((body, None), |(head, tail)| (head, Some(tail)))
    } else if let Some(body) = rest.strip_prefix('|') {
        ("", Some(body))
    } else {
        ("", None)
    };

    let head = head.trim();
    if !head.is_empty() {
        let choices = split_unescaped(head, ',')
            .into_iter()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(unescape_quickadd_text)
            .collect::<Vec<_>>();
        if head.contains(',') {
            spec.choices = choices;
        } else {
            spec.variable_name = Some(unescape_quickadd_text(head));
        }
    }

    if let Some(option_suffix) = option_suffix {
        for option in split_unescaped(option_suffix, '|') {
            let option = option.trim();
            if option.is_empty() {
                continue;
            }
            if let Some(value) = option.strip_prefix("default:") {
                spec.default_value = Some(unescape_quickadd_text(value.trim()));
            } else if let Some(value) = option.strip_prefix("label:") {
                spec.label = Some(unescape_quickadd_text(value.trim()));
            } else if let Some(value) = option.strip_prefix("case:") {
                spec.case_style = Some(value.trim().to_ascii_lowercase());
            } else if option.contains(':')
                || matches!(option, "custom")
                || option.starts_with("type:")
                || option.starts_with("text:")
            {
            } else if spec.default_value.is_none() {
                spec.default_value = Some(unescape_quickadd_text(option));
            }
        }
    }

    spec
}

fn apply_quickadd_case_style(value: &str, style: &str) -> String {
    let words = quickadd_words(value);
    match style.trim().to_ascii_lowercase().as_str() {
        "kebab" | "slug" => words.join("-").to_ascii_lowercase(),
        "snake" => words.join("_").to_ascii_lowercase(),
        "camel" => {
            let mut iter = words.into_iter();
            let Some(first) = iter.next() else {
                return String::new();
            };
            let mut rendered = first.to_ascii_lowercase();
            for word in iter {
                rendered.push_str(&capitalize_ascii(&word.to_ascii_lowercase()));
            }
            rendered
        }
        "pascal" => words
            .into_iter()
            .map(|word| capitalize_ascii(&word.to_ascii_lowercase()))
            .collect::<String>(),
        "title" => words
            .into_iter()
            .map(|word| capitalize_ascii(&word.to_ascii_lowercase()))
            .collect::<Vec<_>>()
            .join(" "),
        "lower" => value.to_ascii_lowercase(),
        "upper" => value.to_ascii_uppercase(),
        _ => value.to_string(),
    }
}

fn quickadd_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn capitalize_ascii(value: &str) -> String {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut rendered = String::new();
    rendered.push(first.to_ascii_uppercase());
    rendered.push_str(chars.as_str());
    rendered
}

trait QuickAddEmptyExt {
    fn if_empty(self, fallback: &str) -> String;
}

impl QuickAddEmptyExt for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.trim().is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn quickadd_parse_date_input(text: &str, reference_ms: i64) -> Option<i64> {
    let normalized = text.trim();
    if normalized.is_empty() {
        return None;
    }

    let lower = normalized.to_ascii_lowercase();
    let day_ms = match lower.as_str() {
        "t" | "today" => quickadd_day_start(reference_ms),
        "tm" | "tomorrow" => quickadd_day_start(reference_ms) + DAY_MS,
        "yd" | "yesterday" => quickadd_day_start(reference_ms) - DAY_MS,
        "next week" => quickadd_day_start(reference_ms) + (7 * DAY_MS),
        _ => {
            if let Some(days) = parse_relative_day_phrase(&lower) {
                quickadd_day_start(reference_ms) + days * DAY_MS
            } else if let Some(weekday) = quickadd_weekday_name_to_index(&lower) {
                quickadd_next_weekday(reference_ms, weekday, false)
            } else if let Some(weekday) = lower
                .strip_prefix("next ")
                .and_then(quickadd_weekday_name_to_index)
            {
                quickadd_next_weekday(reference_ms, weekday, true)
            } else if let Some(parsed) = quickadd_parse_month_day_phrase(&lower, reference_ms) {
                parsed
            } else {
                parse_date_like_string(normalized).map(quickadd_day_start)?
            }
        }
    };

    Some(day_ms)
}

fn parse_relative_day_phrase(text: &str) -> Option<i64> {
    if let Some(days) = text
        .strip_prefix("in ")
        .and_then(|value| value.strip_suffix(" days"))
        .or_else(|| {
            text.strip_prefix("in ")
                .and_then(|value| value.strip_suffix(" day"))
        })
    {
        return days.trim().parse::<i64>().ok();
    }
    if let Some(days) = text
        .strip_suffix(" days")
        .or_else(|| text.strip_suffix(" day"))
        .and_then(|value| value.trim().parse::<i64>().ok())
    {
        return Some(days);
    }
    text.parse::<i64>().ok()
}

fn quickadd_parse_month_day_phrase(text: &str, reference_ms: i64) -> Option<i64> {
    let regex = Regex::new(
        r"(?i)^(?P<month>jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\s+(?P<day>\d{1,2})(?:st|nd|rd|th)?(?:,\s*(?P<year>\d{4}))?$",
    )
    .ok()?;
    let capture = regex.captures(text)?;
    let month = quickadd_month_name_to_number(capture.name("month")?.as_str())?;
    let day = capture.name("day")?.as_str().parse::<i64>().ok()?;
    let (reference_year, _, _, _, _, _, _) =
        vulcan_core::expression::functions::date_components(quickadd_day_start(reference_ms));
    let year = capture
        .name("year")
        .and_then(|value| value.as_str().parse::<i64>().ok())
        .unwrap_or(reference_year);
    let base = parse_date_like_string(&format!("{year:04}-{month:02}-{day:02}"))?;
    let candidate = if capture.name("year").is_none() && base < quickadd_day_start(reference_ms) {
        parse_date_like_string(&format!("{:04}-{month:02}-{day:02}", year + 1))?
    } else {
        base
    };
    Some(quickadd_day_start(candidate))
}

fn quickadd_weekday_name_to_index(text: &str) -> Option<i64> {
    match text {
        "monday" => Some(1),
        "tuesday" => Some(2),
        "wednesday" => Some(3),
        "thursday" => Some(4),
        "friday" => Some(5),
        "saturday" => Some(6),
        "sunday" => Some(7),
        _ => None,
    }
}

fn quickadd_month_name_to_number(text: &str) -> Option<i64> {
    match text {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "sept" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => None,
    }
}

fn quickadd_next_weekday(reference_ms: i64, target: i64, force_next_week: bool) -> i64 {
    let current_day = quickadd_day_start(reference_ms);
    let current_weekday = ((current_day.div_euclid(DAY_MS) + 3).rem_euclid(7)) + 1;
    let mut delta = (target - current_weekday).rem_euclid(7);
    if force_next_week || delta == 0 {
        delta += 7;
    }
    current_day + delta * DAY_MS
}

fn quickadd_day_start(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS) * DAY_MS
}

fn read_clipboard_best_effort() -> String {
    clipboard_commands()
        .into_iter()
        .find_map(|command| run_clipboard_command(&command).ok())
        .unwrap_or_default()
}

fn clipboard_commands() -> Vec<Vec<&'static str>> {
    #[cfg(target_os = "macos")]
    {
        vec![vec!["pbpaste"]]
    }
    #[cfg(target_os = "windows")]
    {
        vec![vec![
            "powershell",
            "-NoProfile",
            "-Command",
            "Get-Clipboard",
        ]]
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        vec![
            vec!["wl-paste", "-n"],
            vec!["xclip", "-selection", "clipboard", "-o"],
            vec!["xsel", "--clipboard", "--output"],
        ]
    }
}

fn run_clipboard_command(command: &[&str]) -> Result<String, String> {
    let Some(program) = command.first() else {
        return Err("clipboard command missing program".to_string());
    };
    let output = ProcessCommand::new(program)
        .args(&command[1..])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("clipboard command failed: {command:?}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[cfg(feature = "js_runtime")]
fn default_system_shell() -> &'static Path {
    #[cfg(target_os = "windows")]
    {
        Path::new("powershell")
    }
    #[cfg(not(target_os = "windows"))]
    {
        Path::new("/bin/sh")
    }
}

#[cfg(feature = "js_runtime")]
fn configure_shell_command(process: &mut ProcessCommand, shell: Option<&Path>, command: &str) {
    #[cfg(target_os = "windows")]
    {
        let shell_name = shell
            .and_then(|value| value.file_name())
            .and_then(|value| value.to_str())
            .unwrap_or("powershell")
            .to_ascii_lowercase();
        if shell_name == "cmd" || shell_name == "cmd.exe" {
            process.arg("/C").arg(command);
        } else if shell_name.contains("powershell")
            || shell_name == "pwsh"
            || shell_name == "pwsh.exe"
        {
            process.arg("-NoProfile").arg("-Command").arg(command);
        } else {
            process.arg("-lc").arg(command);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = shell;
        process.arg("-lc").arg(command);
    }
}

fn apply_date_offset(
    millis: i64,
    offset: Option<&NativeExpression>,
) -> Result<i64, NativeExpressionError> {
    match offset {
        None => Ok(millis),
        Some(NativeExpression::Number(days)) => {
            #[allow(clippy::cast_possible_truncation)]
            {
                Ok(millis + (days * 86_400_000.0) as i64)
            }
        }
        Some(NativeExpression::String(value)) => {
            apply_offset_string(millis, value).map_err(|error| {
                NativeExpressionError::Message(format!("invalid date offset `{value}`: {error}"))
            })
        }
        Some(_) => Err(NativeExpressionError::Message(
            "date offset must be a number or ISO 8601 duration string".to_string(),
        )),
    }
}

fn apply_offset_string(millis: i64, offset: &str) -> Result<i64, String> {
    let offset = offset.trim();
    if let Some(days) = parse_iso_day_offset(offset) {
        return Ok(millis + days * 86_400_000);
    }
    Err("expected values like P1D, P-1M, or P1Y".to_string())
}

fn parse_iso_day_offset(input: &str) -> Option<i64> {
    let trimmed = input.trim().strip_prefix('P')?;
    let (number, unit) = trimmed.split_at(trimmed.len().saturating_sub(1));
    let value = number.parse::<i64>().ok()?;
    let multiplier = match unit {
        "D" => 1,
        "W" => 7,
        "M" => 30,
        "Y" => 365,
        _ => return None,
    };
    Some(value * multiplier)
}

fn weekday_timestamp(reference: i64, weekday: i64) -> i64 {
    let (year, month, day, _, _, _, _) =
        vulcan_core::expression::functions::date_components(reference);
    let current = iso_weekday(year, month, day);
    let monday_based = current - 1;
    let delta = weekday - monday_based;
    reference + delta * 86_400_000
}

fn iso_weekday(year: i64, month: i64, day: i64) -> i64 {
    let days = days_from_civil(year, month, day);
    ((days + 3).rem_euclid(7)) + 1
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_index = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn file_timestamp_millis(path: &Path, creation: bool) -> Option<i64> {
    let metadata = fs::metadata(path).ok()?;
    let system_time = if creation {
        metadata.created().ok().or_else(|| metadata.modified().ok())
    } else {
        metadata.modified().ok()
    }?;
    let duration = system_time.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

#[cfg(feature = "js_runtime")]
fn current_timestamp_millis() -> i64 {
    vulcan_core::current_utc_timestamp_ms()
}

#[cfg(feature = "js_runtime")]
fn js_daily_quote(state: &JsTemplateState) -> Result<String, String> {
    const DAILY_QUOTE_URL: &str =
        "https://raw.githubusercontent.com/Zachatoo/quotes-database/refs/heads/main/quotes.json";

    ensure_allowlisted_url(&state.vault_config.templates, DAILY_QUOTE_URL)?;
    let body = http_get_text(DAILY_QUOTE_URL)?;
    let quotes: Vec<DailyQuote> = serde_json::from_str(&body).map_err(|error| error.to_string())?;
    let Some(quote_count) = i64::try_from(quotes.len()).ok().filter(|count| *count > 0) else {
        return Err("templater daily quote source returned no quotes".to_string());
    };
    let day_index = (current_timestamp_millis() / 86_400_000).rem_euclid(quote_count);
    let quote = &quotes[usize::try_from(day_index).unwrap_or_default()];
    Ok(format!(
        "> [!quote] {}\n> — {}",
        quote.quote.trim(),
        quote.author.trim()
    ))
}

fn file_object_json(path: &str) -> JsonMap<String, JsonValue> {
    let relative = path.replace('\\', "/");
    let stem = Path::new(&relative)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let folder = Path::new(&relative)
        .parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let name = Path::new(&relative)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let extension = Path::new(&relative)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_string();
    let parent = json!({
        "path": if folder.is_empty() { "/" } else { folder.as_str() },
        "name": folder.rsplit('/').next().unwrap_or_default(),
    });
    // Mirrors the Obsidian `TFile` fields templates read, plus `title`/`folder`.
    JsonMap::from_iter([
        ("path".to_string(), JsonValue::String(relative.clone())),
        ("name".to_string(), JsonValue::String(name)),
        ("basename".to_string(), JsonValue::String(stem.clone())),
        ("extension".to_string(), JsonValue::String(extension)),
        ("title".to_string(), JsonValue::String(stem)),
        ("folder".to_string(), JsonValue::String(folder)),
        ("parent".to_string(), parent),
    ])
}

/// `TFile` for a template, vault-relative when it lives inside the vault.
fn path_to_relative_file_json(paths: &VaultPaths, path: &Path) -> JsonMap<String, JsonValue> {
    let relative = path.strip_prefix(paths.vault_root()).unwrap_or(path);
    file_object_json(&relative.to_string_lossy().replace('\\', "/"))
}

#[cfg(feature = "js_runtime")]
fn resolve_vault_path_from_state(state: &JsTemplateState, identifier: &str) -> Option<String> {
    resolve_unfiltered_vault_path_from_state(state, identifier).filter(|path| {
        state
            .read_filter
            .as_ref()
            .is_none_or(|filter| filter.is_allowed(path))
    })
}

#[cfg(feature = "js_runtime")]
fn resolve_unfiltered_vault_path_from_state(
    state: &JsTemplateState,
    identifier: &str,
) -> Option<String> {
    if let Ok(reference) = resolve_note_reference(&state.paths, identifier) {
        return normalize_template_include_path(&reference.path).filter(|path| {
            template_path_exists(&state.paths, state.staged_creates.as_ref(), path)
        });
    }
    let (trimmed, _) = split_include_link(identifier);
    let path = Path::new(trimmed);
    let candidate = if path.extension().is_none() {
        PathBuf::from(format!("{trimmed}.md"))
    } else {
        path.to_path_buf()
    };
    let normalized = normalize_template_include_path(&candidate.to_string_lossy())?;
    template_path_exists(&state.paths, state.staged_creates.as_ref(), &normalized)
        .then_some(normalized)
}

/// Why template web requests fail in a build without the `web` feature.
#[cfg(all(feature = "js_runtime", not(feature = "web")))]
const TEMPLATE_WEB_UNAVAILABLE: &str =
    "templater web requests need a Vulcan build with the `web` feature";

#[cfg(all(feature = "js_runtime", feature = "web"))]
fn http_get_text(url: &str) -> Result<String, String> {
    reqwest::blocking::get(url)
        .map_err(|error| error.to_string())?
        .text()
        .map_err(|error| error.to_string())
}

#[cfg(all(feature = "js_runtime", not(feature = "web")))]
fn http_get_text(_url: &str) -> Result<String, String> {
    Err(TEMPLATE_WEB_UNAVAILABLE.to_string())
}

#[cfg(all(feature = "js_runtime", not(feature = "web")))]
fn ensure_allowlisted_url(_config: &TemplatesConfig, _url: &str) -> Result<(), String> {
    Err(TEMPLATE_WEB_UNAVAILABLE.to_string())
}

#[cfg(all(feature = "js_runtime", feature = "web"))]
fn ensure_allowlisted_url(config: &TemplatesConfig, url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    let Some(host) = parsed.host_str() else {
        return Err("templater web requests require a hostname".to_string());
    };
    if config
        .web_allowlist
        .iter()
        .any(|allowed| host == allowed || host.ends_with(&format!(".{allowed}")))
    {
        Ok(())
    } else {
        Err(format!(
            "templater web request host `{host}` is not allowlisted in [templates].web_allowlist"
        ))
    }
}

#[cfg(any(feature = "js_runtime", test))]
fn random_picture_markdown(size: Option<&str>, query: Option<&str>, include_size: bool) -> String {
    let mut url = "https://source.unsplash.com/random".to_string();
    if let Some(size) = size.filter(|value| !value.trim().is_empty()) {
        url.push('/');
        url.push_str(size);
    }
    if let Some(query) = query.filter(|value| !value.trim().is_empty()) {
        url.push('?');
        url.push_str(query);
    }
    if include_size {
        size.map_or_else(
            || format!("![]({url})"),
            |size| format!("![]({url}|{size})"),
        )
    } else {
        format!("![]({url})")
    }
}

#[cfg(feature = "js_runtime")]
fn html_to_markdown(html: &str) -> String {
    html.replace("<strong>", "**")
        .replace("</strong>", "**")
        .replace("<b>", "**")
        .replace("</b>", "**")
        .replace("<em>", "*")
        .replace("</em>", "*")
        .replace("<i>", "*")
        .replace("</i>", "*")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<p>", "")
        .replace("</p>", "\n\n")
        .replace("<h1>", "# ")
        .replace("</h1>", "\n\n")
        .replace("<h2>", "## ")
        .replace("</h2>", "\n\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateCandidate {
    pub name: String,
    pub source: &'static str,
    pub display_path: String,
    pub absolute_path: PathBuf,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateDiscovery {
    pub templates: Vec<TemplateCandidate>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateInsertMode {
    Append,
    Prepend,
}

impl TemplateInsertMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Prepend => "prepend",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTemplateInsertion {
    pub merged_frontmatter: Option<YamlMapping>,
    pub target_body: String,
    pub template_body: String,
}

#[derive(Debug)]
pub enum TemplateInsertionError {
    NoteFrontmatterNotMapping,
    NoteFrontmatterParse(serde_yaml::Error),
    TemplateFrontmatterNotMapping,
    TemplateFrontmatterParse(serde_yaml::Error),
    YamlSerialize(serde_yaml::Error),
}

impl std::fmt::Display for TemplateInsertionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoteFrontmatterNotMapping => {
                formatter.write_str("note frontmatter must be a YAML mapping")
            }
            Self::NoteFrontmatterParse(error) => {
                write!(formatter, "failed to parse note frontmatter: {error}")
            }
            Self::TemplateFrontmatterNotMapping => {
                formatter.write_str("template frontmatter must be a YAML mapping")
            }
            Self::TemplateFrontmatterParse(error) => {
                write!(formatter, "failed to parse template frontmatter: {error}")
            }
            Self::YamlSerialize(error) => {
                write!(formatter, "failed to serialize merged frontmatter: {error}")
            }
        }
    }
}

impl std::error::Error for TemplateInsertionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NoteFrontmatterParse(error)
            | Self::TemplateFrontmatterParse(error)
            | Self::YamlSerialize(error) => Some(error),
            Self::NoteFrontmatterNotMapping | Self::TemplateFrontmatterNotMapping => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateVariables {
    pub title: String,
    pub date: String,
    pub time: String,
    pub datetime: String,
    pub uuid: String,
    pub timestamp: TemplateTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemplateTimestamp {
    days_since_epoch: i64,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampStrings {
    pub date: String,
    pub time: String,
    pub datetime: String,
}

pub fn discover_templates(
    paths: &VaultPaths,
    obsidian_folder: Option<&Path>,
    templater_folder: Option<&Path>,
) -> Result<TemplateDiscovery, CliError> {
    let mut warnings = Vec::new();
    let mut templates = list_templates_in_directory(
        &paths.vulcan_dir().join("templates"),
        ".vulcan/templates",
        "vulcan",
    )?;
    merge_template_source(
        &mut templates,
        &mut warnings,
        paths,
        templater_folder,
        "templater",
    )?;
    merge_template_source(
        &mut templates,
        &mut warnings,
        paths,
        obsidian_folder,
        "obsidian",
    )?;

    templates.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(TemplateDiscovery {
        templates,
        warnings,
    })
}

fn merge_template_source(
    templates: &mut Vec<TemplateCandidate>,
    warnings: &mut Vec<String>,
    paths: &VaultPaths,
    folder: Option<&Path>,
    source: &'static str,
) -> Result<(), CliError> {
    let mut discovered = folder
        .filter(|folder| !folder.as_os_str().is_empty())
        .map(|folder| {
            list_templates_in_directory(
                &paths.vault_root().join(folder),
                &folder.to_string_lossy(),
                source,
            )
        })
        .transpose()?
        .unwrap_or_default();

    for candidate in discovered.drain(..) {
        if let Some(existing) = templates
            .iter_mut()
            .find(|template| template.name == candidate.name)
        {
            let warning = format!(
                "template {} exists in both {} and {}; using {}",
                existing.name, candidate.display_path, existing.display_path, existing.display_path
            );
            existing.warning = Some(warning.clone());
            warnings.push(warning);
        } else {
            templates.push(candidate);
        }
    }

    Ok(())
}

pub fn resolve_template_file(
    paths: &VaultPaths,
    templates: &[TemplateCandidate],
    name: &str,
) -> Result<TemplateCandidate, CliError> {
    let name_normalized = name.trim_end_matches(".md").replace('\\', "/");

    if let Some(template) = templates.iter().find(|template| {
        let stem = template.name.trim_end_matches(".md");
        let display_stem = template
            .display_path
            .trim_end_matches(".md")
            .replace('\\', "/");
        stem == name_normalized
            || stem == name
            || display_stem == name_normalized
            || display_stem == name
    }) {
        return Ok(template.clone());
    }

    let mut searched = vec![paths.vulcan_dir().join("templates").display().to_string()];
    let templates_config = load_vault_config(paths).config.templates;
    let templater_folder = templates_config.templater_folder;
    let obsidian_folder = templates_config.obsidian_folder;
    if let Some(folder) = templater_folder.filter(|folder| !folder.as_os_str().is_empty()) {
        searched.push(paths.vault_root().join(folder).display().to_string());
    }
    if let Some(folder) = obsidian_folder.filter(|folder| !folder.as_os_str().is_empty()) {
        searched.push(paths.vault_root().join(folder).display().to_string());
    }

    Err(CliError::operation(format!(
        "template not found in {}: {name}",
        searched.join(", ")
    )))
}

pub fn list_templates_in_directory(
    template_dir: &Path,
    display_root: &str,
    source: &'static str,
) -> Result<Vec<TemplateCandidate>, CliError> {
    if !template_dir.exists() {
        return Ok(Vec::new());
    }

    let mut templates = Vec::new();
    collect_templates_recursive(
        template_dir,
        template_dir,
        display_root,
        source,
        &mut templates,
    )?;
    templates.sort_by(|left, right| left.display_path.cmp(&right.display_path));
    Ok(templates)
}

fn collect_templates_recursive(
    base_dir: &Path,
    current_dir: &Path,
    display_root: &str,
    source: &'static str,
    out: &mut Vec<TemplateCandidate>,
) -> Result<(), CliError> {
    let entries = fs::read_dir(current_dir).map_err(CliError::operation)?;
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_templates_recursive(base_dir, &path, display_root, source, out)?;
        } else if vulcan_core::paths::has_markdown_extension(&path) {
            let Some(name) = path
                .file_name()
                .and_then(|v| v.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let rel = path
                .strip_prefix(base_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let display_path = format!("{display_root}/{rel}");
            out.push(TemplateCandidate {
                name,
                display_path,
                source,
                absolute_path: path,
                warning: None,
            });
        }
    }
    Ok(())
}

pub fn template_output_path(
    template_file: &str,
    output_path: Option<&str>,
    now: &TemplateTimestamp,
) -> Result<String, CliError> {
    let path = if let Some(path) = output_path {
        path.to_string()
    } else {
        let date = now.default_date_string();
        format!("{date}-{template_file}")
    };

    normalize_relative_input_path(
        &path,
        RelativePathOptions {
            expected_extension: Some("md"),
            append_extension_if_missing: true,
        },
    )
    .map_err(CliError::operation)
}

#[must_use]
pub fn render_template_variable(
    expression: &str,
    variables: &TemplateVariables,
    config: &TemplatesConfig,
) -> Option<String> {
    if let Some(format) = expression.strip_prefix("date:") {
        return Some(variables.timestamp.format_obsidian(format.trim()));
    }
    if let Some(format) = expression.strip_prefix("time:") {
        return Some(variables.timestamp.format_obsidian(format.trim()));
    }

    match expression {
        "title" => Some(variables.title.clone()),
        "date" => Some(
            variables
                .timestamp
                .format_obsidian(config.date_format.trim()),
        ),
        "time" => Some(
            variables
                .timestamp
                .format_obsidian(config.time_format.trim()),
        ),
        "datetime" => Some(variables.datetime.clone()),
        "uuid" => Some(variables.uuid.clone()),
        _ => None,
    }
}

#[must_use]
pub fn template_variables_for_path(path: &str, timestamp: TemplateTimestamp) -> TemplateVariables {
    let path = Path::new(path);
    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let strings = timestamp.default_strings();

    TemplateVariables {
        title,
        date: strings.date,
        time: strings.time,
        datetime: strings.datetime,
        uuid: generated_uuid_string(),
        timestamp,
    }
}

impl TemplateTimestamp {
    #[must_use]
    pub fn current() -> Self {
        Self::from_millis(vulcan_core::current_utc_timestamp_ms())
    }

    #[must_use]
    pub fn from_millis(ms: i64) -> Self {
        let seconds = ms.div_euclid(1_000);
        let days_since_epoch = seconds.div_euclid(86_400);
        let seconds_of_day = seconds.rem_euclid(86_400);
        let hour = seconds_of_day / 3_600;
        let minute = (seconds_of_day % 3_600) / 60;
        let second = seconds_of_day % 60;
        let (year, month, day) = civil_from_days(days_since_epoch);

        Self {
            days_since_epoch,
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[must_use]
    pub fn default_strings(self) -> TimestampStrings {
        let date = self.default_date_string();
        let time = format!("{:02}:{:02}:{:02}Z", self.hour, self.minute, self.second);
        let datetime = format!("{date}T{time}");

        TimestampStrings {
            date,
            time,
            datetime,
        }
    }

    #[must_use]
    pub fn to_millis(self) -> i64 {
        (self.days_since_epoch * 86_400 + self.hour * 3_600 + self.minute * 60 + self.second)
            * 1_000
    }

    /// This timestamp's time of day moved onto `date` (`YYYY-MM-DD`).
    #[must_use]
    pub fn on_date(self, date: &str) -> Option<Self> {
        let day_ms = parse_date_like_string(date)?.div_euclid(DAY_MS) * DAY_MS;
        Some(Self::from_millis(
            day_ms + self.to_millis().rem_euclid(DAY_MS),
        ))
    }

    #[must_use]
    pub fn default_date_string(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn weekday_index(self) -> usize {
        usize::try_from((self.days_since_epoch + 4).rem_euclid(7)).unwrap_or(0)
    }

    #[must_use]
    pub fn format_obsidian(self, format: &str) -> String {
        let format = if format.is_empty() {
            "YYYY-MM-DD"
        } else {
            format
        };
        let mut rendered = String::with_capacity(format.len());
        let mut remaining = format;

        while !remaining.is_empty() {
            if let Some(token) = Self::next_obsidian_token(remaining) {
                rendered.push_str(&self.token_value(token));
                remaining = &remaining[token.len()..];
            } else if let Some(character) = remaining.chars().next() {
                rendered.push(character);
                remaining = &remaining[character.len_utf8()..];
            } else {
                break;
            }
        }

        rendered
    }

    fn next_obsidian_token(input: &str) -> Option<&'static str> {
        const TOKENS: [&str; 19] = [
            "YYYY", "dddd", "MMMM", "MMM", "ddd", "Do", "YY", "MM", "DD", "dd", "HH", "hh", "mm",
            "ss", "M", "D", "H", "h", "A",
        ];

        for token in TOKENS {
            if input.starts_with(token) {
                return Some(token);
            }
        }
        if input.starts_with('m') {
            return Some("m");
        }
        if input.starts_with('s') {
            return Some("s");
        }
        if input.starts_with('a') {
            return Some("a");
        }
        None
    }

    fn token_value(self, token: &str) -> String {
        const MONTH_NAMES: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        const MONTH_ABBREVIATIONS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        const WEEKDAY_NAMES: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        const WEEKDAY_ABBREVIATIONS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const WEEKDAY_SHORT: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

        let month_index = usize::try_from(self.month.saturating_sub(1)).unwrap_or(0);
        let weekday_index = self.weekday_index();
        let hour_12 = match self.hour % 12 {
            0 => 12,
            hour => hour,
        };

        match token {
            "YYYY" => format!("{:04}", self.year),
            "YY" => format!("{:02}", self.year.rem_euclid(100)),
            "MMMM" => MONTH_NAMES[month_index].to_string(),
            "MMM" => MONTH_ABBREVIATIONS[month_index].to_string(),
            "MM" => format!("{:02}", self.month),
            "M" => self.month.to_string(),
            "DD" => format!("{:02}", self.day),
            "Do" => ordinal_day(self.day),
            "D" => self.day.to_string(),
            "dddd" => WEEKDAY_NAMES[weekday_index].to_string(),
            "ddd" => WEEKDAY_ABBREVIATIONS[weekday_index].to_string(),
            "dd" => WEEKDAY_SHORT[weekday_index].to_string(),
            "HH" => format!("{:02}", self.hour),
            "H" => self.hour.to_string(),
            "hh" => format!("{hour_12:02}"),
            "h" => hour_12.to_string(),
            "mm" => format!("{:02}", self.minute),
            "m" => self.minute.to_string(),
            "ss" => format!("{:02}", self.second),
            "s" => self.second.to_string(),
            "A" => {
                if self.hour < 12 {
                    "AM".to_string()
                } else {
                    "PM".to_string()
                }
            }
            "a" => {
                if self.hour < 12 {
                    "am".to_string()
                } else {
                    "pm".to_string()
                }
            }
            _ => token.to_string(),
        }
    }
}

/// Format a timestamp with Moment.js tokens, as Templater does. The Dataview
/// formatter in `vulcan_core` uses Luxon tokens, where `dd` is the day of month
/// rather than a weekday, so it must not be used for `tp.*` dates.
fn format_date(ms: i64, format: &str) -> String {
    let timestamp = TemplateTimestamp::from_millis(ms);
    let format = if format.is_empty() {
        "YYYY-MM-DD"
    } else {
        format
    };
    let mut rendered = String::with_capacity(format.len());
    let mut remaining = format;
    while let Some(character) = remaining.chars().next() {
        if character == '[' {
            if let Some(end) = remaining.find(']') {
                rendered.push_str(&remaining[1..end]);
                remaining = &remaining[end + 1..];
                continue;
            }
        }
        let (value, consumed) = if remaining.starts_with("SSS") {
            (format!("{:03}", ms.rem_euclid(1_000)), 3)
        } else if character == 'x' {
            (ms.to_string(), 1)
        } else if character == 'X' {
            (ms.div_euclid(1_000).to_string(), 1)
        } else if let Some(token) = TemplateTimestamp::next_obsidian_token(remaining) {
            (timestamp.token_value(token), token.len())
        } else {
            (character.to_string(), character.len_utf8())
        };
        rendered.push_str(&value);
        remaining = &remaining[consumed..];
    }
    rendered
}

fn ordinal_day(day: i64) -> String {
    let suffix = match day.rem_euclid(100) {
        11..=13 => "th",
        _ => match day.rem_euclid(10) {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        },
    };
    format!("{day}{suffix}")
}

fn generated_uuid_string() -> String {
    let value = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        ^ u128::from(std::process::id());
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests;
