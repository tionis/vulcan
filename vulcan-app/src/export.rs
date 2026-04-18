use crate::config as app_config;
use crate::templates::TemplateTimestamp;
use crate::AppError;
use regex::Regex;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;
use vulcan_core::config::{
    ExportEpubTocStyleConfig, ExportGraphFormatConfig, ExportProfileConfig, ExportProfileFormat,
};
use vulcan_core::content_transforms::{
    ContentReplacementRuleConfig, ContentTransformConfig, ContentTransformRuleConfig,
};
use vulcan_core::{
    ensure_vulcan_dir, load_vault_config, validate_vulcan_overrides_toml, ConfigDiagnostic,
    NoteRecord, QueryReport, VaultPaths,
};

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
        build_export_profile_show_report, write_sqlite_export, BoolConfigUpdate, ConfigValueUpdate,
        ExportLinkRecord, ExportProfileCreateRequest, ExportProfileFormat,
        ExportProfileRuleMoveRequest, ExportProfileRuleRequest, ExportProfileRuleWriteAction,
        ExportProfileSetRequest, ExportProfileWriteAction, ExportedNoteDocument,
    };
    use serde_json::{Map, Value};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;
    use vulcan_core::properties::NoteTaskRecord;
    use vulcan_core::{
        EvaluatedInlineExpression, NoteRecord, QueryAst, QueryProjection, QueryReport, QuerySource,
        VaultPaths,
    };

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
