use crate::mdbase::{
    apply_managed_mdbase_note_write, apply_managed_mdbase_note_writes,
    MdbaseManagedNoteWriteBatchRequest, MdbaseManagedNoteWriteChange,
    MdbaseManagedNoteWriteRequest, MdbaseManagedWriteMode, MdbaseWriteOperation,
};
use crate::notes::{
    normalize_date_argument, normalize_note_path, render_periodic_note_contents_with_guard,
    resolve_existing_note_path, write_ordinary_note_if_unchanged_with_profile,
};
use crate::templates::{
    load_named_template, merge_template_frontmatter, parse_frontmatter_document,
    render_note_from_parts, LoadedTemplateRenderRequest, TemplateEngineKind, TemplateRunMode,
    TemplateTimestamp,
};
use crate::AppError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::Path;
use vulcan_core::config::TasksDefaultSource;
use vulcan_core::expression::eval::{evaluate as evaluate_expression, is_truthy, EvalContext};
use vulcan_core::expression::functions::{
    date_components, parse_date_like_string, parse_duration_string,
};
use vulcan_core::expression::parse_expression;
use vulcan_core::ordinary_write::{recover_ordinary_write_batch, OrdinaryWriteChange};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};
use vulcan_core::properties::extract_indexed_properties;
use vulcan_core::{
    active_tasknote_time_entry, evaluate_base_file, expected_periodic_note_path, extract_tasknote,
    inspect_base_file, load_tasks_blocks, load_vault_config, parse_tasknote_natural_language,
    parse_tasknote_reminders, parse_tasknote_time_entries, parse_tasks_query,
    period_range_for_date, resolve_note_reference, shape_tasks_query_result,
    task_upcoming_occurrences, tasknotes_default_date_value, tasknotes_default_recurrence_rule,
    tasknotes_default_reminder_values, tasknotes_reminder_notify_at, tasknotes_status_definition,
    tasknotes_status_state, BasesEvalReport, BasesEvaluator, GraphQueryError, IndexedTaskNote,
    NoteRecord, PermissionGuard, ProfilePermissionGuard, RefactorChange, TaskNotesSavedViewConfig,
    TaskNotesSavedViewFilterValue, TaskNotesSavedViewNode, TasksQueryResult, VaultConfig,
    VaultPaths,
};

mod types;
pub use types::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskPomodoroActivePeriod {
    start_time: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    end_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskPomodoroSession {
    id: String,
    start_time: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    end_time: Option<String>,
    planned_duration: usize,
    #[serde(rename = "type")]
    session_type: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    task_path: Option<String>,
    completed: bool,
    #[serde(skip_serializing_if = "is_false", default)]
    interrupted: bool,
    active_periods: Vec<TaskPomodoroActivePeriod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPomodoroSessionReport {
    pub id: String,
    pub session_type: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub planned_duration_minutes: usize,
    pub elapsed_minutes: i64,
    pub remaining_seconds: i64,
    pub completed: bool,
    pub interrupted: bool,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct TaskPomodoroStartRequest {
    pub task: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskPomodoroStopRequest {
    pub task: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPomodoroReport {
    pub action: String,
    pub dry_run: bool,
    pub storage_note_path: String,
    pub task_path: Option<String>,
    pub title: Option<String>,
    pub session: TaskPomodoroSessionReport,
    pub completed_work_sessions: usize,
    pub suggested_break_type: String,
    pub suggested_break_minutes: usize,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPomodoroStatusReport {
    pub active: Option<TaskPomodoroStatusItem>,
    pub completed_work_sessions: usize,
    pub suggested_break_type: String,
    pub suggested_break_minutes: usize,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskPomodoroStatusItem {
    pub storage_note_path: String,
    pub task_path: Option<String>,
    pub title: Option<String>,
    pub session: TaskPomodoroSessionReport,
}

#[derive(Debug, Clone)]
struct LoadedTaskNote {
    path: String,
    source: String,
    body: String,
    frontmatter: YamlMapping,
    frontmatter_json: Value,
    indexed: IndexedTaskNote,
    config: VaultConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedInlineTask {
    path: String,
    line_number: i64,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedTaskConvertLine {
    start_line: i64,
    end_line: i64,
    title_input: String,
    details: String,
    replacement_prefix: String,
    completed: bool,
}

#[derive(Debug, Clone)]
struct PlannedConvertedTaskNote {
    relative_path: String,
    title: String,
    frontmatter: YamlMapping,
    body: String,
    task_changes: Vec<RefactorChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedInlineTaskCreate {
    used_nlp: bool,
    line: String,
    due: Option<String>,
    scheduled: Option<String>,
    priority: Option<String>,
    recurrence: Option<String>,
    contexts: Vec<String>,
    projects: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NoteEntryInsertion {
    updated: String,
    line_number: i64,
    change: RefactorChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskNotesViewTarget {
    file: String,
    view_name: Option<String>,
    saved_view: Option<TaskNotesSavedViewConfig>,
}

#[derive(Debug, Clone)]
struct TaskNoteRecord {
    path: String,
    indexed: IndexedTaskNote,
    completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskDependencyReference {
    blocker_id: String,
    relation_type: Option<String>,
    gap: Option<String>,
}

#[derive(Debug, Clone)]
struct LoadedNoteMutation {
    path: String,
    source: String,
    body: String,
    frontmatter: YamlMapping,
    created: bool,
}

#[derive(Debug, Clone)]
struct StoredPomodoroSession {
    storage_note_path: String,
    task_path: Option<String>,
    title: Option<String>,
    session: TaskPomodoroSession,
}

#[derive(Debug)]
struct TaskMutationPlan {
    changes: Vec<RefactorChange>,
    moved_to: Option<String>,
}

pub fn apply_task_set(
    paths: &VaultPaths,
    request: &TaskSetRequest,
) -> Result<TaskMutationReport, AppError> {
    apply_task_set_with_guard(paths, request, None)
}

pub fn apply_task_set_with_guard(
    paths: &VaultPaths,
    request: &TaskSetRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskMutationReport, AppError> {
    apply_tasknote_mutation(
        paths,
        &request.task,
        "set",
        request.dry_run,
        guard,
        |frontmatter, loaded| {
            let key = tasknote_frontmatter_key(&loaded.config, &request.property);
            let parsed = parse_tasknote_cli_value(&request.value);
            let mut changes = Vec::new();
            let value = (!matches!(parsed, YamlValue::Null)).then_some(parsed.clone());
            if let Some(change) = set_tasknote_frontmatter_value(frontmatter, &key, value.clone()) {
                changes.push(change);
            }

            if key == loaded.config.tasknotes.field_mapping.status
                && loaded.indexed.recurrence.is_none()
            {
                let next_status = value.as_ref().and_then(yaml_string).unwrap_or_default();
                let completed_key = &loaded.config.tasknotes.field_mapping.completed_date;
                let completed_value =
                    if tasknotes_status_state(&loaded.config.tasknotes, &next_status).completed {
                        Some(YamlValue::String(current_utc_date_string()))
                    } else {
                        None
                    };
                if let Some(change) =
                    set_tasknote_frontmatter_value(frontmatter, completed_key, completed_value)
                {
                    changes.push(change);
                }
            }

            let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
            if let Some(change) = set_tasknote_frontmatter_value(
                frontmatter,
                modified_key,
                Some(YamlValue::String(current_utc_timestamp_string())),
            ) {
                changes.push(change);
            }

            Ok(TaskMutationPlan {
                changes,
                moved_to: None,
            })
        },
    )
}

pub fn apply_task_reschedule(
    paths: &VaultPaths,
    request: &TaskRescheduleRequest,
) -> Result<TaskMutationReport, AppError> {
    apply_task_reschedule_with_guard(paths, request, None)
}

pub fn apply_task_reschedule_with_guard(
    paths: &VaultPaths,
    request: &TaskRescheduleRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskMutationReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    if let Ok(loaded) = load_tasknote_note_with_guard(paths, &request.task, guard) {
        let due_value = resolve_tasknote_date_input(&loaded.config, &request.due, false)?;
        return apply_loaded_tasknote_mutation_with_guard(
            paths,
            &loaded,
            "reschedule",
            request.dry_run,
            guard,
            |frontmatter, loaded| {
                let mut changes = Vec::new();
                let due_key = &loaded.config.tasknotes.field_mapping.due;
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    due_key,
                    Some(YamlValue::String(due_value.clone())),
                ) {
                    changes.push(change);
                }

                let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    modified_key,
                    Some(YamlValue::String(current_utc_timestamp_string())),
                ) {
                    changes.push(change);
                }

                Ok(TaskMutationPlan {
                    changes,
                    moved_to: None,
                })
            },
        );
    }

    apply_inline_task_reschedule(paths, request, guard)
}

pub fn apply_task_complete(
    paths: &VaultPaths,
    request: &TaskCompleteRequest,
) -> Result<TaskMutationReport, AppError> {
    apply_task_complete_with_guard(paths, request, None)
}

pub fn apply_task_complete_with_guard(
    paths: &VaultPaths,
    request: &TaskCompleteRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskMutationReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    if let Ok(loaded) = load_tasknote_note_with_guard(paths, &request.task, guard) {
        return apply_loaded_tasknote_mutation_with_guard(
            paths,
            &loaded,
            "complete",
            request.dry_run,
            guard,
            |frontmatter, loaded| {
                let mut changes = Vec::new();
                if loaded.indexed.recurrence.is_some() {
                    let target_date = match request.date.as_deref() {
                        Some(value) => normalize_date_argument(Some(value))?,
                        None => loaded
                            .indexed
                            .scheduled
                            .as_deref()
                            .or(loaded.indexed.due.as_deref())
                            .map(|value| normalize_date_argument(Some(value)))
                            .transpose()?
                            .unwrap_or_else(current_utc_date_string),
                    };

                    let complete_key = &loaded.config.tasknotes.field_mapping.complete_instances;
                    let skipped_key = &loaded.config.tasknotes.field_mapping.skipped_instances;
                    let complete_yaml_key = YamlValue::String(complete_key.clone());
                    let mut complete_instances =
                        yaml_string_list(frontmatter.get(&complete_yaml_key));
                    if !complete_instances.iter().any(|entry| entry == &target_date) {
                        complete_instances.push(target_date.clone());
                        complete_instances.sort();
                    }
                    if let Some(change) = set_tasknote_frontmatter_value(
                        frontmatter,
                        complete_key,
                        Some(YamlValue::Sequence(
                            complete_instances
                                .iter()
                                .cloned()
                                .map(YamlValue::String)
                                .collect(),
                        )),
                    ) {
                        changes.push(change);
                    }

                    let skipped_yaml_key = YamlValue::String(skipped_key.clone());
                    let skipped_instances = yaml_string_list(frontmatter.get(&skipped_yaml_key))
                        .into_iter()
                        .filter(|entry| entry != &target_date)
                        .collect::<Vec<_>>();
                    let skipped_value = if skipped_instances.is_empty() {
                        None
                    } else {
                        Some(YamlValue::Sequence(
                            skipped_instances
                                .into_iter()
                                .map(YamlValue::String)
                                .collect(),
                        ))
                    };
                    if let Some(change) =
                        set_tasknote_frontmatter_value(frontmatter, skipped_key, skipped_value)
                    {
                        changes.push(change);
                    }
                } else {
                    let status_key = &loaded.config.tasknotes.field_mapping.status;
                    if let Some(change) = set_tasknote_frontmatter_value(
                        frontmatter,
                        status_key,
                        Some(YamlValue::String(first_completed_tasknote_status(
                            &loaded.config,
                        ))),
                    ) {
                        changes.push(change);
                    }
                    let completed_key = &loaded.config.tasknotes.field_mapping.completed_date;
                    if let Some(change) = set_tasknote_frontmatter_value(
                        frontmatter,
                        completed_key,
                        Some(YamlValue::String(current_utc_date_string())),
                    ) {
                        changes.push(change);
                    }
                }

                let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    modified_key,
                    Some(YamlValue::String(current_utc_timestamp_string())),
                ) {
                    changes.push(change);
                }

                Ok(TaskMutationPlan {
                    changes,
                    moved_to: None,
                })
            },
        );
    }

    apply_inline_task_complete(paths, request, guard)
}

pub fn apply_task_archive(
    paths: &VaultPaths,
    request: &TaskArchiveRequest,
) -> Result<TaskMutationReport, AppError> {
    apply_task_archive_with_guard(paths, request, None)
}

pub fn apply_task_archive_with_guard(
    paths: &VaultPaths,
    request: &TaskArchiveRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskMutationReport, AppError> {
    apply_tasknote_mutation(
        paths,
        &request.task,
        "archive",
        request.dry_run,
        guard,
        prepare_tasknote_archive_plan,
    )
}

pub fn process_due_tasknote_auto_archives(
    paths: &VaultPaths,
    exclude_task: Option<&str>,
) -> Result<Vec<String>, AppError> {
    process_due_tasknote_auto_archives_with_guard(paths, exclude_task, None)
}

pub fn process_due_tasknote_auto_archives_with_guard(
    paths: &VaultPaths,
    exclude_task: Option<&str>,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<Vec<String>, AppError> {
    // Read-only callers must be able to inspect tasks without entering a mutation
    // boundary or requiring access to MDB routing controls for automatic upkeep.
    if guard.is_some_and(|guard| guard.grant().write.allow.is_empty()) {
        return Ok(Vec::new());
    }
    preflight_task_mutation_guard(paths, guard)?;
    if guard.is_none() {
        recover_pending_ordinary_task_write(paths)?;
    }
    let config = load_vault_config(paths).config;
    let now_ms = current_utc_timestamp_ms();
    let excluded_path = exclude_task
        .and_then(|task| load_tasknote_note_with_guard(paths, task, guard).ok())
        .map(|loaded| loaded.path);
    let read_filter = guard.map(PermissionGuard::read_filter);
    let candidates = load_tasknote_records_with_filter(paths, read_filter.as_ref())?
        .into_iter()
        .filter(|record| excluded_path.as_ref() != Some(&record.path))
        .filter(|record| {
            if record.indexed.archived {
                return false;
            }

            let Some(status) =
                tasknotes_status_definition(&config.tasknotes, &record.indexed.status)
            else {
                return false;
            };
            if !status.is_completed || !status.auto_archive {
                return false;
            }

            let completed_at = record
                .indexed
                .completed_date
                .as_deref()
                .and_then(parse_date_like_string)
                .unwrap_or_default();
            if completed_at <= 0 {
                return false;
            }

            let delay_ms = i64::try_from(status.auto_archive_delay)
                .unwrap_or(i64::MAX)
                .saturating_mul(60_000);
            now_ms >= completed_at.saturating_add(delay_ms)
        })
        .map(|record| record.path)
        .collect::<Vec<_>>();
    let mut changed_paths = Vec::new();

    for path in candidates {
        let loaded = load_tasknote_note_with_guard(paths, &path, guard)?;
        let report = apply_loaded_tasknote_mutation_with_guard(
            paths,
            &loaded,
            "auto_archive",
            false,
            guard,
            prepare_tasknote_archive_plan,
        )?;
        changed_paths.extend(report.changed_paths);
    }

    changed_paths.sort();
    changed_paths.dedup();
    Ok(changed_paths)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn apply_task_add(
    paths: &VaultPaths,
    request: &TaskAddRequest,
) -> Result<TaskAddReport, AppError> {
    apply_task_add_with_guard(paths, request, None)
}

#[allow(clippy::too_many_lines)]
pub fn apply_task_add_with_guard(
    paths: &VaultPaths,
    request: &TaskAddRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskAddReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    let config = load_vault_config(paths).config;
    let reference_ms = tasknote_reference_ms();
    let raw_title = request.text.trim();
    if raw_title.is_empty() {
        return Err(AppError::operation("task text cannot be empty"));
    }

    let used_nlp = config.tasknotes.enable_natural_language_input && !request.no_nlp;
    let parsed_input = used_nlp
        .then(|| parse_tasknote_natural_language(raw_title, &config.tasknotes, reference_ms));
    let title = parsed_input
        .as_ref()
        .map(|parsed| parsed.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(raw_title)
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(AppError::operation("task title cannot be empty"));
    }

    let status = request
        .status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            parsed_input
                .as_ref()
                .and_then(|parsed| parsed.status.clone())
        })
        .unwrap_or_else(|| config.tasknotes.default_status.clone());
    let priority = request
        .priority
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            parsed_input
                .as_ref()
                .and_then(|parsed| parsed.priority.clone())
        })
        .unwrap_or_else(|| config.tasknotes.default_priority.clone());
    let due = match request.due.as_deref() {
        Some(value) => Some(resolve_tasknote_date_input(&config, value, false)?),
        None => parsed_input
            .as_ref()
            .and_then(|parsed| parsed.due.clone())
            .or_else(|| {
                tasknotes_default_date_value(
                    config.tasknotes.task_creation_defaults.default_due_date,
                    reference_ms,
                )
            }),
    };
    let scheduled = match request.scheduled.as_deref() {
        Some(value) => Some(resolve_tasknote_date_input(&config, value, true)?),
        None => parsed_input
            .as_ref()
            .and_then(|parsed| parsed.scheduled.clone())
            .or_else(|| {
                tasknotes_default_date_value(
                    config
                        .tasknotes
                        .task_creation_defaults
                        .default_scheduled_date,
                    reference_ms,
                )
            }),
    };
    let contexts = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_contexts
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.contexts.iter().cloned()),
            )
            .chain(request.contexts.iter().cloned())
            .collect::<Vec<_>>(),
        normalize_tasknote_context,
    );
    let projects = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_projects
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.projects.iter().cloned()),
            )
            .chain(request.projects.iter().cloned())
            .collect::<Vec<_>>(),
        normalize_tasknote_project,
    );
    let mut tags = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_tags
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.tags.iter().cloned()),
            )
            .chain(request.tags.iter().cloned())
            .collect::<Vec<_>>(),
        normalize_tasknote_tag,
    );
    if config.tasknotes.identification_method == vulcan_core::TaskNotesIdentificationMethod::Tag {
        if let Some(task_tag) = normalize_tasknote_tag(&config.tasknotes.task_tag) {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&task_tag))
            {
                tags.insert(0, task_tag);
            }
        }
    }
    let time_estimate = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.time_estimate)
        .or(config
            .tasknotes
            .task_creation_defaults
            .default_time_estimate);
    let recurrence = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.recurrence.clone())
        .or_else(|| {
            tasknotes_default_recurrence_rule(
                config.tasknotes.task_creation_defaults.default_recurrence,
            )
        });

    let relative_path = format!(
        "{}/{}.md",
        config.tasknotes.tasks_folder.trim_end_matches('/'),
        sanitize_tasknote_filename(&title)
    );
    let absolute_path = paths.vault_root().join(&relative_path);
    if let Some(guard) = guard {
        guard
            .check_read_path(&relative_path)
            .map_err(AppError::operation)?;
        guard
            .check_write_path(&relative_path)
            .map_err(AppError::operation)?;
    }
    if absolute_path.exists() {
        return Err(AppError::operation(format!(
            "destination task already exists: {relative_path}"
        )));
    }

    let timestamp = current_utc_timestamp_string();
    let mapping = &config.tasknotes.field_mapping;
    let mut frontmatter = YamlMapping::new();
    frontmatter.insert(
        YamlValue::String(mapping.title.clone()),
        YamlValue::String(title.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.status.clone()),
        YamlValue::String(status.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.priority.clone()),
        YamlValue::String(priority.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.date_created.clone()),
        YamlValue::String(timestamp.clone()),
    );
    frontmatter.insert(
        YamlValue::String(mapping.date_modified.clone()),
        YamlValue::String(timestamp),
    );
    if let Some(due) = due.as_ref() {
        frontmatter.insert(
            YamlValue::String(mapping.due.clone()),
            YamlValue::String(due.clone()),
        );
    }
    if let Some(scheduled) = scheduled.as_ref() {
        frontmatter.insert(
            YamlValue::String(mapping.scheduled.clone()),
            YamlValue::String(scheduled.clone()),
        );
    }
    if !contexts.is_empty() {
        frontmatter.insert(
            YamlValue::String(mapping.contexts.clone()),
            yaml_string_sequence(&contexts),
        );
    }
    if !projects.is_empty() {
        frontmatter.insert(
            YamlValue::String(mapping.projects.clone()),
            yaml_string_sequence(&projects),
        );
    }
    if !tags.is_empty() {
        frontmatter.insert(
            YamlValue::String("tags".to_string()),
            yaml_string_sequence(&tags),
        );
    }
    if let Some(time_estimate) = time_estimate {
        frontmatter.insert(
            YamlValue::String(mapping.time_estimate.clone()),
            YamlValue::Number(serde_yaml::Number::from(time_estimate as u64)),
        );
    }
    if let Some(recurrence) = recurrence.as_ref() {
        frontmatter.insert(
            YamlValue::String(mapping.recurrence.clone()),
            YamlValue::String(recurrence.clone()),
        );
    }
    if let Some(reminders) = default_tasknote_reminders_yaml_value(&config)? {
        frontmatter.insert(YamlValue::String(mapping.reminders.clone()), reminders);
    }
    if config.tasknotes.identification_method
        == vulcan_core::TaskNotesIdentificationMethod::Property
    {
        if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
            let value = config
                .tasknotes
                .task_property_value
                .as_ref()
                .map_or(YamlValue::Bool(true), |value| {
                    YamlValue::String(value.clone())
                });
            frontmatter.insert(YamlValue::String(property_name.clone()), value);
        }
    }

    let (template_frontmatter, template_body) = match request.template.as_deref() {
        Some(template_name) => load_tasknote_template(
            paths,
            &config,
            template_name,
            &relative_path,
            request.dry_run,
            guard,
        )?,
        None => (None, String::new()),
    };
    let merged_frontmatter =
        merge_template_frontmatter(Some(frontmatter), template_frontmatter).unwrap_or_default();
    let rendered = render_note_from_parts(Some(&merged_frontmatter), &template_body)
        .map_err(AppError::operation)?;
    let frontmatter_json = tasknote_frontmatter_json(&merged_frontmatter);

    let routed = route_task_note_write(
        paths,
        &relative_path,
        None,
        Some(&rendered),
        MdbaseWriteOperation::Create,
        request.dry_run,
        guard,
    )?;
    if !request.dry_run && !routed {
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &relative_path,
            None,
            &rendered,
            "task add",
            task_mutation_profile(paths, guard)?,
        )?;
    }

    Ok(TaskAddReport {
        action: "add".to_string(),
        dry_run: request.dry_run,
        created: !request.dry_run,
        used_nlp,
        path: relative_path.clone(),
        title,
        status,
        priority,
        due,
        scheduled,
        contexts,
        projects,
        tags,
        time_estimate,
        recurrence,
        template: request.template.clone(),
        frontmatter: frontmatter_json,
        body: template_body,
        parsed_input,
        changed_paths: vec![relative_path],
    })
}

pub fn apply_task_create(
    paths: &VaultPaths,
    request: &TaskCreateRequest,
) -> Result<TaskCreateReport, AppError> {
    apply_task_create_with_guard(paths, request, None)
}

pub fn apply_task_create_with_guard(
    paths: &VaultPaths,
    request: &TaskCreateRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskCreateReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    let config = load_vault_config(paths).config;
    let (relative_path, heading) = if let Some(guard) = guard {
        let (target, heading) = request.note.as_deref().map_or_else(
            || (config.inbox.path.as_str(), config.inbox.heading.clone()),
            |note| (note, None),
        );
        (
            resolve_task_mutation_path(paths, target, Some(guard))?,
            heading,
        )
    } else {
        resolve_tasks_create_target(paths, request.note.as_deref())?
    };
    let absolute_path = paths.vault_root().join(&relative_path);
    if absolute_path.exists() && !absolute_path.is_file() {
        return Err(AppError::operation(format!(
            "target note is not a file: {relative_path}"
        )));
    }

    let existing = crate::notes::read_note_for_update(paths, &relative_path)?;
    let created_note = existing.is_none();
    let existing = existing.unwrap_or_default();
    let planned = build_inline_task_create_plan(
        &config,
        &request.text,
        request.due.as_deref(),
        request.priority.as_deref(),
    )?;
    let insertion = append_entry_to_note(&existing, &planned.line, heading.as_deref());
    let task = format!("{}:{}", relative_path, insertion.line_number);
    let changed_paths = vec![relative_path.clone()];
    if let Some(guard) = guard {
        guard
            .check_write_path(&relative_path)
            .map_err(AppError::operation)?;
    }

    let operation = if created_note {
        MdbaseWriteOperation::Create
    } else {
        MdbaseWriteOperation::Update
    };
    let routed = route_task_note_write(
        paths,
        &relative_path,
        (!created_note).then_some(existing.as_str()),
        Some(&insertion.updated),
        operation,
        request.dry_run,
        guard,
    )?;
    if !request.dry_run && !routed {
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &relative_path,
            (!created_note).then_some(existing.as_str()),
            &insertion.updated,
            "task create",
            task_mutation_profile(paths, guard)?,
        )?;
    }

    Ok(TaskCreateReport {
        action: "create".to_string(),
        dry_run: request.dry_run,
        path: relative_path,
        task,
        created_note,
        line_number: insertion.line_number,
        used_nlp: planned.used_nlp,
        line: planned.line,
        due: planned.due,
        scheduled: planned.scheduled,
        priority: planned.priority,
        recurrence: planned.recurrence,
        contexts: planned.contexts,
        projects: planned.projects,
        tags: planned.tags,
        changes: vec![insertion.change],
        changed_paths,
    })
}

pub fn apply_task_convert(
    paths: &VaultPaths,
    request: &TaskConvertRequest,
) -> Result<TaskConvertReport, AppError> {
    apply_task_convert_with_guard(paths, request, None)
}

pub fn apply_task_convert_with_guard(
    paths: &VaultPaths,
    request: &TaskConvertRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskConvertReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    if let Some(line_number) = request.line {
        return apply_task_convert_line(paths, &request.file, line_number, request.dry_run, guard);
    }

    let config = load_vault_config(paths).config;
    let (relative_path, source) = read_existing_note_source(paths, &request.file, guard)?;
    let (frontmatter, body) =
        parse_frontmatter_document(&source, false).map_err(AppError::operation)?;
    let mut frontmatter = frontmatter.unwrap_or_default();
    let title_hint = tasknote_title_from_path(&relative_path);
    let frontmatter_json = tasknote_frontmatter_json(&frontmatter);
    if extract_tasknote(
        &relative_path,
        &title_hint,
        &frontmatter_json,
        &config.tasknotes,
    )
    .is_some()
    {
        return Err(AppError::operation(format!(
            "note is already a TaskNotes task: {relative_path}"
        )));
    }

    let task_changes =
        prepare_existing_note_tasknote_frontmatter(&mut frontmatter, &title_hint, &config);
    let frontmatter_json = tasknote_frontmatter_json(&frontmatter);
    let indexed = extract_tasknote(
        &relative_path,
        &title_hint,
        &frontmatter_json,
        &config.tasknotes,
    )
    .ok_or_else(|| AppError::operation("failed to convert note into a TaskNotes task"))?;
    let rendered =
        render_note_from_parts(Some(&frontmatter), &body).map_err(AppError::operation)?;
    let changed_paths = if task_changes.is_empty() {
        Vec::new()
    } else {
        vec![relative_path.clone()]
    };

    let routed = if task_changes.is_empty() {
        false
    } else {
        route_task_note_write(
            paths,
            &relative_path,
            Some(&source),
            Some(&rendered),
            MdbaseWriteOperation::Update,
            request.dry_run,
            guard,
        )?
    };
    if !request.dry_run && !task_changes.is_empty() && !routed {
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &relative_path,
            Some(&source),
            &rendered,
            "task conversion",
            task_mutation_profile(paths, guard)?,
        )?;
    }

    Ok(TaskConvertReport {
        action: "convert".to_string(),
        dry_run: request.dry_run,
        mode: "note".to_string(),
        source_path: relative_path.clone(),
        target_path: relative_path,
        line_number: None,
        title: indexed.title,
        created: false,
        source_changes: Vec::new(),
        task_changes,
        frontmatter: frontmatter_json,
        body,
        changed_paths,
    })
}

/// Authorize a direct external-editor handoff, not a validated collection write.
/// The caller must rescan the editor's actual bytes after the process exits.
pub fn prepare_task_editor_path(
    paths: &VaultPaths,
    task: &str,
    guard: &ProfilePermissionGuard,
) -> Result<String, AppError> {
    preflight_task_mutation_guard(paths, Some(guard))?;
    guard.check_execute().map_err(AppError::operation)?;
    let _read_guard = consistent_task_read(paths)?;
    Ok(load_tasknote_note_with_guard(paths, task, Some(guard))?.path)
}

pub fn build_task_show_report(paths: &VaultPaths, task: &str) -> Result<TaskShowReport, AppError> {
    let _read_guard = consistent_task_read(paths)?;
    let loaded = load_tasknote_note(paths, task)?;
    Ok(task_show_report(loaded))
}

pub fn build_task_show_report_with_guard(
    paths: &VaultPaths,
    task: &str,
    guard: &ProfilePermissionGuard,
) -> Result<TaskShowReport, AppError> {
    TaskReadScope::Guard(guard).recheck(paths)?;
    let snapshot = guard.snapshot_read_policy().map_err(AppError::operation)?;
    let scope = TaskReadScope::Guard(&snapshot);
    let _read_guard = consistent_task_read(paths)?;
    let result = (|| {
        scope.recheck(paths)?;
        let loaded = load_tasknote_for_read(paths, task, &snapshot)?;
        Ok(task_show_report(loaded))
    })();
    scope.recheck(paths)?;
    result
}

fn load_tasknote_for_read(
    paths: &VaultPaths,
    task: &str,
    guard: &ProfilePermissionGuard,
) -> Result<LoadedTaskNote, AppError> {
    let path = resolve_task_read_path(paths, task, guard)?;
    let source = read_task_source(paths, &path).map_err(AppError::operation)?;
    // Source bytes are canonical; do not borrow mutation authority or cached
    // properties, and recheck tag grants against the current source.
    parse_loaded_tasknote(paths, task, path, source, None, Some(&guard.read_filter()))
}

fn resolve_task_read_path(
    paths: &VaultPaths,
    task: &str,
    guard: &ProfilePermissionGuard,
) -> Result<String, AppError> {
    match vulcan_core::graph::resolve_note_reference_with_guard(paths, task, guard) {
        Ok(note) => Ok(note.path),
        Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. })
            if !guard.has_policy_hook()
                && !guard
                    .grant()
                    .read
                    .allow
                    .iter()
                    .chain(&guard.grant().read.deny)
                    .any(|specifier| {
                        matches!(
                            specifier,
                            vulcan_core::permissions::ResourceSpecifier::Tag(_)
                        )
                    }) =>
        {
            // Preserve direct-path reads without a populated cache only when
            // static path rules alone prove access. Never retry a policy denial
            // or infer tag authority from an unread indexed/source document.
            let path = normalize_note_path(task)?;
            guard.check_read_path(&path).map_err(AppError::operation)?;
            Ok(path)
        }
        Err(error) => Err(AppError::operation(error)),
    }
}

fn task_show_report(loaded: LoadedTaskNote) -> TaskShowReport {
    let status_state = tasknotes_status_state(&loaded.config.tasknotes, &loaded.indexed.status);
    let now_ms = current_utc_timestamp_ms();
    let (total_time_minutes, active_time_minutes, estimate_remaining_minutes, efficiency_ratio) =
        tasknote_time_metrics(&loaded.indexed, now_ms);

    TaskShowReport {
        path: loaded.path,
        title: loaded.indexed.title,
        status: loaded.indexed.status,
        status_type: status_state.status_type,
        completed: status_state.completed,
        archived: loaded.indexed.archived,
        priority: loaded.indexed.priority,
        due: loaded.indexed.due,
        scheduled: loaded.indexed.scheduled,
        completed_date: loaded.indexed.completed_date,
        date_created: loaded.indexed.date_created,
        date_modified: loaded.indexed.date_modified,
        contexts: loaded.indexed.contexts,
        projects: loaded.indexed.projects,
        tags: loaded.indexed.tags,
        recurrence: loaded.indexed.recurrence,
        recurrence_anchor: loaded.indexed.recurrence_anchor,
        complete_instances: loaded.indexed.complete_instances,
        skipped_instances: loaded.indexed.skipped_instances,
        blocked_by: loaded.indexed.blocked_by,
        reminders: loaded.indexed.reminders,
        time_entries: loaded.indexed.time_entries,
        total_time_minutes,
        active_time_minutes,
        estimate_remaining_minutes,
        efficiency_ratio,
        custom_fields: Value::Object(loaded.indexed.custom_fields),
        frontmatter: loaded.frontmatter_json,
        body: loaded.body,
    }
}

pub fn apply_task_track_start(
    paths: &VaultPaths,
    request: &TaskTrackStartRequest,
) -> Result<TaskTrackReport, AppError> {
    apply_task_track_start_with_guard(paths, request, None)
}

pub fn apply_task_track_start_with_guard(
    paths: &VaultPaths,
    request: &TaskTrackStartRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskTrackReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    let loaded = load_tasknote_note_with_guard(paths, &request.task, guard)?;
    let now_ms = current_utc_timestamp_ms();
    if active_tasknote_time_entry(&loaded.indexed.time_entries, now_ms).is_some() {
        return Err(AppError::operation(format!(
            "time tracking is already active for {}",
            loaded.path
        )));
    }

    let start_time = current_utc_timestamp_string();
    let session_description = request
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .unwrap_or("Work session")
        .to_string();

    let report = apply_loaded_tasknote_mutation_with_guard(
        paths,
        &loaded,
        "track_start",
        request.dry_run,
        guard,
        |frontmatter, loaded| {
            let key = &loaded.config.tasknotes.field_mapping.time_entries;
            let yaml_key = YamlValue::String(key.clone());
            let mut entries = yaml_sequence_value(frontmatter.get(&yaml_key))?;
            entries.push(tasknote_time_entry_yaml_value(
                &start_time,
                None,
                Some(&session_description),
            ));

            let mut changes = Vec::new();
            if let Some(change) =
                set_tasknote_frontmatter_value(frontmatter, key, Some(YamlValue::Sequence(entries)))
            {
                changes.push(change);
            }
            if let Some(change) = set_tasknote_frontmatter_value(
                frontmatter,
                &loaded.config.tasknotes.field_mapping.date_modified,
                Some(YamlValue::String(current_utc_timestamp_string())),
            ) {
                changes.push(change);
            }

            Ok(TaskMutationPlan {
                changes,
                moved_to: None,
            })
        },
    )?;

    let mut updated_entries = loaded.indexed.time_entries.clone();
    updated_entries.push(serde_json::json!({
        "startTime": start_time,
        "description": session_description,
    }));
    let session = active_tasknote_time_entry(&updated_entries, now_ms)
        .map(task_time_entry_report)
        .ok_or_else(|| AppError::operation("failed to resolve the started time entry"))?;
    let updated_task = IndexedTaskNote {
        time_entries: updated_entries,
        ..loaded.indexed.clone()
    };
    let (total_time_minutes, active_time_minutes, estimate_remaining_minutes, efficiency_ratio) =
        tasknote_time_metrics(&updated_task, now_ms);

    Ok(TaskTrackReport {
        action: "start".to_string(),
        dry_run: request.dry_run,
        path: loaded.path,
        title: loaded.indexed.title,
        session,
        total_time_minutes,
        active_time_minutes,
        estimate_remaining_minutes,
        efficiency_ratio,
        changed_paths: report.changed_paths,
    })
}

pub fn apply_task_track_stop(
    paths: &VaultPaths,
    request: &TaskTrackStopRequest,
) -> Result<TaskTrackReport, AppError> {
    apply_task_track_stop_with_guard(paths, request, None)
}

pub fn apply_task_track_stop_with_guard(
    paths: &VaultPaths,
    request: &TaskTrackStopRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskTrackReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    let now_ms = current_utc_timestamp_ms();
    let record = resolve_active_tasknote_record(paths, request.task.as_deref(), now_ms, guard)?;
    let loaded = load_tasknote_note_with_guard(paths, &record.path, guard)?;
    let active_entry = active_tasknote_time_entry(&loaded.indexed.time_entries, now_ms)
        .ok_or_else(|| AppError::operation(format!("no active session for {}", loaded.path)))?;
    let stop_time = current_utc_timestamp_string();

    let report = apply_loaded_tasknote_mutation_with_guard(
        paths,
        &loaded,
        "track_stop",
        request.dry_run,
        guard,
        |frontmatter, loaded| {
            let key = &loaded.config.tasknotes.field_mapping.time_entries;
            let yaml_key = YamlValue::String(key.clone());
            let mut entries = yaml_sequence_value(frontmatter.get(&yaml_key))?;
            let mut updated = false;

            for entry in entries.iter_mut().rev() {
                let Some(mapping) = entry.as_mapping_mut() else {
                    continue;
                };
                let start_matches = mapping
                    .get(YamlValue::String("startTime".to_string()))
                    .and_then(YamlValue::as_str)
                    .is_some_and(|value| value == active_entry.start_time);
                let has_end = mapping
                    .get(YamlValue::String("endTime".to_string()))
                    .is_some();
                if start_matches && !has_end {
                    mapping.insert(
                        YamlValue::String("endTime".to_string()),
                        YamlValue::String(stop_time.clone()),
                    );
                    updated = true;
                    break;
                }
            }

            if !updated {
                return Err(AppError::operation(format!(
                    "failed to locate the active time entry in {}",
                    loaded.path
                )));
            }

            let mut changes = Vec::new();
            if let Some(change) =
                set_tasknote_frontmatter_value(frontmatter, key, Some(YamlValue::Sequence(entries)))
            {
                changes.push(change);
            }
            if let Some(change) = set_tasknote_frontmatter_value(
                frontmatter,
                &loaded.config.tasknotes.field_mapping.date_modified,
                Some(YamlValue::String(current_utc_timestamp_string())),
            ) {
                changes.push(change);
            }

            Ok(TaskMutationPlan {
                changes,
                moved_to: None,
            })
        },
    )?;

    let mut updated_entries = loaded.indexed.time_entries.clone();
    if let Some(entry) = updated_entries.iter_mut().rev().find(|entry| {
        entry
            .get("startTime")
            .and_then(Value::as_str)
            .is_some_and(|value| value == active_entry.start_time)
            && entry.get("endTime").is_none()
    }) {
        if let Some(object) = entry.as_object_mut() {
            object.insert("endTime".to_string(), Value::String(stop_time.clone()));
        }
    }
    let stopped_entry = parse_tasknote_time_entries(&updated_entries, now_ms)
        .into_iter()
        .find(|entry| entry.start_time == active_entry.start_time)
        .map(task_time_entry_report)
        .ok_or_else(|| AppError::operation("failed to resolve the stopped time entry"))?;
    let updated_task = IndexedTaskNote {
        time_entries: updated_entries,
        ..loaded.indexed.clone()
    };
    let (total_time_minutes, active_time_minutes, estimate_remaining_minutes, efficiency_ratio) =
        tasknote_time_metrics(&updated_task, now_ms);

    Ok(TaskTrackReport {
        action: "stop".to_string(),
        dry_run: request.dry_run,
        path: loaded.path,
        title: loaded.indexed.title,
        session: stopped_entry,
        total_time_minutes,
        active_time_minutes,
        estimate_remaining_minutes,
        efficiency_ratio,
        changed_paths: report.changed_paths,
    })
}

pub fn build_task_track_status_report(
    paths: &VaultPaths,
) -> Result<TaskTrackStatusReport, AppError> {
    build_task_track_status_report_with_guard(paths, None)
}

pub fn build_task_track_status_report_with_guard(
    paths: &VaultPaths,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskTrackStatusReport, AppError> {
    with_task_read_scope(paths, guard, |scope| {
        Ok(task_track_status_report(load_tasknote_records_with_scope(
            paths, scope,
        )?))
    })
}

fn task_track_status_report(records: Vec<TaskNoteRecord>) -> TaskTrackStatusReport {
    let now_ms = current_utc_timestamp_ms();
    let mut active_sessions = records
        .into_iter()
        .filter_map(|record| {
            let session = active_tasknote_time_entry(&record.indexed.time_entries, now_ms)?;
            Some(TaskTrackStatusItem {
                path: record.path,
                title: record.indexed.title,
                status: record.indexed.status,
                priority: record.indexed.priority,
                session: task_time_entry_report(session),
            })
        })
        .collect::<Vec<_>>();
    active_sessions.sort_by(|left, right| {
        left.session
            .start_time
            .cmp(&right.session.start_time)
            .then_with(|| left.path.cmp(&right.path))
    });
    let total_elapsed_minutes = active_sessions
        .iter()
        .map(|item| item.session.duration_minutes)
        .sum();

    TaskTrackStatusReport {
        total_active_sessions: active_sessions.len(),
        total_elapsed_minutes,
        active_sessions,
    }
}

pub fn build_task_due_report(paths: &VaultPaths, within: &str) -> Result<TaskDueReport, AppError> {
    build_task_due_report_with_guard(paths, within, None)
}

pub fn build_task_due_report_with_guard(
    paths: &VaultPaths,
    within: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskDueReport, AppError> {
    with_task_read_scope(paths, guard, |scope| {
        task_due_report(load_tasknote_records_with_scope(paths, scope)?, within)
    })
}

fn task_due_report(records: Vec<TaskNoteRecord>, within: &str) -> Result<TaskDueReport, AppError> {
    let window_ms = parse_duration_string(within).ok_or_else(|| {
        AppError::operation(format!("failed to parse due window duration: {within}"))
    })?;
    let now_ms = current_utc_timestamp_ms();
    let deadline_ms = now_ms.saturating_add(window_ms.max(0));
    let mut tasks = records
        .into_iter()
        .filter(|record| !record.indexed.archived && !record.completed)
        .filter_map(|record| {
            let due = record.indexed.due.as_ref()?;
            let due_ms = parse_date_like_string(due)?;
            (due_ms <= deadline_ms).then_some(TaskDueItem {
                path: record.path,
                title: record.indexed.title,
                status: record.indexed.status,
                priority: record.indexed.priority,
                due: due.clone(),
                overdue: due_ms < now_ms,
            })
        })
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        let left_due = parse_date_like_string(&left.due).unwrap_or(i64::MAX);
        let right_due = parse_date_like_string(&right.due).unwrap_or(i64::MAX);
        left_due
            .cmp(&right_due)
            .then_with(|| left.path.cmp(&right.path))
    });

    Ok(TaskDueReport {
        reference_time: current_utc_timestamp_string(),
        within: within.to_string(),
        tasks,
    })
}

pub fn build_task_reminders_report(
    paths: &VaultPaths,
    upcoming: &str,
) -> Result<TaskRemindersReport, AppError> {
    build_task_reminders_report_with_guard(paths, upcoming, None)
}

pub fn build_task_reminders_report_with_guard(
    paths: &VaultPaths,
    upcoming: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskRemindersReport, AppError> {
    with_task_read_scope(paths, guard, |scope| {
        task_reminders_report(load_tasknote_records_with_scope(paths, scope)?, upcoming)
    })
}

fn task_reminders_report(
    records: Vec<TaskNoteRecord>,
    upcoming: &str,
) -> Result<TaskRemindersReport, AppError> {
    let window_ms = parse_duration_string(upcoming).ok_or_else(|| {
        AppError::operation(format!(
            "failed to parse reminder window duration: {upcoming}"
        ))
    })?;
    let now_ms = current_utc_timestamp_ms();
    let deadline_ms = now_ms.saturating_add(window_ms.max(0));
    let mut reminders = Vec::new();

    for record in records
        .into_iter()
        .filter(|record| !record.indexed.archived && !record.completed)
    {
        for reminder in parse_tasknote_reminders(&record.indexed.reminders) {
            let Some(notify_at) = tasknotes_reminder_notify_at(&record.indexed, &reminder) else {
                continue;
            };
            if notify_at > deadline_ms {
                continue;
            }
            reminders.push(TaskReminderItem {
                path: record.path.clone(),
                title: record.indexed.title.clone(),
                status: record.indexed.status.clone(),
                priority: record.indexed.priority.clone(),
                reminder_id: reminder.id,
                reminder_type: reminder.reminder_type,
                related_to: reminder.related_to,
                description: reminder.description,
                notify_at: format_utc_timestamp_ms(notify_at),
                overdue: notify_at < now_ms,
            });
        }
    }

    reminders.sort_by(|left, right| {
        let left_at = parse_date_like_string(&left.notify_at).unwrap_or(i64::MAX);
        let right_at = parse_date_like_string(&right.notify_at).unwrap_or(i64::MAX);
        left_at
            .cmp(&right_at)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.reminder_id.cmp(&right.reminder_id))
    });

    Ok(TaskRemindersReport {
        reference_time: current_utc_timestamp_string(),
        upcoming: upcoming.to_string(),
        reminders,
    })
}

#[derive(Clone, Copy)]
enum TaskReadScope<'a> {
    Filter(Option<&'a vulcan_core::PermissionFilter>),
    Guard(&'a ProfilePermissionGuard),
}

impl TaskReadScope<'_> {
    fn recheck(self, paths: &VaultPaths) -> Result<(), AppError> {
        if let Self::Guard(guard) = self {
            let current =
                vulcan_core::resolve_permission_profile(paths, Some(guard.profile_name()))
                    .map_err(AppError::operation)?;
            if &current != guard.selection() {
                return Err(AppError::operation_with_code(
                    "permission_denied",
                    "task read authority changed; resolve a new guard",
                ));
            }
            guard
                .recheck_read_policy_snapshot()
                .map_err(AppError::operation)?;
        }
        Ok(())
    }

    fn load(self, paths: &VaultPaths) -> Result<HashMap<String, NoteRecord>, AppError> {
        match self {
            Self::Filter(filter) => {
                vulcan_core::properties::load_note_index_with_filter(paths, filter)
            }
            Self::Guard(guard) => vulcan_core::properties::load_note_index_with_guard(paths, guard),
        }
        .map_err(AppError::operation)
    }
}

pub fn build_tasks_query_result(
    paths: &VaultPaths,
    source: &str,
) -> Result<TasksQueryResult, AppError> {
    build_tasks_query_result_with_filter(paths, source, None)
}

pub fn build_tasks_query_result_with_filter(
    paths: &VaultPaths,
    source: &str,
    filter: Option<&vulcan_core::PermissionFilter>,
) -> Result<TasksQueryResult, AppError> {
    build_tasks_query_result_with_scope(paths, source, TaskReadScope::Filter(filter))
}

pub fn build_tasks_query_result_with_guard(
    paths: &VaultPaths,
    source: &str,
    guard: &ProfilePermissionGuard,
) -> Result<TasksQueryResult, AppError> {
    TaskReadScope::Guard(guard).recheck(paths)?;
    let snapshot = guard.snapshot_read_policy().map_err(AppError::operation)?;
    build_tasks_query_result_with_scope(paths, source, TaskReadScope::Guard(&snapshot))
}

fn build_tasks_query_result_with_scope(
    paths: &VaultPaths,
    source: &str,
    scope: TaskReadScope<'_>,
) -> Result<TasksQueryResult, AppError> {
    scope.recheck(paths)?;
    let _read_guard = consistent_task_read(paths)?;
    let result = build_tasks_query_result_with_options(paths, source, false, scope);
    scope.recheck(paths)?;
    result
}

fn consistent_task_read(
    paths: &VaultPaths,
) -> Result<Option<vulcan_core::write_lock::ReadLockGuard>, AppError> {
    vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))
}

pub fn build_tasks_eval_report(
    paths: &VaultPaths,
    request: &TaskEvalRequest,
) -> Result<TasksEvalReport, AppError> {
    let _read_guard = consistent_task_read(paths)?;
    let blocks =
        load_tasks_blocks(paths, &request.file, request.block).map_err(AppError::operation)?;
    let notes = TaskReadScope::Filter(None).load(paths)?;
    Ok(evaluate_tasks_blocks(paths, &request.file, blocks, &notes))
}

pub fn build_tasks_eval_report_with_guard(
    paths: &VaultPaths,
    request: &TaskEvalRequest,
    guard: &ProfilePermissionGuard,
) -> Result<TasksEvalReport, AppError> {
    TaskReadScope::Guard(guard).recheck(paths)?;
    let snapshot = guard.snapshot_read_policy().map_err(AppError::operation)?;
    let scope = TaskReadScope::Guard(&snapshot);
    let _read_guard = consistent_task_read(paths)?;
    let result = (|| {
        scope.recheck(paths)?;
        let decisions = TaskReadDecisions::new(&snapshot);
        let blocks = vulcan_core::tasks::load_tasks_blocks_with_guard(
            paths,
            &request.file,
            request.block,
            &decisions,
        )
        .map_err(AppError::operation)?;
        // Resolve the source and derive every block result from one set of read
        // decisions. A failed policy/index read is an operation error, never a
        // successful report containing only block-level syntax diagnostics.
        let notes = vulcan_core::properties::load_note_index_with_guard(paths, &decisions)
            .map_err(AppError::operation)?;
        Ok(evaluate_tasks_blocks(paths, &request.file, blocks, &notes))
    })();
    scope.recheck(paths)?;
    result
}

type TaskReadDecision = Result<(), vulcan_core::permissions::PermissionError>;

struct TaskReadDecisions<'a> {
    guard: &'a dyn PermissionGuard,
    decisions: std::cell::RefCell<HashMap<String, TaskReadDecision>>,
}

impl<'a> TaskReadDecisions<'a> {
    fn new(guard: &'a dyn PermissionGuard) -> Self {
        Self {
            guard,
            decisions: std::cell::RefCell::default(),
        }
    }
}

impl PermissionGuard for TaskReadDecisions<'_> {
    fn profile_name(&self) -> &str {
        self.guard.profile_name()
    }
    fn grant(&self) -> &vulcan_core::permissions::PermissionGrant {
        self.guard.grant()
    }
    fn has_policy_hook(&self) -> bool {
        self.guard.has_policy_hook()
    }
    fn check_policy_decision(
        &self,
        action: &'static str,
        resource: Option<&str>,
    ) -> TaskReadDecision {
        let Some(path) = resource.filter(|_| action == "read") else {
            return self.guard.check_policy_decision(action, resource);
        };
        if let Some(decision) = self.decisions.borrow().get(path) {
            return decision.clone();
        }
        let decision = self.guard.check_policy_decision(action, resource);
        self.decisions
            .borrow_mut()
            .insert(path.to_string(), decision.clone());
        decision
    }
}

fn evaluate_tasks_blocks(
    paths: &VaultPaths,
    requested_file: &str,
    blocks: Vec<vulcan_core::tasks::TasksBlockRecord>,
    notes: &HashMap<String, NoteRecord>,
) -> TasksEvalReport {
    let file = blocks
        .first()
        .map_or_else(|| requested_file.to_string(), |block| block.file.clone());
    let config = load_vault_config(paths).config.tasks;
    let mut reports = Vec::with_capacity(blocks.len());

    for block in blocks {
        let effective_source = tasks_query_source(&config, &block.source, true);
        let effective_source_override =
            (effective_source != block.source).then(|| effective_source.clone());
        let (mut result, error) = match vulcan_core::tasks::evaluate_tasks_query_with_note_index(
            &effective_source,
            notes,
        ) {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error.to_string())),
        };
        if let Some(result) = result.as_mut() {
            strip_global_filter_from_output(result, &config);
        }

        reports.push(TasksBlockEvalReport {
            block_index: block.block_index,
            line_number: block.line_number,
            source: block.source,
            effective_source: effective_source_override,
            result,
            error,
        });
    }

    TasksEvalReport {
        file,
        blocks: reports,
    }
}

pub fn build_tasks_list_report(
    paths: &VaultPaths,
    request: &TaskListRequest,
) -> Result<TasksQueryResult, AppError> {
    build_tasks_list_report_with_filter(paths, request, None)
}

pub fn build_tasks_list_report_with_filter(
    paths: &VaultPaths,
    request: &TaskListRequest,
    read_filter: Option<&vulcan_core::PermissionFilter>,
) -> Result<TasksQueryResult, AppError> {
    build_tasks_list_report_with_scope(paths, request, TaskReadScope::Filter(read_filter))
}

pub fn build_tasks_list_report_with_guard(
    paths: &VaultPaths,
    request: &TaskListRequest,
    guard: &ProfilePermissionGuard,
) -> Result<TasksQueryResult, AppError> {
    TaskReadScope::Guard(guard).recheck(paths)?;
    let snapshot = guard.snapshot_read_policy().map_err(AppError::operation)?;
    build_tasks_list_report_with_scope(paths, request, TaskReadScope::Guard(&snapshot))
}

fn build_tasks_list_report_with_scope(
    paths: &VaultPaths,
    request: &TaskListRequest,
    scope: TaskReadScope<'_>,
) -> Result<TasksQueryResult, AppError> {
    scope.recheck(paths)?;
    let _read_guard = consistent_task_read(paths)?;
    let config = load_vault_config(paths).config.tasks;
    let filter = request
        .filter
        .as_deref()
        .map(str::trim)
        .filter(|filter| !filter.is_empty());
    let effective_source = request.source.unwrap_or(config.default_source);
    let prefilter_source = tasks_list_prefilter_source(request, effective_source);
    let layout_source = tasks_list_layout_source(request);

    let result = match filter {
        None => {
            let source = join_tasks_query_sections([
                Some(prefilter_source.as_str()),
                Some(layout_source.as_str()),
            ]);
            build_tasks_query_result_with_options(paths, &source, false, scope)
        }
        Some(filter) => match parse_tasks_query(filter) {
            Ok(_) => {
                let source = join_tasks_query_sections([
                    Some(prefilter_source.as_str()),
                    Some(filter),
                    Some(layout_source.as_str()),
                ]);
                build_tasks_query_result_with_options(paths, &source, false, scope)
            }
            Err(tasks_error) => build_tasks_list_dql_filter(
                paths,
                filter,
                &tasks_error,
                &config,
                &prefilter_source,
                &layout_source,
                scope,
            ),
        },
    };
    scope.recheck(paths)?;
    result
}

pub fn build_tasks_view_list_report(
    paths: &VaultPaths,
) -> Result<TaskNotesViewListReport, AppError> {
    let _read_guard = consistent_task_read(paths)?;
    let config = load_vault_config(paths).config;
    let mut files = Vec::new();
    let root = paths.vault_root().join("TaskNotes/Views");
    collect_tasknotes_base_files(&root, "TaskNotes/Views", &mut files)?;

    let mut views = Vec::new();
    for file in files {
        let info = inspect_base_file(paths, &file).map_err(AppError::operation)?;
        let file_stem = Path::new(&file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        for view in info.views {
            views.push(TaskNotesViewListItem {
                file: file.clone(),
                file_stem: file_stem.clone(),
                supported: matches!(
                    view.view_type.to_ascii_lowercase().as_str(),
                    "table" | "tasknotestasklist" | "tasknoteskanban"
                ),
                view_name: view.name,
                view_type: view.view_type,
            });
        }
    }

    for saved_view in &config.tasknotes.saved_views {
        views.push(TaskNotesViewListItem {
            file: format!("config.tasknotes.saved_views.{}", saved_view.id),
            file_stem: saved_view.id.clone(),
            supported: true,
            view_name: Some(saved_view.name.clone()),
            view_type: tasknotes_saved_view_type(saved_view).to_string(),
        });
    }

    views.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then_with(|| left.view_name.cmp(&right.view_name))
            .then_with(|| left.view_type.cmp(&right.view_type))
    });

    Ok(TaskNotesViewListReport { views })
}

pub fn build_tasks_view_report(
    paths: &VaultPaths,
    name: &str,
) -> Result<BasesEvalReport, AppError> {
    let _read_guard = consistent_task_read(paths)?;
    let target = resolve_tasknotes_view_target(paths, name)?;
    if let Some(saved_view) = target.saved_view.as_ref() {
        let tasknotes = load_vault_config(paths).config.tasknotes;
        let yaml = render_tasknotes_saved_view_base_yaml(&tasknotes, saved_view)?;
        return BasesEvaluator::new()
            .evaluate_yaml(paths, &target.file, &yaml)
            .map_err(AppError::operation);
    }
    let mut report = evaluate_base_file(paths, &target.file).map_err(AppError::operation)?;
    if let Some(view_name) = target.view_name.as_deref() {
        report
            .views
            .retain(|view| view.name.as_deref() == Some(view_name));
        if report.views.is_empty() {
            return Err(AppError::operation(format!(
                "view `{view_name}` was not found in {}",
                target.file
            )));
        }
    }
    Ok(report)
}

pub fn build_tasks_next_report(
    paths: &VaultPaths,
    count: usize,
    from: Option<&str>,
) -> Result<TasksNextReport, AppError> {
    derive_task_report_with_guard(paths, "is recurring", None, |result| {
        tasks_next_report(result, count, from)
    })
}

pub fn build_tasks_next_report_with_guard(
    paths: &VaultPaths,
    count: usize,
    from: Option<&str>,
    guard: &ProfilePermissionGuard,
) -> Result<TasksNextReport, AppError> {
    derive_task_report_with_guard(paths, "is recurring", Some(guard), |result| {
        tasks_next_report(result, count, from)
    })
}

fn derive_task_report_with_guard<R>(
    paths: &VaultPaths,
    source: &str,
    guard: Option<&ProfilePermissionGuard>,
    derive: impl FnOnce(TasksQueryResult) -> Result<R, AppError>,
) -> Result<R, AppError> {
    with_task_read_scope(paths, guard, |scope| {
        build_tasks_query_result_with_options(paths, source, false, scope).and_then(derive)
    })
}

fn with_task_read_scope<R>(
    paths: &VaultPaths,
    guard: Option<&ProfilePermissionGuard>,
    derive: impl FnOnce(TaskReadScope<'_>) -> Result<R, AppError>,
) -> Result<R, AppError> {
    let snapshot = guard
        .map(|guard| {
            TaskReadScope::Guard(guard).recheck(paths)?;
            guard.snapshot_read_policy().map_err(AppError::operation)
        })
        .transpose()?;
    let scope = snapshot
        .as_ref()
        .map_or(TaskReadScope::Filter(None), TaskReadScope::Guard);
    scope.recheck(paths)?;
    let _read_guard = consistent_task_read(paths)?;
    let report = derive(scope);
    scope.recheck(paths)?;
    report
}

fn tasks_next_report(
    result: TasksQueryResult,
    count: usize,
    from: Option<&str>,
) -> Result<TasksNextReport, AppError> {
    let (reference_date, reference_ms) = resolve_tasks_reference_date(from)?;
    let mut occurrences = Vec::new();

    for task in result.tasks {
        let Value::Object(task_object) = task.clone() else {
            continue;
        };

        for (sequence, date) in task_upcoming_occurrences(&task_object, reference_ms, count)
            .into_iter()
            .enumerate()
        {
            occurrences.push(TasksNextOccurrence {
                date,
                sequence: sequence.saturating_add(1),
                task: task.clone(),
            });
        }
    }

    occurrences.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| task_sort_key(&left.task).cmp(&task_sort_key(&right.task)))
            .then_with(|| left.sequence.cmp(&right.sequence))
    });
    occurrences.truncate(count);

    Ok(TasksNextReport {
        reference_date,
        result_count: occurrences.len(),
        occurrences,
    })
}

pub fn build_tasks_blocked_report(paths: &VaultPaths) -> Result<TasksBlockedReport, AppError> {
    derive_task_report_with_guard(paths, "", None, |result| Ok(tasks_blocked_report(result)))
}

pub fn build_tasks_blocked_report_with_guard(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<TasksBlockedReport, AppError> {
    derive_task_report_with_guard(paths, "", Some(guard), |result| {
        Ok(tasks_blocked_report(result))
    })
}

fn tasks_blocked_report(task_result: TasksQueryResult) -> TasksBlockedReport {
    let graph = tasks_graph_report(&task_result.tasks);
    let tasks_by_key = task_result
        .tasks
        .into_iter()
        .filter_map(|task| task_dependency_key(&task).map(|key| (key, task)))
        .collect::<HashMap<_, _>>();

    let mut blockers_by_task = HashMap::<String, Vec<TaskDependencyEdge>>::new();
    for edge in graph.edges {
        if !edge.resolved || edge.blocker_completed != Some(true) {
            blockers_by_task
                .entry(edge.blocked_key.clone())
                .or_default()
                .push(edge);
        }
    }

    let mut tasks = blockers_by_task
        .into_iter()
        .filter_map(|(key, blockers)| {
            tasks_by_key
                .get(&key)
                .cloned()
                .map(|task| TasksBlockedItem { task, blockers })
        })
        .collect::<Vec<_>>();
    tasks.sort_by_key(|item| task_sort_key(&item.task));

    TasksBlockedReport { tasks }
}

pub fn build_tasks_graph_report(paths: &VaultPaths) -> Result<TasksGraphReport, AppError> {
    derive_task_report_with_guard(paths, "", None, |result| {
        Ok(tasks_graph_report(&result.tasks))
    })
}

pub fn build_tasks_graph_report_with_guard(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<TasksGraphReport, AppError> {
    derive_task_report_with_guard(paths, "", Some(guard), |result| {
        Ok(tasks_graph_report(&result.tasks))
    })
}

fn tasks_graph_report(tasks: &[Value]) -> TasksGraphReport {
    let mut tasks = tasks
        .iter()
        .filter_map(|task| {
            let key = task_dependency_key(task)?;
            Some((key, task))
        })
        .collect::<Vec<_>>();
    tasks.sort_by_key(|item| task_sort_key(item.1));

    let mut node_by_id = HashMap::<String, TaskDependencyNode>::new();
    let mut nodes = Vec::with_capacity(tasks.len());
    for (key, task) in &tasks {
        let node = TaskDependencyNode {
            key: key.clone(),
            id: task
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .filter(|id| !id.trim().is_empty()),
            path: task
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            line: task.get("line").and_then(Value::as_i64).unwrap_or_default(),
            text: task
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            completed: task
                .get("completed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        if let Some(id) = node.id.clone() {
            node_by_id.entry(id).or_insert_with(|| node.clone());
        }
        nodes.push(node);
    }

    let mut edges = tasks
        .iter()
        .flat_map(|(key, task)| {
            task_blocker_references(task).into_iter().map(|reference| {
                let blocker_id = reference.blocker_id;
                let blocker = node_by_id.get(blocker_id.as_str());
                TaskDependencyEdge {
                    blocked_key: key.clone(),
                    blocker_id,
                    relation_type: reference.relation_type,
                    gap: reference.gap,
                    resolved: blocker.is_some(),
                    blocker_key: blocker.map(|node| node.key.clone()),
                    blocker_path: blocker.map(|node| node.path.clone()),
                    blocker_line: blocker.map(|node| node.line),
                    blocker_text: blocker.map(|node| node.text.clone()),
                    blocker_completed: blocker.map(|node| node.completed),
                }
            })
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        left.blocked_key
            .cmp(&right.blocked_key)
            .then_with(|| left.blocker_id.cmp(&right.blocker_id))
    });

    TasksGraphReport { nodes, edges }
}

fn resolve_tasknotes_view_target(
    paths: &VaultPaths,
    name: &str,
) -> Result<TaskNotesViewTarget, AppError> {
    if is_explicit_tasknotes_view_path(name) {
        let normalized = normalize_relative_input_path(
            name,
            RelativePathOptions {
                expected_extension: Some("base"),
                append_extension_if_missing: true,
            },
        )
        .map_err(AppError::operation)?;
        let _ = inspect_base_file(paths, &normalized).map_err(AppError::operation)?;
        return Ok(TaskNotesViewTarget {
            file: normalized,
            view_name: None,
            saved_view: None,
        });
    }

    let catalog = build_tasks_view_list_report(paths)?;
    if let Some(target) = unique_tasknotes_view_name_match(&catalog.views, name)? {
        return Ok(target);
    }
    if let Some(target) = unique_tasknotes_saved_view_match(paths, name)? {
        return Ok(target);
    }
    if let Some(target) = unique_tasknotes_view_file_match(&catalog.views, name)? {
        return Ok(target);
    }

    Err(AppError::operation(format!(
        "no TaskNotes view matched `{name}`"
    )))
}

fn collect_tasknotes_base_files(
    directory: &Path,
    relative: &str,
    files: &mut Vec<String>,
) -> Result<(), AppError> {
    if !directory.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(directory).map_err(AppError::operation)?;
    for entry in entries {
        let entry = entry.map_err(AppError::operation)?;
        let path = entry.path();
        let relative_path = format!("{relative}/{}", entry.file_name().to_string_lossy());
        let file_type = entry.file_type().map_err(AppError::operation)?;
        if file_type.is_dir() {
            collect_tasknotes_base_files(&path, &relative_path, files)?;
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
        {
            files.push(relative_path);
        }
    }
    Ok(())
}

fn is_explicit_tasknotes_view_path(name: &str) -> bool {
    name.contains('/')
        || name.contains('\\')
        || Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("base"))
}

fn unique_tasknotes_view_name_match(
    views: &[TaskNotesViewListItem],
    name: &str,
) -> Result<Option<TaskNotesViewTarget>, AppError> {
    let matches = views
        .iter()
        .filter(|view| {
            view.view_name
                .as_deref()
                .is_some_and(|view_name| view_name.eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        let options = matches
            .iter()
            .map(|view| {
                format!(
                    "{} ({})",
                    view.view_name.as_deref().unwrap_or("<unnamed>"),
                    view.file
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::operation(format!(
            "multiple TaskNotes views matched `{name}`: {options}"
        )));
    }

    Ok(Some(TaskNotesViewTarget {
        file: matches[0].file.clone(),
        view_name: matches[0].view_name.clone(),
        saved_view: None,
    }))
}

fn unique_tasknotes_view_file_match(
    views: &[TaskNotesViewListItem],
    name: &str,
) -> Result<Option<TaskNotesViewTarget>, AppError> {
    let matches = views
        .iter()
        .filter(|view| {
            tasknotes_view_file_aliases(&view.file_stem)
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Ok(None);
    }

    let file = &matches[0].file;
    if matches.iter().any(|view| view.file != *file) {
        let options = matches
            .iter()
            .map(|view| view.file.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::operation(format!(
            "multiple TaskNotes view files matched `{name}`: {options}"
        )));
    }

    Ok(Some(TaskNotesViewTarget {
        file: file.clone(),
        view_name: None,
        saved_view: None,
    }))
}

fn unique_tasknotes_saved_view_match(
    paths: &VaultPaths,
    name: &str,
) -> Result<Option<TaskNotesViewTarget>, AppError> {
    let config = load_vault_config(paths).config;
    let matches = config
        .tasknotes
        .saved_views
        .into_iter()
        .filter(|view| view.id.eq_ignore_ascii_case(name) || view.name.eq_ignore_ascii_case(name))
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        let options = matches
            .iter()
            .map(|view| format!("{} ({})", view.name, view.id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::operation(format!(
            "multiple TaskNotes saved views matched `{name}`: {options}"
        )));
    }

    let saved_view = matches[0].clone();
    Ok(Some(TaskNotesViewTarget {
        file: format!("config.tasknotes.saved_views.{}", saved_view.id),
        view_name: Some(saved_view.name.clone()),
        saved_view: Some(saved_view),
    }))
}

fn tasknotes_view_file_aliases(file_stem: &str) -> Vec<String> {
    let mut aliases = vec![file_stem.to_string()];
    if let Some(alias) = file_stem.strip_suffix("-default") {
        aliases.push(alias.to_string());
    }
    aliases
}

fn tasknotes_saved_view_type(saved_view: &TaskNotesSavedViewConfig) -> &'static str {
    let has_kanban_options = saved_view
        .view_options
        .keys()
        .any(|key| matches!(key.as_str(), "columnWidth" | "hideEmptyColumns"));
    if has_kanban_options {
        "tasknotesKanban"
    } else {
        "tasknotesTaskList"
    }
}

fn render_tasknotes_saved_view_base_yaml(
    tasknotes: &vulcan_core::TaskNotesConfig,
    saved_view: &TaskNotesSavedViewConfig,
) -> Result<String, AppError> {
    let mut root = YamlMapping::new();
    let mut source = YamlMapping::new();
    source.insert(
        YamlValue::String("type".to_string()),
        YamlValue::String("tasknotes".to_string()),
    );
    let mut source_config = YamlMapping::new();
    source_config.insert(
        YamlValue::String("type".to_string()),
        YamlValue::String(tasknotes_saved_view_type(saved_view).to_string()),
    );
    source.insert(
        YamlValue::String("config".to_string()),
        YamlValue::Mapping(source_config),
    );
    root.insert(
        YamlValue::String("source".to_string()),
        YamlValue::Mapping(source),
    );

    let mut view = YamlMapping::new();
    view.insert(
        YamlValue::String("type".to_string()),
        YamlValue::String(tasknotes_saved_view_type(saved_view).to_string()),
    );
    view.insert(
        YamlValue::String("name".to_string()),
        YamlValue::String(saved_view.name.clone()),
    );
    if let Some(filters) = tasknotes_saved_view_query_to_yaml(tasknotes, &saved_view.query)? {
        view.insert(YamlValue::String("filters".to_string()), filters);
    }
    if let Some(sort_column) = tasknotes_saved_view_sort_column(tasknotes, &saved_view.query) {
        let mut sort_entry = YamlMapping::new();
        sort_entry.insert(
            YamlValue::String("column".to_string()),
            YamlValue::String(sort_column),
        );
        sort_entry.insert(
            YamlValue::String("direction".to_string()),
            YamlValue::String(
                saved_view
                    .query
                    .sort_direction
                    .clone()
                    .unwrap_or_else(|| "ASC".to_string())
                    .to_ascii_uppercase(),
            ),
        );
        view.insert(
            YamlValue::String("sort".to_string()),
            YamlValue::Sequence(vec![YamlValue::Mapping(sort_entry)]),
        );
    }
    if let Some(group_property) = tasknotes_saved_view_group_column(tasknotes, &saved_view.query) {
        let mut group_by = YamlMapping::new();
        group_by.insert(
            YamlValue::String("property".to_string()),
            YamlValue::String(group_property),
        );
        group_by.insert(
            YamlValue::String("direction".to_string()),
            YamlValue::String(
                saved_view
                    .query
                    .sort_direction
                    .clone()
                    .unwrap_or_else(|| "ASC".to_string())
                    .to_ascii_uppercase(),
            ),
        );
        view.insert(
            YamlValue::String("groupBy".to_string()),
            YamlValue::Mapping(group_by),
        );
    }

    root.insert(
        YamlValue::String("views".to_string()),
        YamlValue::Sequence(vec![YamlValue::Mapping(view)]),
    );

    serde_yaml::to_string(&root).map_err(AppError::operation)
}

fn tasknotes_saved_view_query_to_yaml(
    tasknotes: &vulcan_core::TaskNotesConfig,
    query: &vulcan_core::TaskNotesSavedViewQuery,
) -> Result<Option<YamlValue>, AppError> {
    let filters =
        tasknotes_saved_view_group_yaml(tasknotes, &query.conjunction, &query.children, &query.id)?;
    Ok(filters.map(|filters| match filters {
        YamlValue::String(_) => YamlValue::Sequence(vec![filters]),
        other => other,
    }))
}

fn tasknotes_saved_view_group_yaml(
    tasknotes: &vulcan_core::TaskNotesConfig,
    conjunction: &str,
    children: &[TaskNotesSavedViewNode],
    _group_id: &str,
) -> Result<Option<YamlValue>, AppError> {
    let mut clauses = Vec::new();
    for child in children {
        let clause = match child {
            TaskNotesSavedViewNode::Condition(condition) => {
                tasknotes_saved_view_condition_yaml(tasknotes, condition)?
            }
            TaskNotesSavedViewNode::Group(group) => tasknotes_saved_view_group_yaml(
                tasknotes,
                &group.conjunction,
                &group.children,
                &group.id,
            )?,
        };
        if let Some(clause) = clause {
            clauses.push(clause);
        }
    }

    if clauses.is_empty() {
        return Ok(None);
    }
    if clauses.len() == 1 {
        return Ok(clauses.into_iter().next());
    }

    let key = if conjunction.eq_ignore_ascii_case("or") {
        "or"
    } else {
        "and"
    };
    let mut mapping = YamlMapping::new();
    mapping.insert(
        YamlValue::String(key.to_string()),
        YamlValue::Sequence(clauses),
    );
    Ok(Some(YamlValue::Mapping(mapping)))
}

fn tasknotes_saved_view_condition_yaml(
    tasknotes: &vulcan_core::TaskNotesConfig,
    condition: &vulcan_core::TaskNotesSavedViewCondition,
) -> Result<Option<YamlValue>, AppError> {
    if condition.property.trim().is_empty() || condition.operator.trim().is_empty() {
        return Ok(None);
    }
    let no_value = matches!(
        condition.operator.as_str(),
        "is-empty" | "is-not-empty" | "is-checked" | "is-not-checked"
    );
    if !no_value && condition.value.is_none() {
        return Ok(None);
    }
    let expression = tasknotes_saved_view_expression(tasknotes, condition)?;
    Ok(Some(YamlValue::String(expression)))
}

fn tasknotes_saved_view_expression(
    tasknotes: &vulcan_core::TaskNotesConfig,
    condition: &vulcan_core::TaskNotesSavedViewCondition,
) -> Result<String, AppError> {
    let property = condition.property.as_str();
    let operator = condition.operator.as_str();

    if property == "status.isCompleted" {
        let completed_statuses = tasknotes
            .statuses
            .iter()
            .filter(|status| status.is_completed)
            .map(|status| {
                format!(
                    "note.{} == {}",
                    tasknotes.field_mapping.status,
                    quote_saved_view_string(&status.value)
                )
            })
            .collect::<Vec<_>>();
        let completed_expr = if completed_statuses.is_empty() {
            "false".to_string()
        } else if completed_statuses.len() == 1 {
            completed_statuses[0].clone()
        } else {
            format!("({})", completed_statuses.join(" || "))
        };
        let result = format!(
            "({completed_expr}) || (note.{} && note.{}.map(date(value).format(\"YYYY-MM-DD\")).contains({}))",
            tasknotes.field_mapping.complete_instances,
            tasknotes.field_mapping.complete_instances,
            quote_saved_view_string(&current_utc_date_string())
        );
        return Ok(if matches!(operator, "is-not" | "is-not-checked") {
            format!("!({result})")
        } else {
            result
        });
    }

    if property == "archived" {
        let archived_expr = format!(
            "file.tags.contains({})",
            quote_saved_view_string(&tasknotes.field_mapping.archive_tag)
        );
        return Ok(if matches!(operator, "is-not" | "is-not-checked") {
            format!("!{archived_expr}")
        } else {
            archived_expr
        });
    }

    if property == "dependencies.isBlocked" {
        let blocked_key = &tasknotes.field_mapping.blocked_by;
        let blocked_expr = format!("(note.{blocked_key} && list(note.{blocked_key}).length > 0)");
        return Ok(if matches!(operator, "is-not" | "is-not-checked") {
            format!("!({blocked_expr})")
        } else {
            blocked_expr
        });
    }

    if property == "dependencies.isBlocking" {
        return Ok("true".to_string());
    }

    let base_property = tasknotes_saved_view_property_path(tasknotes, property);
    tasknotes_saved_view_operator_expression(
        &base_property,
        operator,
        property,
        condition.value.as_ref(),
    )
}

fn tasknotes_saved_view_operator_expression(
    base_property: &str,
    operator: &str,
    property: &str,
    value: Option<&TaskNotesSavedViewFilterValue>,
) -> Result<String, AppError> {
    let expression = match operator {
        "is" => tasknotes_saved_view_is_expression(base_property, property, value)?,
        "is-not" => format!(
            "!({})",
            tasknotes_saved_view_is_expression(base_property, property, value)?
        ),
        "contains" => tasknotes_saved_view_contains_expression(base_property, property, value)?,
        "does-not-contain" => format!(
            "!({})",
            tasknotes_saved_view_contains_expression(base_property, property, value)?
        ),
        "is-before" => format!(
            "{base_property} < {}",
            quote_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view date comparison is missing a value")
            })?)
        ),
        "is-after" => format!(
            "{base_property} > {}",
            quote_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view date comparison is missing a value")
            })?)
        ),
        "is-on-or-before" => format!(
            "{base_property} <= {}",
            quote_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view date comparison is missing a value")
            })?)
        ),
        "is-on-or-after" => format!(
            "{base_property} >= {}",
            quote_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view date comparison is missing a value")
            })?)
        ),
        "is-empty" => format!("{base_property}.isEmpty()"),
        "is-not-empty" => format!("!{base_property}.isEmpty()"),
        "is-checked" => format!("{base_property} == true"),
        "is-not-checked" => format!("{base_property} != true"),
        "is-greater-than" => format!(
            "{base_property} > {}",
            numeric_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view numeric comparison is missing a value")
            })?)
        ),
        "is-less-than" => format!(
            "{base_property} < {}",
            numeric_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view numeric comparison is missing a value")
            })?)
        ),
        "is-greater-than-or-equal" => format!(
            "{base_property} >= {}",
            numeric_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view numeric comparison is missing a value")
            })?)
        ),
        "is-less-than-or-equal" => format!(
            "{base_property} <= {}",
            numeric_saved_view_value(value.ok_or_else(|| {
                AppError::operation("saved view numeric comparison is missing a value")
            })?)
        ),
        other => {
            return Err(AppError::operation(format!(
                "unsupported TaskNotes saved view operator: {other}"
            )))
        }
    };
    Ok(expression)
}

fn tasknotes_saved_view_is_expression(
    base_property: &str,
    property: &str,
    value: Option<&TaskNotesSavedViewFilterValue>,
) -> Result<String, AppError> {
    let value =
        value.ok_or_else(|| AppError::operation("saved view comparison is missing a value"))?;
    match value {
        TaskNotesSavedViewFilterValue::TextList(values) => {
            if values.is_empty() {
                Ok(format!("(!{base_property} || {base_property}.length == 0)"))
            } else {
                let clauses = values
                    .iter()
                    .map(|entry| tasknotes_saved_view_contains_one(base_property, property, entry))
                    .collect::<Vec<_>>();
                Ok(parenthesize_joined_or(clauses))
            }
        }
        TaskNotesSavedViewFilterValue::Bool(flag) => Ok(format!("{base_property} == {flag}")),
        TaskNotesSavedViewFilterValue::Integer(number) => {
            Ok(format!("{base_property} == {number}"))
        }
        TaskNotesSavedViewFilterValue::Text(text) => {
            if text.trim().is_empty() {
                Ok(format!(
                    "(!{base_property} || {base_property} == \"\" || {base_property} == null)"
                ))
            } else if tasknotes_saved_view_is_list_property(property) {
                Ok(tasknotes_saved_view_contains_one(
                    base_property,
                    property,
                    text,
                ))
            } else {
                Ok(format!(
                    "{base_property} == {}",
                    quote_saved_view_string(text)
                ))
            }
        }
    }
}

fn tasknotes_saved_view_contains_expression(
    base_property: &str,
    property: &str,
    value: Option<&TaskNotesSavedViewFilterValue>,
) -> Result<String, AppError> {
    let value = value
        .ok_or_else(|| AppError::operation("saved view contains comparison is missing a value"))?;
    match value {
        TaskNotesSavedViewFilterValue::TextList(values) => Ok(parenthesize_joined_or(
            values
                .iter()
                .map(|entry| tasknotes_saved_view_contains_one(base_property, property, entry))
                .collect(),
        )),
        TaskNotesSavedViewFilterValue::Text(text) => Ok(tasknotes_saved_view_contains_one(
            base_property,
            property,
            text,
        )),
        TaskNotesSavedViewFilterValue::Bool(flag) => Ok(format!(
            "{base_property}.lower().contains({})",
            quote_saved_view_string(&flag.to_string())
        )),
        TaskNotesSavedViewFilterValue::Integer(number) => Ok(format!(
            "{base_property}.lower().contains({})",
            quote_saved_view_string(&number.to_string())
        )),
    }
}

fn tasknotes_saved_view_contains_one(base_property: &str, property: &str, value: &str) -> String {
    if property == "projects" {
        if value.starts_with("[[") && value.ends_with("]]") {
            return format!(
                "{base_property}.contains({})",
                quote_saved_view_string(value)
            );
        }
        return format!(
            "({base_property}.contains({}) || {base_property}.contains({}))",
            quote_saved_view_string(&format!("[[{value}]]")),
            quote_saved_view_string(value)
        );
    }

    if tasknotes_saved_view_is_list_property(property) {
        return format!(
            "{base_property}.contains({})",
            quote_saved_view_string(value)
        );
    }

    format!(
        "{base_property}.lower().contains({})",
        quote_saved_view_string(&value.to_ascii_lowercase())
    )
}

fn tasknotes_saved_view_is_list_property(property: &str) -> bool {
    matches!(
        property,
        "tags" | "contexts" | "projects" | "blockedBy" | "blocking"
    )
}

fn tasknotes_saved_view_property_path(
    tasknotes: &vulcan_core::TaskNotesConfig,
    property: &str,
) -> String {
    match property {
        "title" => "file.name".to_string(),
        "status" => format!("note.{}", tasknotes.field_mapping.status),
        "priority" => format!("note.{}", tasknotes.field_mapping.priority),
        "due" => format!("note.{}", tasknotes.field_mapping.due),
        "scheduled" => format!("note.{}", tasknotes.field_mapping.scheduled),
        "contexts" => format!("note.{}", tasknotes.field_mapping.contexts),
        "projects" => format!("note.{}", tasknotes.field_mapping.projects),
        "tags" => "file.tags".to_string(),
        "path" => "file.path".to_string(),
        "dateCreated" => "file.ctime".to_string(),
        "dateModified" => "file.mtime".to_string(),
        "timeEstimate" => format!("note.{}", tasknotes.field_mapping.time_estimate),
        "completedDate" => format!("note.{}", tasknotes.field_mapping.completed_date),
        "recurrence" => format!("note.{}", tasknotes.field_mapping.recurrence),
        "blockedBy" => format!("note.{}", tasknotes.field_mapping.blocked_by),
        user if user.starts_with("user:") => tasknotes
            .user_fields
            .iter()
            .find(|field| field.id == user[5..] || field.key == user[5..])
            .map_or_else(
                || format!("note.{user}"),
                |field| format!("note.{}", field.key),
            ),
        other => format!("note.{other}"),
    }
}

fn tasknotes_saved_view_sort_column(
    tasknotes: &vulcan_core::TaskNotesConfig,
    query: &vulcan_core::TaskNotesSavedViewQuery,
) -> Option<String> {
    query
        .sort_key
        .as_deref()
        .filter(|key| !key.trim().is_empty() && *key != "none")
        .map(|key| tasknotes_saved_view_column_name(tasknotes, key))
}

fn tasknotes_saved_view_group_column(
    tasknotes: &vulcan_core::TaskNotesConfig,
    query: &vulcan_core::TaskNotesSavedViewQuery,
) -> Option<String> {
    query
        .group_key
        .as_deref()
        .filter(|key| !key.trim().is_empty() && *key != "none")
        .map(|key| tasknotes_saved_view_column_name(tasknotes, key))
}

fn tasknotes_saved_view_column_name(tasknotes: &vulcan_core::TaskNotesConfig, key: &str) -> String {
    match key {
        "due" => tasknotes.field_mapping.due.clone(),
        "scheduled" => tasknotes.field_mapping.scheduled.clone(),
        "priority" => tasknotes.field_mapping.priority.clone(),
        "status" => tasknotes.field_mapping.status.clone(),
        "title" => tasknotes.field_mapping.title.clone(),
        "dateCreated" => "file.ctime".to_string(),
        "dateModified" => "file.mtime".to_string(),
        "completedDate" => tasknotes.field_mapping.completed_date.clone(),
        "tags" => "file.tags".to_string(),
        "path" => "file.path".to_string(),
        "timeEstimate" => tasknotes.field_mapping.time_estimate.clone(),
        "recurrence" => tasknotes.field_mapping.recurrence.clone(),
        user if user.starts_with("user:") => tasknotes
            .user_fields
            .iter()
            .find(|field| field.id == user[5..] || field.key == user[5..])
            .map_or_else(|| user.to_string(), |field| field.key.clone()),
        other => other.to_string(),
    }
}

fn quote_saved_view_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn quote_saved_view_value(value: &TaskNotesSavedViewFilterValue) -> String {
    match value {
        TaskNotesSavedViewFilterValue::Bool(flag) => flag.to_string(),
        TaskNotesSavedViewFilterValue::Integer(number) => number.to_string(),
        TaskNotesSavedViewFilterValue::Text(text) => quote_saved_view_string(text),
        TaskNotesSavedViewFilterValue::TextList(values) => {
            quote_saved_view_string(&values.join(", "))
        }
    }
}

fn numeric_saved_view_value(value: &TaskNotesSavedViewFilterValue) -> String {
    match value {
        TaskNotesSavedViewFilterValue::Integer(number) => number.to_string(),
        TaskNotesSavedViewFilterValue::Bool(flag) => {
            if *flag {
                "1".to_string()
            } else {
                "0".to_string()
            }
        }
        TaskNotesSavedViewFilterValue::Text(text) => {
            text.parse::<i64>().unwrap_or_default().to_string()
        }
        TaskNotesSavedViewFilterValue::TextList(values) => values
            .first()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default()
            .to_string(),
    }
}

fn parenthesize_joined_or(clauses: Vec<String>) -> String {
    if clauses.len() == 1 {
        clauses.into_iter().next().unwrap_or_default()
    } else {
        format!("({})", clauses.join(" || "))
    }
}

fn resolve_tasks_reference_date(from: Option<&str>) -> Result<(String, i64), AppError> {
    let reference_date = from
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || TemplateTimestamp::current().default_date_string(),
            ToOwned::to_owned,
        );
    let reference_ms = parse_date_like_string(&reference_date).ok_or_else(|| {
        AppError::operation(format!(
            "failed to parse recurrence reference date: {reference_date}"
        ))
    })?;

    Ok((day_string_from_ms(reference_ms), reference_ms))
}

fn day_string_from_ms(ms: i64) -> String {
    let (year, month, day, _, _, _, _) = date_components(ms);
    format!("{year:04}-{month:02}-{day:02}")
}

fn build_tasks_query_result_with_options(
    paths: &VaultPaths,
    source: &str,
    include_global_query: bool,
    scope: TaskReadScope<'_>,
) -> Result<TasksQueryResult, AppError> {
    let config = load_vault_config(paths).config.tasks;
    let effective_source = tasks_query_source(&config, source, include_global_query);
    // Parse before loading records, preserving syntax-error precedence.
    parse_tasks_query(&effective_source).map_err(AppError::operation)?;
    let note_index = scope.load(paths)?;
    let mut result =
        vulcan_core::tasks::evaluate_tasks_query_with_note_index(&effective_source, &note_index)
            .map_err(AppError::operation)?;
    strip_global_filter_from_output(&mut result, &config);
    Ok(result)
}

fn tasks_query_source(
    config: &vulcan_core::config::TasksConfig,
    source: &str,
    include_global_query: bool,
) -> String {
    let mut sections = Vec::new();
    if let Some(tag) = config
        .global_filter
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    {
        sections.push(format!("tag includes {tag}"));
    }
    if include_global_query {
        if let Some(query) = config
            .global_query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
        {
            sections.push(query.to_string());
        }
    }
    if !source.trim().is_empty() {
        sections.push(source.trim().to_string());
    }
    sections.join("\n")
}

fn build_tasks_list_dql_filter(
    paths: &VaultPaths,
    filter: &str,
    tasks_error: &str,
    config: &vulcan_core::config::TasksConfig,
    prefilter_source: &str,
    layout_source: &str,
    scope: TaskReadScope<'_>,
) -> Result<TasksQueryResult, AppError> {
    let expression_source = tasks_dql_filter_expression(config, filter);
    let expression = parse_expression(&expression_source).map_err(|expression_error| {
        AppError::operation(format!(
            "failed to parse filter as Tasks DSL ({tasks_error}); failed to parse as Dataview expression ({expression_error})"
        ))
    })?;

    let base_source = tasks_query_source(config, prefilter_source, false);
    parse_tasks_query(&base_source).map_err(AppError::operation)?;
    let note_index = scope.load(paths)?;
    let base_result =
        vulcan_core::tasks::evaluate_tasks_query_with_note_index(&base_source, &note_index)
            .map_err(AppError::operation)?;
    let note_by_path = note_index
        .values()
        .map(|note| (note.document_path.as_str(), note))
        .collect::<HashMap<_, _>>();
    let formulas = BTreeMap::new();
    let mut tasks = Vec::new();

    for task in base_result.tasks {
        let Some(path) = task.get("path").and_then(Value::as_str) else {
            continue;
        };
        let Some(note) = note_by_path.get(path) else {
            continue;
        };
        let Value::Object(task_fields) = task.clone() else {
            continue;
        };

        let mut scoped_note = (*note).clone();
        scoped_note.properties = Value::Object(task_fields);
        let context = EvalContext::new(&scoped_note, &formulas).with_note_lookup(&note_index);
        let value = evaluate_expression(&expression, &context).map_err(|error| {
            AppError::operation(format!(
                "failed to evaluate Dataview expression for {path}: {error}"
            ))
        })?;
        if is_truthy(&value) {
            tasks.push(task);
        }
    }

    let mut result = if layout_source.trim().is_empty() {
        TasksQueryResult {
            result_count: tasks.len(),
            tasks,
            groups: Vec::new(),
            hidden_fields: Vec::new(),
            shown_fields: Vec::new(),
            short_mode: false,
            plan: None,
        }
    } else {
        let layout_query = parse_tasks_query(layout_source).map_err(AppError::operation)?;
        shape_tasks_query_result(tasks, &layout_query)
    };
    strip_global_filter_from_output(&mut result, config);
    Ok(result)
}

fn tasks_list_prefilter_source(request: &TaskListRequest, source: TasksDefaultSource) -> String {
    let mut sections = Vec::new();
    if !request.include_archived {
        sections.push("is not archived".to_string());
    }
    match source {
        TasksDefaultSource::Tasknotes => sections.push("source is file".to_string()),
        TasksDefaultSource::Inline => sections.push("source is inline".to_string()),
        TasksDefaultSource::All => {}
    }
    if let Some(status) = tasks_query_value(request.status.as_deref()) {
        sections.push(format!("status is {}", quote_tasks_query_value(status)));
    }
    if let Some(priority) = tasks_query_value(request.priority.as_deref()) {
        sections.push(format!("priority is {}", quote_tasks_query_value(priority)));
    }
    if let Some(due_before) = tasks_query_value(request.due_before.as_deref()) {
        sections.push(format!(
            "due before {}",
            quote_tasks_query_value(due_before)
        ));
    }
    if let Some(due_after) = tasks_query_value(request.due_after.as_deref()) {
        sections.push(format!("due after {}", quote_tasks_query_value(due_after)));
    }
    if let Some(project) = tasks_query_value(request.project.as_deref()) {
        sections.push(format!(
            "project includes {}",
            quote_tasks_query_value(project)
        ));
    }
    if let Some(context) = tasks_query_value(request.context.as_deref()) {
        sections.push(format!(
            "context includes {}",
            quote_tasks_query_value(context)
        ));
    }
    sections.join("\n")
}

fn tasks_list_layout_source(request: &TaskListRequest) -> String {
    let mut sections = Vec::new();
    if let Some(sort_by) = tasks_query_value(request.sort_by.as_deref()) {
        sections.push(format!("sort by {}", quote_tasks_query_value(sort_by)));
    }
    if let Some(group_by) = tasks_query_value(request.group_by.as_deref()) {
        sections.push(format!("group by {}", quote_tasks_query_value(group_by)));
    }
    sections.join("\n")
}

fn tasks_query_value(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn quote_tasks_query_value(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '#' | '@'))
    {
        return value.to_string();
    }

    if !value.contains('"') {
        return format!("\"{value}\"");
    }
    if !value.contains('\'') {
        return format!("'{value}'");
    }

    value.to_string()
}

fn join_tasks_query_sections<'a>(sections: impl IntoIterator<Item = Option<&'a str>>) -> String {
    sections
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>()
        .join("\n")
}

fn tasks_dql_filter_expression(config: &vulcan_core::config::TasksConfig, filter: &str) -> String {
    let mut clauses = Vec::new();
    if let Some(tag) = config
        .global_filter
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    {
        let quoted = serde_json::to_string(tag).expect("task filter tag should serialize");
        clauses.push(format!("contains(tags, {quoted})"));
    }
    clauses.push(format!("({})", filter.trim()));
    clauses.join(" && ")
}

fn strip_global_filter_from_output(
    result: &mut TasksQueryResult,
    config: &vulcan_core::config::TasksConfig,
) {
    if !config.remove_global_filter {
        return;
    }
    let Some(global_filter) = config
        .global_filter
        .as_deref()
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
    else {
        return;
    };

    let normalized = normalize_tag_name(global_filter);
    for task in &mut result.tasks {
        strip_task_global_filter(task, global_filter, &normalized);
    }
    for group in &mut result.groups {
        for task in &mut group.tasks {
            strip_task_global_filter(task, global_filter, &normalized);
        }
    }
}

fn strip_task_global_filter(task: &mut Value, raw_tag: &str, normalized_tag: &str) {
    let Some(object) = task.as_object_mut() else {
        return;
    };

    if let Some(Value::Array(tags)) = object.get_mut("tags") {
        tags.retain(|tag| {
            tag.as_str()
                .is_none_or(|tag| normalize_tag_name(tag) != normalized_tag)
        });
    }

    for field in ["text", "visual"] {
        if let Some(Value::String(text)) = object.get_mut(field) {
            *text = strip_tag_from_text(text, raw_tag, normalized_tag);
        }
    }

    if let Some(Value::Array(children)) = object.get_mut("children") {
        for child in children {
            strip_task_global_filter(child, raw_tag, normalized_tag);
        }
    }
}

fn strip_tag_from_text(text: &str, raw_tag: &str, normalized_tag: &str) -> String {
    text.split_whitespace()
        .filter(|token| {
            !token.eq_ignore_ascii_case(raw_tag) && normalize_tag_name(token) != normalized_tag
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn task_dependency_key(task: &Value) -> Option<String> {
    let path = task.get("path").and_then(Value::as_str)?;
    let line = task.get("line").and_then(Value::as_i64).unwrap_or_default();
    Some(
        task.get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("{path}:{line}"), ToOwned::to_owned),
    )
}

fn task_blocker_references(task: &Value) -> Vec<TaskDependencyReference> {
    let mut references = Vec::new();
    collect_task_blocker_references(
        task.get("blockedBy").unwrap_or(&Value::Null),
        &mut references,
    );
    if references.is_empty() {
        collect_task_blocker_references(
            task.get("blocked-by").unwrap_or(&Value::Null),
            &mut references,
        );
    }
    references.sort_by(|left, right| {
        left.blocker_id
            .cmp(&right.blocker_id)
            .then_with(|| left.relation_type.cmp(&right.relation_type))
            .then_with(|| left.gap.cmp(&right.gap))
    });
    references.dedup();
    references
}

fn collect_task_blocker_references(value: &Value, references: &mut Vec<TaskDependencyReference>) {
    match value {
        Value::String(text) => {
            let text = text.trim();
            if !text.is_empty() {
                references.push(TaskDependencyReference {
                    blocker_id: text.to_string(),
                    relation_type: None,
                    gap: None,
                });
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_task_blocker_references(value, references);
            }
        }
        Value::Object(object) => {
            if let Some(uid) = object.get("uid").and_then(Value::as_str).map(str::trim) {
                if !uid.is_empty() {
                    references.push(TaskDependencyReference {
                        blocker_id: uid.to_string(),
                        relation_type: object
                            .get("reltype")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        gap: object
                            .get("gap")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    });
                }
            }
        }
        _ => {}
    }
}

fn task_sort_key(task: &Value) -> (String, i64) {
    (
        task.get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        task.get("line").and_then(Value::as_i64).unwrap_or_default(),
    )
}

pub fn build_task_track_log_report(
    paths: &VaultPaths,
    task: &str,
) -> Result<TaskTrackLogReport, AppError> {
    let _read_guard = consistent_task_read(paths)?;
    let loaded = load_tasknote_note(paths, task)?;
    Ok(task_track_log_report(loaded))
}

pub fn build_task_track_log_report_with_guard(
    paths: &VaultPaths,
    task: &str,
    guard: &ProfilePermissionGuard,
) -> Result<TaskTrackLogReport, AppError> {
    with_task_read_scope(paths, Some(guard), |scope| {
        let TaskReadScope::Guard(snapshot) = scope else {
            unreachable!("a supplied guard always produces a guarded scope")
        };
        Ok(task_track_log_report(load_tasknote_for_read(
            paths, task, snapshot,
        )?))
    })
}

fn task_track_log_report(loaded: LoadedTaskNote) -> TaskTrackLogReport {
    let now_ms = current_utc_timestamp_ms();
    let entries = parse_tasknote_time_entries(&loaded.indexed.time_entries, now_ms)
        .into_iter()
        .map(task_time_entry_report)
        .collect::<Vec<_>>();
    let (total_time_minutes, active_time_minutes, estimate_remaining_minutes, efficiency_ratio) =
        tasknote_time_metrics(&loaded.indexed, now_ms);

    TaskTrackLogReport {
        path: loaded.path,
        title: loaded.indexed.title,
        total_time_minutes,
        active_time_minutes,
        estimate_remaining_minutes,
        efficiency_ratio,
        entries,
    }
}

pub fn build_task_track_summary_report(
    paths: &VaultPaths,
    period: TaskTrackSummaryPeriod,
) -> Result<TaskTrackSummaryReport, AppError> {
    build_task_track_summary_report_with_guard(paths, period, None)
}

pub fn build_task_track_summary_report_with_guard(
    paths: &VaultPaths,
    period: TaskTrackSummaryPeriod,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskTrackSummaryReport, AppError> {
    with_task_read_scope(paths, guard, |scope| {
        task_track_summary_report(
            paths,
            period,
            load_tasknote_records_with_scope(paths, scope)?,
        )
    })
}

fn task_track_summary_report(
    paths: &VaultPaths,
    period: TaskTrackSummaryPeriod,
    records: Vec<TaskNoteRecord>,
) -> Result<TaskTrackSummaryReport, AppError> {
    let config = load_vault_config(paths).config;
    let (from, to, from_ms, now_ms) = resolve_task_track_summary_window(&config, period)?;
    let mut total_minutes = 0_i64;
    let mut tasks_with_time = 0_usize;
    let mut active_tasks = 0_usize;
    let mut completed_tasks = 0_usize;
    let mut task_totals = Vec::new();
    let mut project_totals = HashMap::<String, i64>::new();

    for record in records {
        let entries = parse_tasknote_time_entries(&record.indexed.time_entries, now_ms);
        let mut task_minutes = 0_i64;
        let mut has_active_session = false;

        for entry in entries {
            let Some(start_ms) = parse_date_like_string(&entry.start_time) else {
                continue;
            };
            if start_ms < from_ms || start_ms > now_ms {
                continue;
            }
            task_minutes += entry.duration_minutes;
            has_active_session |= entry.is_active;
        }

        if task_minutes <= 0 {
            continue;
        }

        total_minutes += task_minutes;
        tasks_with_time += 1;
        if has_active_session {
            active_tasks += 1;
        } else if record.completed {
            completed_tasks += 1;
        }
        task_totals.push(TaskTrackSummaryTaskItem {
            path: record.path.clone(),
            title: record.indexed.title.clone(),
            minutes: task_minutes,
        });
        for project in &record.indexed.projects {
            *project_totals.entry(project.clone()).or_default() += task_minutes;
        }
    }

    task_totals.sort_by(|left, right| {
        right
            .minutes
            .cmp(&left.minutes)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut top_projects = project_totals
        .into_iter()
        .map(|(project, minutes)| TaskTrackSummaryProjectItem { project, minutes })
        .collect::<Vec<_>>();
    top_projects.sort_by(|left, right| {
        right
            .minutes
            .cmp(&left.minutes)
            .then_with(|| left.project.cmp(&right.project))
    });

    Ok(TaskTrackSummaryReport {
        period: match period {
            TaskTrackSummaryPeriod::Day => "day",
            TaskTrackSummaryPeriod::Week => "week",
            TaskTrackSummaryPeriod::Month => "month",
            TaskTrackSummaryPeriod::All => "all",
        }
        .to_string(),
        from,
        to,
        total_minutes,
        total_hours: parse_f64_value(total_minutes) / 60.0,
        tasks_with_time,
        active_tasks,
        completed_tasks,
        top_tasks: task_totals,
        top_projects,
    })
}

#[allow(clippy::too_many_lines)]
pub fn apply_task_pomodoro_start(
    paths: &VaultPaths,
    request: &TaskPomodoroStartRequest,
) -> Result<TaskPomodoroReport, AppError> {
    apply_task_pomodoro_start_with_guard(paths, request, None)
}

#[allow(clippy::too_many_lines)]
pub fn apply_task_pomodoro_start_with_guard(
    paths: &VaultPaths,
    request: &TaskPomodoroStartRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskPomodoroReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    let mut loaded = load_tasknote_note_with_guard(paths, &request.task, guard)?;
    let now_ms = current_utc_timestamp_ms();
    let storage_note_path =
        task_pomodoro_storage_target_path(&loaded.config, &loaded.path, now_ms)?;
    if let Some(guard) = guard {
        guard
            .check_read_path(&storage_note_path)
            .map_err(AppError::operation)?;
        guard
            .check_write_path(&storage_note_path)
            .map_err(AppError::operation)?;
    }
    let mut changed_paths = process_due_task_pomodoros(paths, request.dry_run, guard)?;
    if resolve_active_task_pomodoro_session(paths, None, guard, &changed_paths)?.is_some() {
        return Err(AppError::operation(
            "a TaskNotes pomodoro session is already active",
        ));
    }

    if changed_paths.contains(&loaded.path) && !request.dry_run {
        loaded = load_tasknote_note_with_guard(paths, &request.task, guard)?;
    }
    let start_time = current_utc_timestamp_string();
    let config = loaded.config.clone();
    let session = TaskPomodoroSession {
        id: current_utc_timestamp_ms().to_string(),
        start_time: start_time.clone(),
        end_time: None,
        planned_duration: config.tasknotes.pomodoro.work_duration.max(1),
        session_type: "work".to_string(),
        task_path: Some(loaded.path.clone()),
        completed: false,
        interrupted: false,
        active_periods: vec![TaskPomodoroActivePeriod {
            start_time,
            end_time: None,
        }],
    };
    let session_value = task_pomodoro_session_yaml_value(&session)?;

    let mutation = if matches!(
        config.tasknotes.pomodoro.storage_location,
        vulcan_core::config::TaskNotesPomodoroStorageLocation::Task
    ) {
        apply_loaded_tasknote_mutation_with_guard(
            paths,
            &loaded,
            "pomodoro_start",
            request.dry_run,
            guard,
            |frontmatter, loaded| {
                let mut changes = Vec::new();
                if let Some(change) = update_pomodoro_session_sequence(
                    frontmatter,
                    &loaded.config.tasknotes.field_mapping.pomodoros,
                    |sessions| {
                        sessions.push(session_value.clone());
                        Ok(())
                    },
                )? {
                    changes.push(change);
                }
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    &loaded.config.tasknotes.field_mapping.date_modified,
                    Some(YamlValue::String(current_utc_timestamp_string())),
                ) {
                    changes.push(change);
                }
                Ok(TaskMutationPlan {
                    changes,
                    moved_to: None,
                })
            },
        )?
    } else {
        apply_note_frontmatter_mutation_with_guard(
            paths,
            &storage_note_path,
            Some("daily"),
            "pomodoro_start",
            request.dry_run,
            guard,
            |frontmatter, _loaded| {
                let mut changes = Vec::new();
                if let Some(change) = update_pomodoro_session_sequence(
                    frontmatter,
                    &config.tasknotes.field_mapping.pomodoros,
                    |sessions| {
                        sessions.push(session_value.clone());
                        Ok(())
                    },
                )? {
                    changes.push(change);
                }
                Ok(changes)
            },
        )?
    };

    changed_paths.extend(mutation.changed_paths);
    changed_paths.sort();
    changed_paths.dedup();

    let completed_work_sessions = completed_work_task_pomodoros(
        &collect_tasknotes_pomodoro_sessions_with_overrides(paths, &changed_paths, guard)?,
    );
    let (suggested_break_type, suggested_break_minutes) =
        suggested_task_pomodoro_break(&config, completed_work_sessions.saturating_add(1));

    Ok(TaskPomodoroReport {
        action: "start".to_string(),
        dry_run: request.dry_run,
        storage_note_path,
        task_path: Some(loaded.path),
        title: Some(loaded.indexed.title),
        session: task_pomodoro_session_report(&session, now_ms),
        completed_work_sessions,
        suggested_break_type,
        suggested_break_minutes,
        changed_paths,
    })
}

#[allow(clippy::too_many_lines)]
pub fn apply_task_pomodoro_stop(
    paths: &VaultPaths,
    request: &TaskPomodoroStopRequest,
) -> Result<TaskPomodoroReport, AppError> {
    apply_task_pomodoro_stop_with_guard(paths, request, None)
}

#[allow(clippy::too_many_lines)]
pub fn apply_task_pomodoro_stop_with_guard(
    paths: &VaultPaths,
    request: &TaskPomodoroStopRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskPomodoroReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    if let Some(task) = request.task.as_deref() {
        resolve_task_mutation_path(paths, task, guard)?;
    }
    let mut changed_paths = process_due_task_pomodoros(paths, request.dry_run, guard)?;
    let active = resolve_active_task_pomodoro_session(
        paths,
        request.task.as_deref(),
        guard,
        &changed_paths,
    )?
    .ok_or_else(|| AppError::operation("no active TaskNotes pomodoro session"))?;

    let config = load_vault_config(paths).config;
    let now_ms = current_utc_timestamp_ms();
    let stop_time = current_utc_timestamp_string();
    let updated_session = finalize_task_pomodoro_session(&active.session, &stop_time, false, true);
    let storage_note_path = active.storage_note_path.clone();

    let mutation = if matches!(
        config.tasknotes.pomodoro.storage_location,
        vulcan_core::config::TaskNotesPomodoroStorageLocation::Task
    ) {
        let target_task = active
            .task_path
            .as_deref()
            .unwrap_or(storage_note_path.as_str());
        let loaded = load_tasknote_note_with_guard(paths, target_task, guard)?;
        apply_loaded_tasknote_mutation_with_guard(
            paths,
            &loaded,
            "pomodoro_stop",
            request.dry_run,
            guard,
            |frontmatter, loaded| {
                let mut changes = Vec::new();
                if let Some(change) = update_pomodoro_session_sequence(
                    frontmatter,
                    &loaded.config.tasknotes.field_mapping.pomodoros,
                    |sessions| {
                        let Some((index, _)) =
                            sessions.iter().enumerate().rev().find(|(_, value)| {
                                parse_task_pomodoro_session_yaml(value).is_some_and(|session| {
                                    session.id == active.session.id && session.end_time.is_none()
                                })
                            })
                        else {
                            return Err(AppError::operation(format!(
                                "failed to locate the active pomodoro session in {path}",
                                path = loaded.path
                            )));
                        };
                        sessions[index] = task_pomodoro_session_yaml_value(&updated_session)?;
                        Ok(())
                    },
                )? {
                    changes.push(change);
                }
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    &loaded.config.tasknotes.field_mapping.date_modified,
                    Some(YamlValue::String(current_utc_timestamp_string())),
                ) {
                    changes.push(change);
                }
                Ok(TaskMutationPlan {
                    changes,
                    moved_to: None,
                })
            },
        )?
    } else {
        apply_note_frontmatter_mutation_with_guard(
            paths,
            &storage_note_path,
            Some("daily"),
            "pomodoro_stop",
            request.dry_run,
            guard,
            |frontmatter, _loaded| {
                let mut changes = Vec::new();
                if let Some(change) = update_pomodoro_session_sequence(
                    frontmatter,
                    &config.tasknotes.field_mapping.pomodoros,
                    |sessions| {
                        let Some((index, _)) =
                            sessions.iter().enumerate().rev().find(|(_, value)| {
                                parse_task_pomodoro_session_yaml(value).is_some_and(|session| {
                                    session.id == active.session.id && session.end_time.is_none()
                                })
                            })
                        else {
                            return Err(AppError::operation(format!(
                                "failed to locate the active pomodoro session in {storage_note_path}"
                            )));
                        };
                        sessions[index] = task_pomodoro_session_yaml_value(&updated_session)?;
                        Ok(())
                    },
                )? {
                    changes.push(change);
                }
                Ok(changes)
            },
        )?
    };

    changed_paths.extend(mutation.changed_paths);
    changed_paths.sort();
    changed_paths.dedup();

    let completed_work_sessions = completed_work_task_pomodoros(
        &collect_tasknotes_pomodoro_sessions_with_overrides(paths, &changed_paths, guard)?,
    );
    let (suggested_break_type, suggested_break_minutes) =
        suggested_task_pomodoro_break(&config, completed_work_sessions);

    Ok(TaskPomodoroReport {
        action: "stop".to_string(),
        dry_run: request.dry_run,
        storage_note_path,
        task_path: active.task_path,
        title: active.title,
        session: task_pomodoro_session_report(&updated_session, now_ms),
        completed_work_sessions,
        suggested_break_type,
        suggested_break_minutes,
        changed_paths,
    })
}

pub fn build_task_pomodoro_status_report(
    paths: &VaultPaths,
) -> Result<TaskPomodoroStatusReport, AppError> {
    build_task_pomodoro_status_report_with_guard(paths, None)
}

pub fn build_task_pomodoro_status_report_with_guard(
    paths: &VaultPaths,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskPomodoroStatusReport, AppError> {
    preflight_task_mutation_guard(paths, guard)?;
    let changed_paths = process_due_task_pomodoros(paths, false, guard)?;
    let config = load_vault_config(paths).config;
    let sessions =
        collect_tasknotes_pomodoro_sessions_with_overrides(paths, &changed_paths, guard)?;
    let completed_work_sessions = completed_work_task_pomodoros(&sessions);
    let mut active_sessions = sessions
        .iter()
        .filter(|stored| stored.session.end_time.is_none())
        .cloned()
        .collect::<Vec<_>>();
    let active = match active_sessions.len() {
        0 => None,
        1 => active_sessions.pop(),
        _ => {
            return Err(AppError::operation(
                "multiple active TaskNotes pomodoro sessions; specify the task to stop",
            ))
        }
    };
    let projected_sessions = completed_work_sessions.saturating_add(
        active
            .as_ref()
            .filter(|stored| stored.session.session_type == "work")
            .map_or(0, |_| 1),
    );
    let (suggested_break_type, suggested_break_minutes) =
        suggested_task_pomodoro_break(&config, projected_sessions);
    let now_ms = current_utc_timestamp_ms();

    Ok(TaskPomodoroStatusReport {
        active: active.map(|stored| TaskPomodoroStatusItem {
            storage_note_path: stored.storage_note_path,
            task_path: stored.task_path,
            title: stored.title,
            session: task_pomodoro_session_report(&stored.session, now_ms),
        }),
        completed_work_sessions,
        suggested_break_type,
        suggested_break_minutes,
        changed_paths,
    })
}

fn apply_task_convert_line(
    paths: &VaultPaths,
    file: &str,
    line_number: i64,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskConvertReport, AppError> {
    if !dry_run && guard.is_none() {
        recover_pending_ordinary_task_write(paths)?;
    }
    let config = load_vault_config(paths).config;
    let (source_path, source) = read_existing_note_source(paths, file, guard)?;
    let selection = resolve_task_convert_line(&source, line_number)?;
    let planned = build_converted_tasknote(
        paths,
        &config,
        &selection.title_input,
        &selection.details,
        selection.completed,
        guard,
    )?;
    let replacement_line = format!(
        "{}[[{}]]",
        selection.replacement_prefix,
        tasknote_link_target(&planned.relative_path)
    );
    let (updated_source, source_change) =
        replace_task_convert_line_range(&source, &selection, &replacement_line)?;
    let rendered_task = render_note_from_parts(Some(&planned.frontmatter), &planned.body)
        .map_err(AppError::operation)?;
    let frontmatter_json = tasknote_frontmatter_json(&planned.frontmatter);
    let changed_paths = vec![source_path.clone(), planned.relative_path.clone()];

    let routed = route_task_note_batch(
        paths,
        &[
            MdbaseManagedNoteWriteChange {
                path: &planned.relative_path,
                before: None,
                after: Some(&rendered_task),
            },
            MdbaseManagedNoteWriteChange {
                path: &source_path,
                before: Some(&source),
                after: Some(&updated_source),
            },
        ],
        dry_run,
        guard,
    )?;

    if !dry_run && !routed {
        write_ordinary_task_conversion_with_guard(
            paths,
            &source_path,
            &source,
            &updated_source,
            &planned.relative_path,
            &rendered_task,
            guard,
        )?;
    }

    Ok(TaskConvertReport {
        action: "convert".to_string(),
        dry_run,
        mode: "line".to_string(),
        source_path,
        target_path: planned.relative_path,
        line_number: Some(line_number),
        title: planned.title,
        created: true,
        source_changes: vec![source_change],
        task_changes: planned.task_changes,
        frontmatter: frontmatter_json,
        body: planned.body,
        changed_paths,
    })
}

#[cfg(test)]
fn write_ordinary_task_conversion(
    paths: &VaultPaths,
    source_path: &str,
    source_before: &str,
    source_after: &str,
    task_path: &str,
    task_contents: &str,
) -> Result<(), AppError> {
    write_ordinary_task_conversion_with_guard(
        paths,
        source_path,
        source_before,
        source_after,
        task_path,
        task_contents,
        None,
    )
}

fn write_ordinary_task_conversion_with_guard(
    paths: &VaultPaths,
    source_path: &str,
    source_before: &str,
    source_after: &str,
    task_path: &str,
    task_contents: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<(), AppError> {
    vulcan_core::initialize_vulcan_dir(paths).map_err(AppError::operation)?;
    vulcan_core::ordinary_write::apply_ordinary_write_batch_with_preflight(
        paths,
        &[
            OrdinaryWriteChange {
                path: task_path.to_string(),
                before: None,
                after: Some(task_contents.to_string()),
            },
            OrdinaryWriteChange {
                path: source_path.to_string(),
                before: Some(source_before.to_string()),
                after: Some(source_after.to_string()),
            },
        ],
        || {
            let profile = task_mutation_profile(paths, guard).map_err(|error| error.to_string())?;
            for path in [source_path, task_path] {
                if let Some(guard) = guard {
                    guard
                        .check_read_path(path)
                        .and_then(|()| guard.check_write_path(path))
                        .map_err(|error| error.to_string())?;
                }
                if crate::notes::note_path_is_mdbase_managed(paths, path, profile)
                    .map_err(|error| error.to_string())?
                {
                    return Err(
                        "mdbase collection changed during task conversion; retry the operation"
                            .into(),
                    );
                }
            }
            Ok(())
        },
    )
    .map(|_| ())
    .map_err(|error| AppError::operation_with_code(error.code, error.message))
}

fn read_existing_note_source(
    paths: &VaultPaths,
    note: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<(String, String), AppError> {
    let relative_path = resolve_task_mutation_path(paths, note, guard)?;
    let source = read_task_source(paths, &relative_path).map_err(AppError::operation)?;
    Ok((relative_path, source))
}

fn resolve_tasks_create_target(
    paths: &VaultPaths,
    note: Option<&str>,
) -> Result<(String, Option<String>), AppError> {
    if let Some(note) = note {
        return match resolve_note_reference(paths, note) {
            Ok(resolved) => Ok((resolved.path, None)),
            Err(GraphQueryError::AmbiguousIdentifier { .. }) => Err(AppError::operation(format!(
                "note identifier '{note}' is ambiguous"
            ))),
            Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. }) => {
                Ok((normalize_note_path(note)?, None))
            }
            Err(error) => Err(AppError::operation(error)),
        };
    }

    let config = load_vault_config(paths).config;
    Ok((
        normalize_note_path(&config.inbox.path)?,
        config.inbox.heading,
    ))
}

fn task_text_contains_tag(text: &str, tag: &str) -> bool {
    let normalized = normalize_tag_name(tag);
    text.split_whitespace()
        .any(|token| normalize_tag_name(token) == normalized)
}

fn normalize_tag_name(tag: &str) -> String {
    tag.trim().trim_start_matches('#').to_ascii_lowercase()
}

fn inline_task_priority_marker(config: &VaultConfig, priority: &str) -> Option<&'static str> {
    let normalized = priority.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "none" => None,
        "highest" => Some("🔺"),
        "high" | "urgent" => Some("⏫"),
        "medium" | "normal" => Some("🔼"),
        "low" => Some("🔽"),
        "lowest" => Some("⏬"),
        _ => config
            .tasknotes
            .priorities
            .iter()
            .find(|candidate| candidate.value.eq_ignore_ascii_case(priority))
            .and_then(|candidate| match candidate.weight {
                i32::MIN..=0 => None,
                1 => Some("🔽"),
                2 => Some("🔼"),
                3 => Some("⏫"),
                _ => Some("🔺"),
            }),
    }
}

#[allow(clippy::too_many_lines)]
fn build_inline_task_create_plan(
    config: &VaultConfig,
    text: &str,
    due: Option<&str>,
    priority: Option<&str>,
) -> Result<PlannedInlineTaskCreate, AppError> {
    let reference_ms = tasknote_reference_ms();
    let raw_text = text.trim();
    if raw_text.is_empty() {
        return Err(AppError::operation("task text cannot be empty"));
    }

    let used_nlp = config.tasknotes.enable_natural_language_input;
    let parsed_input = used_nlp
        .then(|| parse_tasknote_natural_language(raw_text, &config.tasknotes, reference_ms));
    let title = parsed_input
        .as_ref()
        .map(|parsed| parsed.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(raw_text)
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(AppError::operation("task title cannot be empty"));
    }

    let due = match due {
        Some(value) => Some(resolve_tasknote_date_input(config, value, false)?),
        None => parsed_input.as_ref().and_then(|parsed| parsed.due.clone()),
    };
    let scheduled = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.scheduled.clone());
    let priority = priority
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            parsed_input
                .as_ref()
                .and_then(|parsed| parsed.priority.clone())
        });
    if let Some(priority) = priority.as_deref() {
        if inline_task_priority_marker(config, priority).is_none() {
            return Err(AppError::operation(format!(
                "unsupported inline task priority: {priority}"
            )));
        }
    }

    let contexts = parsed_input
        .as_ref()
        .map_or_else(Vec::new, |parsed| parsed.contexts.clone());
    let projects = parsed_input
        .as_ref()
        .map_or_else(Vec::new, |parsed| parsed.projects.clone());
    let mut tags = parsed_input
        .as_ref()
        .map_or_else(Vec::new, |parsed| parsed.tags.clone());
    if let Some(global_filter) = config
        .tasks
        .global_filter
        .as_deref()
        .and_then(normalize_tasknote_tag)
    {
        if !tags
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&global_filter))
            && !task_text_contains_tag(&title, &global_filter)
        {
            tags.push(global_filter);
        }
    }
    tags = dedup_tasknote_values(tags, normalize_tasknote_tag);
    let recurrence = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.recurrence.clone());

    let mut tokens = vec![title.clone()];
    tokens.extend(contexts.iter().cloned());
    tokens.extend(tags.iter().map(|tag| format!("#{tag}")));
    tokens.extend(projects.iter().cloned());
    if let Some(due) = due.as_ref() {
        tokens.push(format!("📅 {due}"));
    }
    if let Some(scheduled) = scheduled.as_ref() {
        tokens.push(format!("⏳ {scheduled}"));
    }
    if config.tasks.set_created_date {
        tokens.push(format!("➕ {}", current_utc_date_string()));
    }
    if let Some(priority) = priority
        .as_deref()
        .and_then(|value| inline_task_priority_marker(config, value))
    {
        tokens.push(priority.to_string());
    }
    if let Some(recurrence) = recurrence.as_ref() {
        tokens.push(format!("🔁 {recurrence}"));
    }

    Ok(PlannedInlineTaskCreate {
        used_nlp,
        line: format!("- [ ] {}", tokens.join(" ")),
        due,
        scheduled,
        priority,
        recurrence,
        contexts,
        projects,
        tags,
    })
}

fn yaml_string_sequence(values: &[String]) -> YamlValue {
    YamlValue::Sequence(
        values
            .iter()
            .cloned()
            .map(YamlValue::String)
            .collect::<Vec<_>>(),
    )
}

fn tasknote_frontmatter_json(frontmatter: &YamlMapping) -> Value {
    serde_json::to_value(YamlValue::Mapping(frontmatter.clone())).unwrap_or(Value::Null)
}

fn sanitize_tasknote_filename(title: &str) -> String {
    let mut sanitized = title
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
            _ => character,
        })
        .collect::<String>();
    sanitized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    sanitized = sanitized.trim_matches(['.', ' ']).to_string();
    if sanitized.is_empty() {
        "Untitled Task".to_string()
    } else {
        sanitized
    }
}

fn load_tasknote_template(
    paths: &VaultPaths,
    config: &VaultConfig,
    template_name: &str,
    target_path: &str,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<(Option<YamlMapping>, String), AppError> {
    let loaded = if let Some(guard) = guard {
        crate::templates::load_named_template_with_guard(paths, config, template_name, guard)?
    } else {
        load_named_template(paths, config, template_name)?
    };
    let vars = HashMap::new();
    let read_filter = guard.map(PermissionGuard::read_filter);
    let rendered = crate::templates::render_loaded_template_with_authority(
        paths,
        config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path,
            target_contents: None,
            engine: TemplateEngineKind::Auto,
            vars: &vars,
            allow_mutations: !dry_run,
            run_mode: TemplateRunMode::Create,
        },
        read_filter.as_ref(),
        guard,
    )?;
    let (frontmatter, body) =
        parse_frontmatter_document(&rendered.content, true).map_err(AppError::operation)?;
    Ok((frontmatter, normalize_tasknote_body(&body)))
}

fn tasknote_title_from_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or("Untitled Task")
        .to_string()
}

fn prepare_existing_note_tasknote_frontmatter(
    frontmatter: &mut YamlMapping,
    title_hint: &str,
    config: &VaultConfig,
) -> Vec<RefactorChange> {
    let mapping = &config.tasknotes.field_mapping;
    let mut changes = Vec::new();

    let title_key = YamlValue::String(mapping.title.clone());
    let title = frontmatter
        .get(&title_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| title_hint.to_string());
    if let Some(change) =
        set_tasknote_frontmatter_value(frontmatter, &mapping.title, Some(YamlValue::String(title)))
    {
        changes.push(change);
    }

    let status_key = YamlValue::String(mapping.status.clone());
    let status = frontmatter
        .get(&status_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| config.tasknotes.default_status.clone());
    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.status,
        Some(YamlValue::String(status)),
    ) {
        changes.push(change);
    }

    let priority_key = YamlValue::String(mapping.priority.clone());
    let priority = frontmatter
        .get(&priority_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| config.tasknotes.default_priority.clone());
    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.priority,
        Some(YamlValue::String(priority)),
    ) {
        changes.push(change);
    }

    let created_key = YamlValue::String(mapping.date_created.clone());
    let date_created = frontmatter
        .get(&created_key)
        .and_then(yaml_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(current_utc_timestamp_string);
    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.date_created,
        Some(YamlValue::String(date_created)),
    ) {
        changes.push(change);
    }

    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        &mapping.date_modified,
        Some(YamlValue::String(current_utc_timestamp_string())),
    ) {
        changes.push(change);
    }

    if config.tasknotes.identification_method == vulcan_core::TaskNotesIdentificationMethod::Tag {
        let tags_key = YamlValue::String("tags".to_string());
        let mut tags = yaml_string_list(frontmatter.get(&tags_key));
        if let Some(task_tag) = normalize_tasknote_tag(&config.tasknotes.task_tag) {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&task_tag))
            {
                tags.insert(0, task_tag);
                if let Some(change) = set_tasknote_frontmatter_value(
                    frontmatter,
                    "tags",
                    Some(yaml_string_sequence(&tags)),
                ) {
                    changes.push(change);
                }
            }
        }
    } else if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
        let value = config
            .tasknotes
            .task_property_value
            .as_ref()
            .map_or(YamlValue::Bool(true), |value| {
                YamlValue::String(value.clone())
            });
        if let Some(change) =
            set_tasknote_frontmatter_value(frontmatter, property_name, Some(value))
        {
            changes.push(change);
        }
    }

    changes
}

fn tasknote_link_target(path: &str) -> String {
    path.strip_suffix(".md").unwrap_or(path).to_string()
}

fn extract_line_content_as_task_title(line: &str) -> String {
    let mut cleaned = line.trim().to_string();
    cleaned = Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s*\[[^\]]\]\s*")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    cleaned = Regex::new(r"^\s*[-*+]\s+")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    cleaned = Regex::new(r"^\s*\d+[.)]\s+")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    let blockquote_prefix = Regex::new(r"^\s*>\s*").expect("regex should compile");
    while cleaned.trim_start().starts_with('>') {
        cleaned = blockquote_prefix.replace(&cleaned, "").into_owned();
    }
    cleaned = Regex::new(r"^\s*#{1,6}\s+")
        .expect("regex should compile")
        .replace(&cleaned, "")
        .into_owned();
    if Regex::new(r"^\s*(?:-{3,}|={3,})\s*$")
        .expect("regex should compile")
        .is_match(&cleaned)
    {
        return String::new();
    }
    cleaned.trim().to_string()
}

fn line_replacement_prefix(line: &str) -> String {
    if let Some(captures) = Regex::new(r"^(\s*)((?:[-*+]|\d+[.)])\s+)\[[^\]]\]")
        .expect("regex should compile")
        .captures(line)
    {
        let indent = captures.get(1).map_or("", |capture| capture.as_str());
        let prefix = captures.get(2).map_or("- ", |capture| capture.as_str());
        return format!("{indent}{prefix}");
    }
    if let Some(captures) = Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+)")
        .expect("regex should compile")
        .captures(line)
    {
        return captures
            .get(1)
            .map_or("- ".to_string(), |capture| capture.as_str().to_string());
    }
    if let Some(captures) = Regex::new(r"^(\s*(?:>\s*)+)")
        .expect("regex should compile")
        .captures(line)
    {
        return captures
            .get(1)
            .map_or("> ".to_string(), |capture| capture.as_str().to_string());
    }
    "- ".to_string()
}

fn resolve_task_convert_line(
    source: &str,
    line_number: i64,
) -> Result<ResolvedTaskConvertLine, AppError> {
    let lines = source.split('\n').collect::<Vec<_>>();
    let index = usize::try_from(line_number.saturating_sub(1))
        .map_err(|_| AppError::operation(format!("invalid line number: {line_number}")))?;
    let line = lines
        .get(index)
        .copied()
        .ok_or_else(|| AppError::operation(format!("line {line_number} not found")))?;
    let heading = Regex::new(r"^\s*(#{1,6})\s+(.+?)\s*$").expect("regex should compile");
    if let Some(captures) = heading.captures(line) {
        let level = captures.get(1).map_or(0, |capture| capture.as_str().len());
        let title_input = captures
            .get(2)
            .map_or(String::new(), |capture| capture.as_str().trim().to_string());
        if title_input.is_empty() {
            return Err(AppError::operation(format!(
                "line {line_number} does not contain convertible heading text"
            )));
        }

        let mut end_index = index;
        for (candidate_index, candidate) in lines.iter().enumerate().skip(index + 1) {
            if let Some(next_heading) = heading.captures(candidate) {
                let next_level = next_heading
                    .get(1)
                    .map_or(0, |capture| capture.as_str().len());
                if next_level <= level {
                    break;
                }
            }
            end_index = candidate_index;
        }
        let details = lines
            .get(index + 1..=end_index)
            .map_or_else(String::new, |selected| selected.join("\n"));
        return Ok(ResolvedTaskConvertLine {
            start_line: line_number,
            end_line: i64::try_from(end_index + 1)
                .map_err(|_| AppError::operation("heading range exceeds supported size"))?,
            title_input,
            details,
            replacement_prefix: "- ".to_string(),
            completed: false,
        });
    }

    let title_input = extract_line_content_as_task_title(line);
    if title_input.is_empty() {
        return Err(AppError::operation(format!(
            "line {line_number} does not contain convertible task text"
        )));
    }

    let completed = Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s*\[[xX]\]")
        .expect("regex should compile")
        .is_match(line);
    Ok(ResolvedTaskConvertLine {
        start_line: line_number,
        end_line: line_number,
        title_input,
        details: String::new(),
        replacement_prefix: line_replacement_prefix(line),
        completed,
    })
}

fn replace_task_convert_line_range(
    source: &str,
    selection: &ResolvedTaskConvertLine,
    replacement_line: &str,
) -> Result<(String, RefactorChange), AppError> {
    let mut lines = source
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let start_index = usize::try_from(selection.start_line.saturating_sub(1))
        .map_err(|_| AppError::operation("invalid conversion start line"))?;
    let end_index = usize::try_from(selection.end_line.saturating_sub(1))
        .map_err(|_| AppError::operation("invalid conversion end line"))?;
    if start_index >= lines.len() || end_index >= lines.len() || start_index > end_index {
        return Err(AppError::operation(
            "conversion line range is out of bounds",
        ));
    }

    let before = lines[start_index..=end_index].join("\n");
    lines.splice(start_index..=end_index, [replacement_line.to_string()]);
    Ok((
        lines.join("\n"),
        RefactorChange {
            before,
            after: replacement_line.to_string(),
        },
    ))
}

#[allow(clippy::too_many_lines)]
fn build_converted_tasknote(
    paths: &VaultPaths,
    config: &VaultConfig,
    title_input: &str,
    details: &str,
    completed: bool,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<PlannedConvertedTaskNote, AppError> {
    let reference_ms = tasknote_reference_ms();
    let raw_title = title_input.trim();
    if raw_title.is_empty() {
        return Err(AppError::operation("task text cannot be empty"));
    }

    let parsed_input = config
        .tasknotes
        .enable_natural_language_input
        .then(|| parse_tasknote_natural_language(raw_title, &config.tasknotes, reference_ms));
    let title = parsed_input
        .as_ref()
        .map(|parsed| parsed.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(raw_title)
        .trim()
        .to_string();
    if title.is_empty() {
        return Err(AppError::operation("task title cannot be empty"));
    }

    let status = if completed {
        first_completed_tasknote_status(config)
    } else {
        parsed_input
            .as_ref()
            .and_then(|parsed| parsed.status.clone())
            .unwrap_or_else(|| config.tasknotes.default_status.clone())
    };
    let priority = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.priority.clone())
        .unwrap_or_else(|| config.tasknotes.default_priority.clone());
    let due = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.due.clone())
        .or_else(|| {
            tasknotes_default_date_value(
                config.tasknotes.task_creation_defaults.default_due_date,
                reference_ms,
            )
        });
    let scheduled = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.scheduled.clone())
        .or_else(|| {
            tasknotes_default_date_value(
                config
                    .tasknotes
                    .task_creation_defaults
                    .default_scheduled_date,
                reference_ms,
            )
        });
    let contexts = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_contexts
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.contexts.iter().cloned()),
            )
            .collect::<Vec<_>>(),
        normalize_tasknote_context,
    );
    let projects = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_projects
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.projects.iter().cloned()),
            )
            .collect::<Vec<_>>(),
        normalize_tasknote_project,
    );
    let mut tags = dedup_tasknote_values(
        config
            .tasknotes
            .task_creation_defaults
            .default_tags
            .iter()
            .cloned()
            .chain(
                parsed_input
                    .as_ref()
                    .into_iter()
                    .flat_map(|parsed| parsed.tags.iter().cloned()),
            )
            .collect::<Vec<_>>(),
        normalize_tasknote_tag,
    );
    if config.tasknotes.identification_method == vulcan_core::TaskNotesIdentificationMethod::Tag {
        if let Some(task_tag) = normalize_tasknote_tag(&config.tasknotes.task_tag) {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&task_tag))
            {
                tags.insert(0, task_tag);
            }
        }
    }
    let time_estimate = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.time_estimate)
        .or(config
            .tasknotes
            .task_creation_defaults
            .default_time_estimate);
    let recurrence = parsed_input
        .as_ref()
        .and_then(|parsed| parsed.recurrence.clone())
        .or_else(|| {
            tasknotes_default_recurrence_rule(
                config.tasknotes.task_creation_defaults.default_recurrence,
            )
        });

    let relative_path = format!(
        "{}/{}.md",
        config.tasknotes.tasks_folder.trim_end_matches('/'),
        sanitize_tasknote_filename(&title)
    );
    if let Some(guard) = guard {
        guard
            .check_read_path(&relative_path)
            .map_err(AppError::operation)?;
        guard
            .check_write_path(&relative_path)
            .map_err(AppError::operation)?;
    }
    if paths.vault_root().join(&relative_path).exists() {
        return Err(AppError::operation(format!(
            "destination task already exists: {relative_path}"
        )));
    }

    let mapping = &config.tasknotes.field_mapping;
    let timestamp = current_utc_timestamp_string();
    let mut frontmatter = YamlMapping::new();
    let mut task_changes = Vec::new();
    for (key, value) in [
        (
            mapping.title.as_str(),
            Some(YamlValue::String(title.clone())),
        ),
        (mapping.status.as_str(), Some(YamlValue::String(status))),
        (mapping.priority.as_str(), Some(YamlValue::String(priority))),
        (
            mapping.date_created.as_str(),
            Some(YamlValue::String(timestamp.clone())),
        ),
        (
            mapping.date_modified.as_str(),
            Some(YamlValue::String(timestamp)),
        ),
    ] {
        if let Some(change) = set_tasknote_frontmatter_value(&mut frontmatter, key, value) {
            task_changes.push(change);
        }
    }
    if let Some(due) = due {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.due,
            Some(YamlValue::String(due)),
        ) {
            task_changes.push(change);
        }
    }
    if let Some(scheduled) = scheduled {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.scheduled,
            Some(YamlValue::String(scheduled)),
        ) {
            task_changes.push(change);
        }
    }
    if !contexts.is_empty() {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.contexts,
            Some(yaml_string_sequence(&contexts)),
        ) {
            task_changes.push(change);
        }
    }
    if !projects.is_empty() {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.projects,
            Some(yaml_string_sequence(&projects)),
        ) {
            task_changes.push(change);
        }
    }
    if !tags.is_empty() {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            "tags",
            Some(yaml_string_sequence(&tags)),
        ) {
            task_changes.push(change);
        }
    }
    if let Some(time_estimate) = time_estimate {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.time_estimate,
            Some(YamlValue::Number(serde_yaml::Number::from(
                time_estimate as u64,
            ))),
        ) {
            task_changes.push(change);
        }
    }
    if let Some(recurrence) = recurrence {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.recurrence,
            Some(YamlValue::String(recurrence)),
        ) {
            task_changes.push(change);
        }
    }
    if let Some(reminders) = default_tasknote_reminders_yaml_value(config)? {
        if let Some(change) =
            set_tasknote_frontmatter_value(&mut frontmatter, &mapping.reminders, Some(reminders))
        {
            task_changes.push(change);
        }
    }
    if completed {
        if let Some(change) = set_tasknote_frontmatter_value(
            &mut frontmatter,
            &mapping.completed_date,
            Some(YamlValue::String(current_utc_date_string())),
        ) {
            task_changes.push(change);
        }
    }
    if config.tasknotes.identification_method
        == vulcan_core::TaskNotesIdentificationMethod::Property
    {
        if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
            let value = config
                .tasknotes
                .task_property_value
                .as_ref()
                .map_or(YamlValue::Bool(true), |value| {
                    YamlValue::String(value.clone())
                });
            if let Some(change) =
                set_tasknote_frontmatter_value(&mut frontmatter, property_name, Some(value))
            {
                task_changes.push(change);
            }
        }
    }

    Ok(PlannedConvertedTaskNote {
        relative_path,
        title,
        frontmatter,
        body: normalize_tasknote_body(details),
        task_changes,
    })
}

fn append_entry_to_note(contents: &str, entry: &str, heading: Option<&str>) -> NoteEntryInsertion {
    if let Some(heading) = heading {
        append_entry_under_heading(contents, heading, entry)
    } else {
        append_entry_at_end(contents, entry)
    }
}

fn append_entry_at_end(contents: &str, entry: &str) -> NoteEntryInsertion {
    let mut prefix = contents.trim_end_matches('\n').to_string();
    if !prefix.is_empty() {
        prefix.push_str("\n\n");
    }
    let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
        .expect("line count should fit in i64");
    let mut updated = prefix;
    updated.push_str(entry.trim_end());
    updated.push('\n');

    NoteEntryInsertion {
        updated,
        line_number,
        change: RefactorChange {
            before: String::new(),
            after: entry.trim_end().to_string(),
        },
    }
}

fn append_entry_under_heading(contents: &str, heading: &str, entry: &str) -> NoteEntryInsertion {
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
        let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
            .expect("line count should fit in i64");
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        if insert_at < contents.len() && !contents[insert_at..].starts_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&contents[insert_at..]);
        NoteEntryInsertion {
            updated,
            line_number,
            change: RefactorChange {
                before: String::new(),
                after: entry.trim_end().to_string(),
            },
        }
    } else {
        let mut prefix = contents.trim_end_matches('\n').to_string();
        if !prefix.is_empty() {
            prefix.push_str("\n\n");
        }
        prefix.push_str(heading);
        prefix.push_str("\n\n");
        let line_number = i64::try_from(prefix.lines().count().saturating_add(1))
            .expect("line count should fit in i64");
        let mut updated = prefix;
        updated.push_str(entry.trim_end());
        updated.push('\n');
        NoteEntryInsertion {
            updated,
            line_number,
            change: RefactorChange {
                before: String::new(),
                after: entry.trim_end().to_string(),
            },
        }
    }
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|ch| *ch == '#').count();
    (hashes > 0 && hashes <= 6 && line.chars().nth(hashes).is_some_and(char::is_whitespace))
        .then_some(hashes)
}

fn task_mutation_profile<'a>(
    paths: &VaultPaths,
    guard: Option<&'a ProfilePermissionGuard>,
) -> Result<Option<&'a str>, AppError> {
    let Some(guard) = guard else { return Ok(None) };
    let name = guard.selection().name.as_str();
    let current =
        vulcan_core::resolve_permission_profile(paths, Some(name)).map_err(AppError::operation)?;
    if &current != guard.selection() {
        return Err(AppError::operation_with_code(
            "permission_denied",
            "task mutation authority changed; resolve a new guard",
        ));
    }
    Ok(Some(name))
}

fn route_task_note_write(
    paths: &VaultPaths,
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
    operation: MdbaseWriteOperation,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<bool, AppError> {
    apply_managed_mdbase_note_write(
        paths,
        &MdbaseManagedNoteWriteRequest {
            path,
            before,
            after,
            operation,
            mode: MdbaseManagedWriteMode::Validated,
            dry_run,
            permission_profile: task_mutation_profile(paths, guard)?,
            quiet: true,
        },
    )
    .map(|report| report.is_some())
}

fn route_task_note_batch(
    paths: &VaultPaths,
    changes: &[MdbaseManagedNoteWriteChange<'_>],
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<bool, AppError> {
    let permission_profile = task_mutation_profile(paths, guard)?;
    let request = |dry_run| MdbaseManagedNoteWriteBatchRequest {
        changes,
        operation: MdbaseWriteOperation::Batch,
        mode: MdbaseManagedWriteMode::Validated,
        allow_mixed_paths: true,
        dry_run,
        permission_profile,
        quiet: true,
    };
    let routed = apply_managed_mdbase_note_writes(paths, &request(true))?.is_some();
    if !routed {
        return Ok(false);
    }
    if !dry_run {
        apply_managed_mdbase_note_writes(paths, &request(false))?
            .ok_or_else(|| AppError::operation("task write batch contained no managed records"))?;
    }
    Ok(true)
}

fn apply_tasknote_mutation<F>(
    paths: &VaultPaths,
    task: &str,
    action: &str,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
    mutate: F,
) -> Result<TaskMutationReport, AppError>
where
    F: FnOnce(&mut YamlMapping, &LoadedTaskNote) -> Result<TaskMutationPlan, AppError>,
{
    if guard.is_some() {
        // Recovery is a separate privileged workflow, not authority implicitly
        // borrowed by a scoped mutation from an earlier writer's journal.
        vulcan_core::ordinary_write::ensure_no_pending_ordinary_write_batch(paths)
            .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    } else if !dry_run {
        recover_pending_ordinary_task_write(paths)?;
    }
    let _ = task_mutation_profile(paths, guard)?;
    if let Some(guard) = guard {
        crate::mdbase::load_mdbase_routing_collection(paths, guard)?;
    }
    let loaded = load_tasknote_note_with_guard(paths, task, guard)?;
    apply_loaded_tasknote_mutation_with_guard(paths, &loaded, action, dry_run, guard, mutate)
}

fn apply_loaded_tasknote_mutation_with_guard<F>(
    paths: &VaultPaths,
    loaded: &LoadedTaskNote,
    action: &str,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
    mutate: F,
) -> Result<TaskMutationReport, AppError>
where
    F: FnOnce(&mut YamlMapping, &LoadedTaskNote) -> Result<TaskMutationPlan, AppError>,
{
    let mut frontmatter = loaded.frontmatter.clone();
    let TaskMutationPlan {
        mut changes,
        moved_to,
    } = mutate(&mut frontmatter, loaded)?;
    let moved_to = moved_to.filter(|path| path != &loaded.path);
    let rendered =
        render_note_from_parts(Some(&frontmatter), &loaded.body).map_err(AppError::operation)?;

    let mut changed_paths = Vec::new();
    if !changes.is_empty() || moved_to.is_some() {
        changed_paths.push(loaded.path.clone());
        if let Some(path) = moved_to.as_ref() {
            changed_paths.push(path.clone());
        }
    }
    authorize_task_changed_paths(&mut changed_paths, guard)?;

    let routed = if changed_paths.is_empty() {
        false
    } else if let Some(destination) = moved_to.as_ref() {
        let rename_changes = [
            MdbaseManagedNoteWriteChange {
                path: &loaded.path,
                before: Some(&loaded.source),
                after: None,
            },
            MdbaseManagedNoteWriteChange {
                path: destination,
                before: None,
                after: Some(&rendered),
            },
        ];
        apply_managed_mdbase_note_writes(
            paths,
            &MdbaseManagedNoteWriteBatchRequest {
                changes: &rename_changes,
                operation: MdbaseWriteOperation::Rename {
                    from: loaded.path.clone(),
                    to: destination.clone(),
                },
                mode: MdbaseManagedWriteMode::Validated,
                allow_mixed_paths: false,
                dry_run,
                permission_profile: task_mutation_profile(paths, guard)?,
                quiet: true,
            },
        )?
        .is_some()
    } else {
        route_task_note_write(
            paths,
            &loaded.path,
            Some(&loaded.source),
            Some(&rendered),
            MdbaseWriteOperation::Update,
            dry_run,
            guard,
        )?
    };

    if !dry_run && !changed_paths.is_empty() && !routed {
        if let Some(destination) = moved_to.as_ref() {
            move_ordinary_tasknote_if_unchanged(
                paths,
                &loaded.path,
                &loaded.source,
                destination,
                &rendered,
                task_mutation_profile(paths, guard)?,
            )?;
        } else {
            write_ordinary_note_if_unchanged_with_profile(
                paths,
                &loaded.path,
                Some(&loaded.source),
                &rendered,
                "task mutation",
                task_mutation_profile(paths, guard)?,
            )?;
        }
    }

    if changes.is_empty() && moved_to.is_some() {
        changes.push(RefactorChange {
            before: loaded.path.clone(),
            after: moved_to.clone().unwrap_or_else(|| loaded.path.clone()),
        });
    }

    Ok(TaskMutationReport {
        action: action.to_string(),
        dry_run,
        path: moved_to.clone().unwrap_or_else(|| loaded.path.clone()),
        moved_from: moved_to.as_ref().map(|_| loaded.path.clone()),
        moved_to,
        changes,
        changed_paths,
    })
}

fn move_ordinary_tasknote_if_unchanged(
    paths: &VaultPaths,
    source_path: &str,
    source_before: &str,
    destination_path: &str,
    destination_contents: &str,
    permission_profile: Option<&str>,
) -> Result<(), AppError> {
    vulcan_core::initialize_vulcan_dir(paths).map_err(AppError::operation)?;
    vulcan_core::ordinary_write::apply_ordinary_write_batch_with_preflight(
        paths,
        &[
            OrdinaryWriteChange {
                path: destination_path.to_string(),
                before: None,
                after: Some(destination_contents.to_string()),
            },
            OrdinaryWriteChange {
                path: source_path.to_string(),
                before: Some(source_before.to_string()),
                after: None,
            },
        ],
        || {
            for path in [source_path, destination_path] {
                if crate::notes::note_path_is_mdbase_managed(paths, path, permission_profile)
                    .map_err(|error| error.to_string())?
                {
                    return Err(
                        "mdbase collection changed during task move; retry the operation".into(),
                    );
                }
            }
            Ok(())
        },
    )
    .map(|_| ())
    .map_err(|error| AppError::operation_with_code(error.code, error.message))
}

fn recover_pending_ordinary_task_write(paths: &VaultPaths) -> Result<(), AppError> {
    if paths.vulcan_dir().exists() {
        recover_ordinary_write_batch(paths)
            .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    }
    Ok(())
}

fn read_task_source(paths: &VaultPaths, path: &str) -> Result<String, std::io::Error> {
    let mut file =
        vulcan_core::paths::secure_open_regular_read(paths.vault_root(), Path::new(path))?;
    let mut source = String::new();
    file.read_to_string(&mut source)?;
    Ok(source)
}

fn load_tasknote_note(paths: &VaultPaths, task: &str) -> Result<LoadedTaskNote, AppError> {
    load_tasknote_note_with_guard(paths, task, None)
}

fn load_tasknote_note_with_guard(
    paths: &VaultPaths,
    task: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<LoadedTaskNote, AppError> {
    let path = resolve_task_mutation_path(paths, task, guard)?;
    let source = read_task_source(paths, &path).map_err(AppError::operation)?;
    let read_filter = guard.map(PermissionGuard::read_filter);
    let cached_properties =
        vulcan_core::properties::load_note_index_with_filter(paths, read_filter.as_ref())
            .ok()
            .and_then(|index| {
                index
                    .into_values()
                    .find(|note| note.document_path == path)
                    .map(|note| note.properties)
            });
    parse_loaded_tasknote(paths, task, path, source, cached_properties, None)
}

fn parse_loaded_tasknote(
    paths: &VaultPaths,
    task: &str,
    path: String,
    source: String,
    cached_properties: Option<Value>,
    read_filter: Option<&vulcan_core::PermissionFilter>,
) -> Result<LoadedTaskNote, AppError> {
    let config = load_vault_config(paths).config;
    let parsed = vulcan_core::parse_document(&source, &config);
    if let Some(filter) = read_filter {
        let tags = parsed
            .tags
            .iter()
            .map(|tag| tag.tag_text.clone())
            .collect::<Vec<_>>();
        if !filter.path_permission().is_allowed_with_tags(&path, &tags) {
            return Err(AppError::operation_with_code(
                "permission_denied",
                "task source is no longer readable under the selected grant",
            ));
        }
    }
    let indexed_properties = extract_indexed_properties(&parsed, &config)
        .map_err(AppError::operation)?
        .map(|properties| serde_json::from_str::<Value>(&properties.canonical_json))
        .transpose()
        .map_err(AppError::operation)?;
    let (frontmatter, body) =
        parse_frontmatter_document(&source, false).map_err(AppError::operation)?;
    let frontmatter = frontmatter.unwrap_or_default();
    let frontmatter_json = cached_properties
        .or(indexed_properties)
        .unwrap_or_else(|| Value::Object(Map::new()));
    let title = Path::new(&path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let indexed =
        extract_tasknote(&path, title, &frontmatter_json, &config.tasknotes).or_else(|| {
            let mut permissive = config.tasknotes.clone();
            permissive.excluded_folders.clear();
            extract_tasknote(&path, title, &frontmatter_json, &permissive)
        });
    let indexed = indexed
        .ok_or_else(|| AppError::operation(format!("note is not a TaskNotes task: {task}")))?;

    Ok(LoadedTaskNote {
        path,
        source,
        body: normalize_tasknote_body(&body),
        frontmatter,
        frontmatter_json,
        indexed,
        config,
    })
}

fn normalize_tasknote_body(body: &str) -> String {
    let body = body.trim_start_matches('\n').trim_end_matches('\n');
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

fn current_utc_timestamp_ms() -> i64 {
    vulcan_core::current_utc_timestamp_ms()
}

fn format_utc_timestamp_ms(ms: i64) -> String {
    TemplateTimestamp::from_millis(ms)
        .default_strings()
        .datetime
}

fn parse_rounded_i64(value: f64) -> Option<i64> {
    format!("{value:.0}").parse::<i64>().ok()
}

fn parse_f64_value(value: i64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

fn parse_task_pomodoro_session_json(value: &Value) -> Option<TaskPomodoroSession> {
    serde_json::from_value(value.clone()).ok()
}

fn parse_task_pomodoro_session_yaml(value: &YamlValue) -> Option<TaskPomodoroSession> {
    serde_yaml::from_value(value.clone()).ok()
}

fn task_pomodoro_session_yaml_value(session: &TaskPomodoroSession) -> Result<YamlValue, AppError> {
    serde_yaml::to_value(session).map_err(AppError::operation)
}

fn task_pomodoro_elapsed_minutes(session: &TaskPomodoroSession, now_ms: i64) -> i64 {
    let Some(start_ms) = parse_date_like_string(&session.start_time) else {
        return 0;
    };
    let end_ms = session
        .end_time
        .as_deref()
        .and_then(parse_date_like_string)
        .unwrap_or(now_ms);
    end_ms.saturating_sub(start_ms).div_euclid(60_000)
}

fn task_pomodoro_remaining_seconds(session: &TaskPomodoroSession, now_ms: i64) -> i64 {
    let Some(start_ms) = parse_date_like_string(&session.start_time) else {
        return 0;
    };
    let planned_ms = i64::try_from(session.planned_duration)
        .unwrap_or(i64::MAX)
        .saturating_mul(60_000);
    let elapsed_ms = now_ms.saturating_sub(start_ms);
    planned_ms.saturating_sub(elapsed_ms).div_euclid(1_000)
}

fn task_pomodoro_due_completion_ms(session: &TaskPomodoroSession) -> Option<i64> {
    let start_ms = parse_date_like_string(&session.start_time)?;
    let planned_ms = i64::try_from(session.planned_duration)
        .unwrap_or(i64::MAX)
        .saturating_mul(60_000);
    Some(start_ms.saturating_add(planned_ms))
}

fn finalize_task_pomodoro_session(
    session: &TaskPomodoroSession,
    end_time: &str,
    completed: bool,
    interrupted: bool,
) -> TaskPomodoroSession {
    let mut updated = session.clone();
    updated.end_time = Some(end_time.to_string());
    updated.completed = completed;
    updated.interrupted = interrupted;
    if let Some(period) = updated
        .active_periods
        .iter_mut()
        .rev()
        .find(|period| period.end_time.is_none())
    {
        period.end_time = Some(end_time.to_string());
    }
    updated
}

fn task_pomodoro_session_report(
    session: &TaskPomodoroSession,
    now_ms: i64,
) -> TaskPomodoroSessionReport {
    TaskPomodoroSessionReport {
        id: session.id.clone(),
        session_type: session.session_type.clone(),
        start_time: session.start_time.clone(),
        end_time: session.end_time.clone(),
        planned_duration_minutes: session.planned_duration,
        elapsed_minutes: task_pomodoro_elapsed_minutes(session, now_ms),
        remaining_seconds: task_pomodoro_remaining_seconds(session, now_ms).max(0),
        completed: session.completed,
        interrupted: session.interrupted,
        active: session.end_time.is_none(),
    }
}

fn suggested_task_pomodoro_break(
    config: &VaultConfig,
    completed_work_sessions: usize,
) -> (String, usize) {
    let interval = config.tasknotes.pomodoro.long_break_interval.max(1);
    if completed_work_sessions > 0 && completed_work_sessions.is_multiple_of(interval) {
        (
            "long-break".to_string(),
            config.tasknotes.pomodoro.long_break.max(1),
        )
    } else {
        (
            "short-break".to_string(),
            config.tasknotes.pomodoro.short_break.max(1),
        )
    }
}

fn completed_work_task_pomodoros(sessions: &[StoredPomodoroSession]) -> usize {
    sessions
        .iter()
        .filter(|stored| stored.session.session_type == "work" && stored.session.completed)
        .count()
}

fn tasknote_estimate_minutes(task: &IndexedTaskNote) -> Option<i64> {
    task.time_estimate.and_then(|minutes| {
        minutes
            .is_finite()
            .then(|| parse_rounded_i64(minutes))
            .flatten()
            .filter(|minutes| *minutes > 0)
    })
}

fn tasknote_time_metrics(
    task: &IndexedTaskNote,
    now_ms: i64,
) -> (i64, i64, Option<i64>, Option<i64>) {
    let entries = parse_tasknote_time_entries(&task.time_entries, now_ms);
    let total_time_minutes = entries
        .iter()
        .map(|entry| entry.duration_minutes)
        .sum::<i64>();
    let active_time_minutes = entries
        .iter()
        .filter(|entry| entry.is_active)
        .map(|entry| entry.duration_minutes)
        .sum::<i64>();
    let estimate_remaining_minutes =
        tasknote_estimate_minutes(task).map(|estimate| estimate.saturating_sub(total_time_minutes));
    let efficiency_ratio = tasknote_estimate_minutes(task).map(|estimate| {
        if estimate <= 0 {
            0
        } else {
            parse_rounded_i64(
                (parse_f64_value(total_time_minutes) / parse_f64_value(estimate)) * 100.0,
            )
            .unwrap_or_default()
        }
    });

    (
        total_time_minutes,
        active_time_minutes,
        estimate_remaining_minutes,
        efficiency_ratio,
    )
}

fn task_time_entry_report(entry: vulcan_core::TaskNotesTimeEntry) -> TaskTimeEntryReport {
    TaskTimeEntryReport {
        start_time: entry.start_time,
        end_time: entry.end_time,
        description: entry.description,
        duration_minutes: entry.duration_minutes,
        active: entry.is_active,
    }
}

fn load_tasknote_records_with_filter(
    paths: &VaultPaths,
    filter: Option<&vulcan_core::PermissionFilter>,
) -> Result<Vec<TaskNoteRecord>, AppError> {
    load_tasknote_records_with_scope(paths, TaskReadScope::Filter(filter))
}

fn load_tasknote_records_with_scope(
    paths: &VaultPaths,
    scope: TaskReadScope<'_>,
) -> Result<Vec<TaskNoteRecord>, AppError> {
    let config = load_vault_config(paths).config;
    let note_index = scope.load(paths)?;
    let mut records = note_index
        .into_values()
        .filter_map(|note| {
            let title = Path::new(&note.document_path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default();
            let indexed = extract_tasknote(
                &note.document_path,
                title,
                &note.properties,
                &config.tasknotes,
            )?;
            let completed = tasknotes_status_state(&config.tasknotes, &indexed.status).completed;
            Some(TaskNoteRecord {
                path: note.document_path,
                indexed,
                completed,
            })
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(records)
}

fn load_note_frontmatter_for_mutation(
    paths: &VaultPaths,
    relative_path: &str,
    create_periodic: Option<&str>,
    guard: Option<&ProfilePermissionGuard>,
    dry_run: bool,
) -> Result<LoadedNoteMutation, AppError> {
    let (source, created) = match read_task_source(paths, relative_path) {
        Ok(source) => (source, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(period_type) = create_periodic else {
                return Err(AppError::operation(format!(
                    "note not found: {relative_path}"
                )));
            };
            let mut warnings = Vec::new();
            (
                render_periodic_note_contents_with_guard(
                    paths,
                    period_type,
                    relative_path,
                    &mut warnings,
                    guard,
                    dry_run,
                )?,
                true,
            )
        }
        Err(error) => return Err(AppError::operation(error)),
    };

    let (frontmatter, body) =
        parse_frontmatter_document(&source, false).map_err(AppError::operation)?;
    Ok(LoadedNoteMutation {
        path: relative_path.to_string(),
        source,
        body: normalize_tasknote_body(&body),
        frontmatter: frontmatter.unwrap_or_default(),
        created,
    })
}

#[cfg(test)]
fn apply_note_frontmatter_mutation<F>(
    paths: &VaultPaths,
    relative_path: &str,
    create_periodic: Option<&str>,
    action: &str,
    dry_run: bool,
    mutate: F,
) -> Result<TaskMutationReport, AppError>
where
    F: FnOnce(&mut YamlMapping, &LoadedNoteMutation) -> Result<Vec<RefactorChange>, AppError>,
{
    apply_note_frontmatter_mutation_with_guard(
        paths,
        relative_path,
        create_periodic,
        action,
        dry_run,
        None,
        mutate,
    )
}

fn apply_note_frontmatter_mutation_with_guard<F>(
    paths: &VaultPaths,
    relative_path: &str,
    create_periodic: Option<&str>,
    action: &str,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
    mutate: F,
) -> Result<TaskMutationReport, AppError>
where
    F: FnOnce(&mut YamlMapping, &LoadedNoteMutation) -> Result<Vec<RefactorChange>, AppError>,
{
    preflight_task_mutation_guard(paths, guard)?;
    if let Some(guard) = guard {
        guard
            .check_read_path(relative_path)
            .map_err(AppError::operation)?;
        guard
            .check_write_path(relative_path)
            .map_err(AppError::operation)?;
    }
    let loaded =
        load_note_frontmatter_for_mutation(paths, relative_path, create_periodic, guard, dry_run)?;
    let mut frontmatter = loaded.frontmatter.clone();
    let mut changes = mutate(&mut frontmatter, &loaded)?;
    let rendered =
        render_note_from_parts(Some(&frontmatter), &loaded.body).map_err(AppError::operation)?;

    let has_writes = loaded.created || !changes.is_empty();
    let changed_paths = if has_writes {
        vec![loaded.path.clone()]
    } else {
        Vec::new()
    };

    let routed = if has_writes {
        route_task_note_write(
            paths,
            &loaded.path,
            (!loaded.created).then_some(loaded.source.as_str()),
            Some(&rendered),
            if loaded.created {
                MdbaseWriteOperation::Create
            } else {
                MdbaseWriteOperation::Update
            },
            dry_run,
            guard,
        )?
    } else {
        false
    };

    if !dry_run && has_writes && !routed {
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &loaded.path,
            (!loaded.created).then_some(loaded.source.as_str()),
            &rendered,
            action,
            task_mutation_profile(paths, guard)?,
        )?;
    }

    if loaded.created {
        changes.insert(
            0,
            RefactorChange {
                before: "<missing>".to_string(),
                after: loaded.path.clone(),
            },
        );
    }

    Ok(TaskMutationReport {
        action: action.to_string(),
        dry_run,
        path: loaded.path,
        moved_from: None,
        moved_to: None,
        changes,
        changed_paths,
    })
}

fn pomodoro_session_is_readable(
    stored: &StoredPomodoroSession,
    guard: Option<&ProfilePermissionGuard>,
) -> bool {
    guard.is_none_or(|guard| {
        guard.check_read_path(&stored.storage_note_path).is_ok()
            && stored
                .task_path
                .as_ref()
                .is_none_or(|path| guard.check_read_path(path).is_ok())
    })
}

fn collect_tasknotes_pomodoro_sessions(
    paths: &VaultPaths,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<Vec<StoredPomodoroSession>, AppError> {
    let config = load_vault_config(paths).config;
    let filter = guard.map(PermissionGuard::read_filter);
    let note_index = vulcan_core::properties::load_note_index_with_filter(paths, filter.as_ref())
        .map_err(AppError::operation)?;
    let field_name = config.tasknotes.field_mapping.pomodoros.clone();
    let mut task_titles = HashMap::new();
    let mut task_sessions = Vec::new();
    let mut daily_sessions = Vec::new();

    for note in note_index.values() {
        let title = Path::new(&note.document_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if let Some(tasknote) = extract_tasknote(
            &note.document_path,
            title,
            &note.properties,
            &config.tasknotes,
        ) {
            task_titles.insert(note.document_path.clone(), tasknote.title.clone());

            if let Some(values) = note
                .properties
                .as_object()
                .and_then(|object| object.get(&field_name))
                .and_then(Value::as_array)
            {
                for session in values.iter().filter_map(parse_task_pomodoro_session_json) {
                    task_sessions.push(StoredPomodoroSession {
                        storage_note_path: note.document_path.clone(),
                        task_path: Some(note.document_path.clone()),
                        title: Some(tasknote.title.clone()),
                        session,
                    });
                }
            }
        }

        if note.periodic_type.as_deref() == Some("daily") {
            if let Some(values) = note
                .properties
                .as_object()
                .and_then(|object| object.get(&field_name))
                .and_then(Value::as_array)
            {
                for session in values.iter().filter_map(parse_task_pomodoro_session_json) {
                    daily_sessions.push(StoredPomodoroSession {
                        storage_note_path: note.document_path.clone(),
                        task_path: session.task_path.clone(),
                        title: session
                            .task_path
                            .as_ref()
                            .and_then(|path| task_titles.get(path).cloned()),
                        session,
                    });
                }
            }
        }
    }

    let mut sessions = match config.tasknotes.pomodoro.storage_location {
        vulcan_core::config::TaskNotesPomodoroStorageLocation::Task => task_sessions,
        vulcan_core::config::TaskNotesPomodoroStorageLocation::DailyNote => daily_sessions,
    };
    sessions.retain(|stored| pomodoro_session_is_readable(stored, guard));
    sessions.sort_by(|left, right| {
        left.storage_note_path
            .cmp(&right.storage_note_path)
            .then_with(|| left.session.start_time.cmp(&right.session.start_time))
            .then_with(|| left.session.id.cmp(&right.session.id))
    });
    Ok(sessions)
}

fn collect_tasknotes_pomodoro_sessions_with_overrides(
    paths: &VaultPaths,
    changed_paths: &[String],
    guard: Option<&ProfilePermissionGuard>,
) -> Result<Vec<StoredPomodoroSession>, AppError> {
    let config = load_vault_config(paths).config;
    let mut sessions = collect_tasknotes_pomodoro_sessions(paths, guard)?;
    if changed_paths.is_empty() {
        return Ok(sessions);
    }

    let filter = guard.map(PermissionGuard::read_filter);
    let task_titles = load_tasknote_records_with_filter(paths, filter.as_ref())?
        .into_iter()
        .map(|record| (record.path, record.indexed.title))
        .collect::<HashMap<_, _>>();

    sessions.retain(|stored| {
        !changed_paths
            .iter()
            .any(|path| path == &stored.storage_note_path)
    });
    for path in changed_paths {
        if let Some(guard) = guard {
            guard.check_read_path(path).map_err(AppError::operation)?;
        }
        sessions.extend(load_stored_pomodoro_sessions_from_path(
            paths,
            &config,
            path,
            &task_titles,
            guard,
        )?);
    }
    sessions.retain(|stored| pomodoro_session_is_readable(stored, guard));
    sessions.sort_by(|left, right| {
        left.storage_note_path
            .cmp(&right.storage_note_path)
            .then_with(|| left.session.start_time.cmp(&right.session.start_time))
            .then_with(|| left.session.id.cmp(&right.session.id))
    });
    Ok(sessions)
}

fn load_stored_pomodoro_sessions_from_path(
    paths: &VaultPaths,
    config: &VaultConfig,
    relative_path: &str,
    task_titles: &HashMap<String, String>,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<Vec<StoredPomodoroSession>, AppError> {
    let source = match read_task_source(paths, relative_path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::operation(error)),
    };
    let (frontmatter, _) =
        parse_frontmatter_document(&source, false).map_err(AppError::operation)?;
    let frontmatter = frontmatter.unwrap_or_default();
    let pomodoros_key = YamlValue::String(config.tasknotes.field_mapping.pomodoros.clone());
    let sessions = yaml_sequence_value(frontmatter.get(&pomodoros_key))?;
    if sessions.is_empty() {
        return Ok(Vec::new());
    }

    if let Ok(loaded) = load_tasknote_note_with_guard(paths, relative_path, guard) {
        return Ok(sessions
            .into_iter()
            .filter_map(|value| parse_task_pomodoro_session_yaml(&value))
            .map(|session| StoredPomodoroSession {
                storage_note_path: relative_path.to_string(),
                task_path: Some(relative_path.to_string()),
                title: Some(loaded.indexed.title.clone()),
                session,
            })
            .collect());
    }

    Ok(sessions
        .into_iter()
        .filter_map(|value| parse_task_pomodoro_session_yaml(&value))
        .map(|session| StoredPomodoroSession {
            storage_note_path: relative_path.to_string(),
            task_path: session.task_path.clone(),
            title: session
                .task_path
                .as_ref()
                .and_then(|path| task_titles.get(path).cloned()),
            session,
        })
        .collect())
}

fn yaml_sequence_value(value: Option<&YamlValue>) -> Result<Vec<YamlValue>, AppError> {
    match value {
        None | Some(YamlValue::Null) => Ok(Vec::new()),
        Some(YamlValue::Sequence(items)) => Ok(items.clone()),
        Some(_) => Err(AppError::operation(
            "TaskNotes frontmatter value must be a YAML sequence",
        )),
    }
}

fn tasknote_time_entry_yaml_value(
    start_time: &str,
    end_time: Option<&str>,
    description: Option<&str>,
) -> YamlValue {
    let mut mapping = YamlMapping::new();
    mapping.insert(
        YamlValue::String("startTime".to_string()),
        YamlValue::String(start_time.to_string()),
    );
    if let Some(end_time) = end_time {
        mapping.insert(
            YamlValue::String("endTime".to_string()),
            YamlValue::String(end_time.to_string()),
        );
    }
    if let Some(description) = description.filter(|description| !description.trim().is_empty()) {
        mapping.insert(
            YamlValue::String("description".to_string()),
            YamlValue::String(description.to_string()),
        );
    }
    YamlValue::Mapping(mapping)
}

fn resolve_active_task_pomodoro_session(
    paths: &VaultPaths,
    task: Option<&str>,
    guard: Option<&ProfilePermissionGuard>,
    changed_paths: &[String],
) -> Result<Option<StoredPomodoroSession>, AppError> {
    let sessions = collect_tasknotes_pomodoro_sessions_with_overrides(paths, changed_paths, guard)?;
    let active_sessions = sessions
        .into_iter()
        .filter(|stored| stored.session.end_time.is_none())
        .collect::<Vec<_>>();

    if let Some(task) = task {
        let task_path = load_tasknote_note_with_guard(paths, task, guard)?.path;
        let mut matches = active_sessions
            .into_iter()
            .filter(|stored| stored.task_path.as_deref() == Some(task_path.as_str()))
            .collect::<Vec<_>>();
        return match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.pop()),
            _ => Err(AppError::operation(
                "multiple active TaskNotes pomodoro sessions match that task",
            )),
        };
    }

    match active_sessions.len() {
        0 => Ok(None),
        1 => Ok(active_sessions.into_iter().next()),
        _ => Err(AppError::operation(
            "multiple active TaskNotes pomodoro sessions; specify the task to stop",
        )),
    }
}

fn task_pomodoro_storage_target_path(
    config: &VaultConfig,
    task_path: &str,
    now_ms: i64,
) -> Result<String, AppError> {
    match config.tasknotes.pomodoro.storage_location {
        vulcan_core::config::TaskNotesPomodoroStorageLocation::Task => Ok(task_path.to_string()),
        vulcan_core::config::TaskNotesPomodoroStorageLocation::DailyNote => {
            let daily = config.periodic.note("daily").ok_or_else(|| {
                AppError::operation("daily periodic note configuration is missing")
            })?;
            if !daily.enabled {
                return Err(AppError::operation(
                    "tasknotes pomodoro daily-note storage requires periodic.daily.enabled = true",
                ));
            }
            let date = TemplateTimestamp::from_millis(now_ms).default_date_string();
            expected_periodic_note_path(&config.periodic, "daily", &date).ok_or_else(|| {
                AppError::operation("failed to resolve the daily note path for pomodoro storage")
            })
        }
    }
}

fn update_pomodoro_session_sequence(
    frontmatter: &mut YamlMapping,
    key: &str,
    update: impl FnOnce(&mut Vec<YamlValue>) -> Result<(), AppError>,
) -> Result<Option<RefactorChange>, AppError> {
    let yaml_key = YamlValue::String(key.to_string());
    let before = frontmatter.get(&yaml_key).cloned();
    let mut sessions = yaml_sequence_value(before.as_ref())?;
    update(&mut sessions)?;
    Ok(set_tasknote_frontmatter_value(
        frontmatter,
        key,
        Some(YamlValue::Sequence(sessions)),
    ))
}

#[allow(clippy::too_many_lines)]
fn process_due_task_pomodoros(
    paths: &VaultPaths,
    dry_run: bool,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<Vec<String>, AppError> {
    let now_ms = current_utc_timestamp_ms();
    let config = load_vault_config(paths).config;
    let due_sessions = collect_tasknotes_pomodoro_sessions(paths, guard)?
        .into_iter()
        .filter_map(|stored| {
            let due_ms = task_pomodoro_due_completion_ms(&stored.session)?;
            (stored.session.end_time.is_none() && now_ms >= due_ms).then_some((stored, due_ms))
        })
        .collect::<Vec<_>>();

    let mut changed_paths = Vec::new();
    for (stored, due_ms) in due_sessions {
        let finished_at = format_utc_timestamp_ms(due_ms);
        let key = config.tasknotes.field_mapping.pomodoros.clone();
        let mutation = if matches!(
            config.tasknotes.pomodoro.storage_location,
            vulcan_core::config::TaskNotesPomodoroStorageLocation::Task
        ) {
            let loaded = load_tasknote_note_with_guard(paths, &stored.storage_note_path, guard)?;
            apply_loaded_tasknote_mutation_with_guard(
                paths,
                &loaded,
                "pomodoro_complete",
                dry_run,
                guard,
                |frontmatter, loaded| {
                    let mut changes = Vec::new();
                    if let Some(change) = update_pomodoro_session_sequence(
                        frontmatter,
                        &loaded.config.tasknotes.field_mapping.pomodoros,
                        |sessions| {
                            let Some((index, _)) =
                                sessions.iter().enumerate().rev().find(|(_, value)| {
                                    parse_task_pomodoro_session_yaml(value).is_some_and(|session| {
                                        session.id == stored.session.id
                                            && session.end_time.is_none()
                                    })
                                })
                            else {
                                return Err(AppError::operation(format!(
                                    "failed to locate the active pomodoro session in {path}",
                                    path = loaded.path
                                )));
                            };
                            let updated = finalize_task_pomodoro_session(
                                &stored.session,
                                &finished_at,
                                true,
                                false,
                            );
                            sessions[index] = task_pomodoro_session_yaml_value(&updated)?;
                            Ok(())
                        },
                    )? {
                        changes.push(change);
                    }
                    if let Some(change) = set_tasknote_frontmatter_value(
                        frontmatter,
                        &loaded.config.tasknotes.field_mapping.date_modified,
                        Some(YamlValue::String(current_utc_timestamp_string())),
                    ) {
                        changes.push(change);
                    }
                    Ok(TaskMutationPlan {
                        changes,
                        moved_to: None,
                    })
                },
            )?
        } else {
            apply_note_frontmatter_mutation_with_guard(
                paths,
                &stored.storage_note_path,
                Some("daily"),
                "pomodoro_complete",
                dry_run,
                guard,
                |frontmatter, _loaded| {
                    let mut changes = Vec::new();
                    if let Some(change) = update_pomodoro_session_sequence(
                        frontmatter,
                        &key,
                        |sessions| {
                            let Some((index, _)) =
                                sessions.iter().enumerate().rev().find(|(_, value)| {
                                    parse_task_pomodoro_session_yaml(value).is_some_and(|session| {
                                        session.id == stored.session.id
                                            && session.end_time.is_none()
                                    })
                                })
                            else {
                                return Err(AppError::operation(format!(
                                    "failed to locate the active pomodoro session in {storage_note_path}",
                                    storage_note_path = stored.storage_note_path
                                )));
                            };
                            let updated = finalize_task_pomodoro_session(
                                &stored.session,
                                &finished_at,
                                true,
                                false,
                            );
                            sessions[index] = task_pomodoro_session_yaml_value(&updated)?;
                            Ok(())
                        },
                    )? {
                        changes.push(change);
                    }
                    Ok(changes)
                },
            )?
        };
        changed_paths.extend(mutation.changed_paths);
    }

    changed_paths.sort();
    changed_paths.dedup();
    Ok(changed_paths)
}

fn resolve_task_track_summary_window(
    config: &VaultConfig,
    period: TaskTrackSummaryPeriod,
) -> Result<(String, String, i64, i64), AppError> {
    let now_ms = current_utc_timestamp_ms();
    let today = current_utc_date_string();

    let (from, to) = match period {
        TaskTrackSummaryPeriod::Day => (today.clone(), today),
        TaskTrackSummaryPeriod::Week => period_range_for_date(&config.periodic, "weekly", &today)
            .map(|(from, _)| (from, today))
            .ok_or_else(|| AppError::operation("failed to resolve the current weekly period"))?,
        TaskTrackSummaryPeriod::Month => period_range_for_date(&config.periodic, "monthly", &today)
            .map(|(from, _)| (from, today))
            .ok_or_else(|| AppError::operation("failed to resolve the current monthly period"))?,
        TaskTrackSummaryPeriod::All => ("1970-01-01".to_string(), today),
    };

    let from_ms = parse_date_like_string(&from).ok_or_else(|| {
        AppError::operation(format!("failed to parse summary start date: {from}"))
    })?;
    Ok((from, to, from_ms, now_ms))
}

fn resolve_active_tasknote_record(
    paths: &VaultPaths,
    task: Option<&str>,
    now_ms: i64,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskNoteRecord, AppError> {
    if let Some(task) = task {
        let loaded = load_tasknote_note_with_guard(paths, task, guard)?;
        return Ok(TaskNoteRecord {
            path: loaded.path,
            completed: tasknotes_status_state(&loaded.config.tasknotes, &loaded.indexed.status)
                .completed,
            indexed: loaded.indexed,
        });
    }

    let read_filter = guard.map(PermissionGuard::read_filter);
    let active_records = load_tasknote_records_with_filter(paths, read_filter.as_ref())?
        .into_iter()
        .filter(|record| active_tasknote_time_entry(&record.indexed.time_entries, now_ms).is_some())
        .collect::<Vec<_>>();
    match active_records.len() {
        0 => Err(AppError::operation(
            "no active TaskNotes time tracking sessions",
        )),
        1 => Ok(active_records
            .into_iter()
            .next()
            .unwrap_or_else(|| unreachable!())),
        _ => Err(AppError::operation(
            "multiple active TaskNotes time tracking sessions; specify the task to stop",
        )),
    }
}

fn preflight_task_mutation_guard(
    paths: &VaultPaths,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<(), AppError> {
    if let Some(guard) = guard {
        task_mutation_profile(paths, Some(guard))?;
        crate::mdbase::load_mdbase_routing_collection(paths, guard)?;
        vulcan_core::ordinary_write::ensure_no_pending_ordinary_write_batch(paths)
            .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    }
    Ok(())
}

fn resolve_task_mutation_path(
    paths: &VaultPaths,
    task: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<String, AppError> {
    let Some(guard) = guard else {
        return resolve_existing_note_path(paths, task);
    };
    let path = match vulcan_core::resolve_note_reference_with_filter(
        paths,
        task,
        Some(&guard.read_filter()),
    ) {
        Ok(note) => note.path,
        Err(GraphQueryError::CacheMissing | GraphQueryError::NoteNotFound { .. }) => {
            // Authorize even missing paths before inspecting their disk state.
            normalize_note_path(task)?
        }
        Err(error) => return Err(AppError::operation(error)),
    };
    guard.check_read_path(&path).map_err(AppError::operation)?;
    guard.check_write_path(&path).map_err(AppError::operation)?;
    Ok(path)
}

fn resolve_inline_task(
    paths: &VaultPaths,
    task: &str,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<ResolvedInlineTask, AppError> {
    let filter = guard.map(PermissionGuard::read_filter);
    let note_index = vulcan_core::properties::load_note_index_with_filter(paths, filter.as_ref())
        .map_err(AppError::operation)?;

    if let Some((note_ref, line_number)) = parse_task_line_reference(task) {
        let path = resolve_task_mutation_path(paths, note_ref, guard)?;
        if let Some(task) = find_inline_task_in_path(&note_index, &path, line_number) {
            return Ok(task);
        }
        return Err(AppError::operation(format!(
            "no inline task at {path}:{line_number}"
        )));
    }

    // A readable note that is not writable is not an invitation to retry its
    // identifier as task text. Preserve the permission denial across fallback.
    if let Some(guard) = guard {
        if let Ok(note) =
            vulcan_core::resolve_note_reference_with_filter(paths, task, Some(&guard.read_filter()))
        {
            guard
                .check_read_path(&note.path)
                .map_err(AppError::operation)?;
            guard
                .check_write_path(&note.path)
                .map_err(AppError::operation)?;
        }
    }

    if let Some(path) = resolve_task_mutation_path(paths, task, guard)
        .ok()
        .filter(|path| note_index.values().any(|note| &note.document_path == path))
    {
        let mut tasks = inline_tasks_for_path(&note_index, &path);
        return match tasks.len() {
            0 => Err(AppError::operation(format!(
                "note has no inline tasks: {path}"
            ))),
            1 => Ok(tasks.remove(0)),
            _ => Err(AppError::operation(format!(
                "multiple inline tasks found in {path}; use <note>:<line> or exact task text"
            ))),
        };
    }

    let mut matches = note_index
        .values()
        .flat_map(inline_tasks_for_note)
        .filter(|candidate| {
            candidate.text == task || candidate.text.eq_ignore_ascii_case(task.trim())
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line_number.cmp(&right.line_number))
    });
    matches
        .dedup_by(|left, right| left.path == right.path && left.line_number == right.line_number);

    match matches.len() {
        0 => Err(AppError::operation(format!(
            "inline task not found: {task}"
        ))),
        1 => Ok(matches.remove(0)),
        _ => Err(AppError::operation(format!(
            "multiple inline tasks match '{task}'; use <note>:<line> to disambiguate"
        ))),
    }
}

fn parse_task_line_reference(task: &str) -> Option<(&str, i64)> {
    let (note, line_number) = task.rsplit_once(':')?;
    let line_number = line_number.trim().parse::<i64>().ok()?;
    (line_number > 0).then_some((note.trim(), line_number))
}

fn inline_tasks_for_path(
    note_index: &std::collections::HashMap<String, NoteRecord>,
    path: &str,
) -> Vec<ResolvedInlineTask> {
    note_index
        .values()
        .find(|note| note.document_path == path)
        .map_or_else(Vec::new, inline_tasks_for_note)
}

fn find_inline_task_in_path(
    note_index: &std::collections::HashMap<String, NoteRecord>,
    path: &str,
    line_number: i64,
) -> Option<ResolvedInlineTask> {
    inline_tasks_for_path(note_index, path)
        .into_iter()
        .find(|candidate| candidate.line_number == line_number)
}

fn inline_tasks_for_note(note: &NoteRecord) -> Vec<ResolvedInlineTask> {
    note.tasks
        .iter()
        .filter(|task| task.properties.get("taskSource").and_then(Value::as_str) != Some("file"))
        .map(|task| ResolvedInlineTask {
            path: note.document_path.clone(),
            line_number: task.line_number,
            text: task.text.clone(),
        })
        .collect()
}

fn authorize_task_changed_paths(
    changed_paths: &mut Vec<String>,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<(), AppError> {
    changed_paths.sort();
    changed_paths.dedup();
    if let Some(guard) = guard {
        for path in changed_paths {
            guard.check_write_path(path).map_err(AppError::operation)?;
        }
    }
    Ok(())
}

fn apply_inline_task_reschedule(
    paths: &VaultPaths,
    request: &TaskRescheduleRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskMutationReport, AppError> {
    let resolved = resolve_inline_task(paths, &request.task, guard)?;
    if let Some(guard) = guard {
        guard
            .check_read_path(&resolved.path)
            .map_err(AppError::operation)?;
        guard
            .check_write_path(&resolved.path)
            .map_err(AppError::operation)?;
    }
    let config = load_vault_config(paths).config;
    let due_value = resolve_tasknote_date_input(&config, &request.due, false)?;
    let source = read_task_source(paths, &resolved.path).map_err(AppError::operation)?;
    let (rendered, change) =
        reschedule_inline_task_source(&source, resolved.line_number, &due_value)?;
    let changes = change.into_iter().collect::<Vec<_>>();
    let mut changed_paths = if changes.is_empty() {
        Vec::new()
    } else {
        vec![resolved.path.clone()]
    };
    authorize_task_changed_paths(&mut changed_paths, guard)?;

    let routed = if changes.is_empty() {
        false
    } else {
        route_task_note_write(
            paths,
            &resolved.path,
            Some(&source),
            Some(&rendered),
            MdbaseWriteOperation::Update,
            request.dry_run,
            guard,
        )?
    };
    if !request.dry_run && !changes.is_empty() && !routed {
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &resolved.path,
            Some(&source),
            &rendered,
            "task reschedule",
            task_mutation_profile(paths, guard)?,
        )?;
    }

    Ok(TaskMutationReport {
        action: "reschedule".to_string(),
        dry_run: request.dry_run,
        path: resolved.path,
        moved_from: None,
        moved_to: None,
        changes,
        changed_paths,
    })
}

fn apply_inline_task_complete(
    paths: &VaultPaths,
    request: &TaskCompleteRequest,
    guard: Option<&ProfilePermissionGuard>,
) -> Result<TaskMutationReport, AppError> {
    let resolved = resolve_inline_task(paths, &request.task, guard)?;
    if let Some(guard) = guard {
        guard
            .check_read_path(&resolved.path)
            .map_err(AppError::operation)?;
        guard
            .check_write_path(&resolved.path)
            .map_err(AppError::operation)?;
    }
    let config = load_vault_config(paths).config;
    let completed_symbol = first_completed_inline_status_symbol(&config);
    let completed_date = normalize_date_argument(request.date.as_deref())?;
    let source = read_task_source(paths, &resolved.path).map_err(AppError::operation)?;
    let (rendered, change) = complete_inline_task_source(
        &source,
        resolved.line_number,
        &completed_symbol,
        &completed_date,
    )?;
    let changes = change.into_iter().collect::<Vec<_>>();
    let mut changed_paths = if changes.is_empty() {
        Vec::new()
    } else {
        vec![resolved.path.clone()]
    };
    authorize_task_changed_paths(&mut changed_paths, guard)?;

    let routed = if changes.is_empty() {
        false
    } else {
        route_task_note_write(
            paths,
            &resolved.path,
            Some(&source),
            Some(&rendered),
            MdbaseWriteOperation::Update,
            request.dry_run,
            guard,
        )?
    };
    if !request.dry_run && !changes.is_empty() && !routed {
        write_ordinary_note_if_unchanged_with_profile(
            paths,
            &resolved.path,
            Some(&source),
            &rendered,
            "task complete",
            task_mutation_profile(paths, guard)?,
        )?;
    }

    Ok(TaskMutationReport {
        action: "complete".to_string(),
        dry_run: request.dry_run,
        path: resolved.path,
        moved_from: None,
        moved_to: None,
        changes,
        changed_paths,
    })
}

fn complete_inline_task_source(
    source: &str,
    line_number: i64,
    completed_symbol: &str,
    completed_date: &str,
) -> Result<(String, Option<RefactorChange>), AppError> {
    let mut lines = source
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let index = usize::try_from(line_number.saturating_sub(1))
        .map_err(|_| AppError::operation(format!("invalid task line number: {line_number}")))?;
    let current = lines
        .get(index)
        .cloned()
        .ok_or_else(|| AppError::operation(format!("task line {line_number} not found")))?;
    let updated = update_inline_task_line(&current, completed_symbol, completed_date)?;
    let change = (updated != current).then(|| RefactorChange {
        before: current.clone(),
        after: updated.clone(),
    });
    lines[index] = updated;
    Ok((lines.join("\n"), change))
}

fn reschedule_inline_task_source(
    source: &str,
    line_number: i64,
    due: &str,
) -> Result<(String, Option<RefactorChange>), AppError> {
    let mut lines = source
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let index = usize::try_from(line_number.saturating_sub(1))
        .map_err(|_| AppError::operation(format!("invalid task line number: {line_number}")))?;
    let current = lines
        .get(index)
        .cloned()
        .ok_or_else(|| AppError::operation(format!("task line {line_number} not found")))?;
    let updated = update_inline_task_due_marker(&current, due)?;
    let change = (updated != current).then(|| RefactorChange {
        before: current.clone(),
        after: updated.clone(),
    });
    lines[index] = updated;
    Ok((lines.join("\n"), change))
}

fn update_inline_task_line(
    line: &str,
    completed_symbol: &str,
    completed_date: &str,
) -> Result<String, AppError> {
    let completed_char = completed_symbol
        .chars()
        .next()
        .ok_or_else(|| AppError::operation("completed task status cannot be empty"))?;
    let checkbox =
        Regex::new(r"^(\s*(?:[-*+]|\d+[.)])\s+\[)(.)(\])").expect("regex should compile");
    let captures = checkbox.captures(line).ok_or_else(|| {
        AppError::operation(format!(
            "line is not an inline task and cannot be completed: {line}"
        ))
    })?;
    let full = captures
        .get(0)
        .ok_or_else(|| AppError::operation("failed to locate task checkbox"))?;
    let prefix = captures.get(1).map_or("", |capture| capture.as_str());
    let suffix = captures.get(3).map_or("", |capture| capture.as_str());
    let replaced = format!(
        "{}{}{}{}{}",
        &line[..full.start()],
        prefix,
        completed_char,
        suffix,
        &line[full.end()..]
    );
    let completion_marker = Regex::new(r"✅\s+\S+").expect("regex should compile");
    let replaced = if completion_marker.is_match(&replaced) {
        completion_marker
            .replace(&replaced, format!("✅ {completed_date}"))
            .into_owned()
    } else {
        format!("{} ✅ {completed_date}", replaced.trim_end())
    };
    Ok(replaced)
}

fn update_inline_task_due_marker(line: &str, due: &str) -> Result<String, AppError> {
    let checkbox = Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s+\[[^\]]\]").expect("regex should compile");
    if !checkbox.is_match(line) {
        return Err(AppError::operation(format!(
            "line is not an inline task and cannot be rescheduled: {line}"
        )));
    }

    let due_marker =
        Regex::new(r"(?:📅|📆|🗓\x{FE0F}?)\s*\d{4}-\d{2}-\d{2}").expect("regex should compile");
    if due_marker.is_match(line) {
        Ok(due_marker.replace(line, format!("📅 {due}")).into_owned())
    } else {
        Ok(format!("{} 📅 {due}", line.trim_end()))
    }
}

fn first_completed_inline_status_symbol(config: &VaultConfig) -> String {
    config
        .tasks
        .statuses
        .completed
        .first()
        .cloned()
        .unwrap_or_else(|| "x".to_string())
}

fn tasknote_frontmatter_key(config: &VaultConfig, property: &str) -> String {
    let property = property.trim();
    let mapping = &config.tasknotes.field_mapping;
    match property {
        "title" => mapping.title.clone(),
        "status" => mapping.status.clone(),
        "priority" => mapping.priority.clone(),
        "due" => mapping.due.clone(),
        "scheduled" => mapping.scheduled.clone(),
        "contexts" => mapping.contexts.clone(),
        "projects" => mapping.projects.clone(),
        "timeEstimate" | "time_estimate" => mapping.time_estimate.clone(),
        "completedDate" | "completed_date" => mapping.completed_date.clone(),
        "dateCreated" | "date_created" => mapping.date_created.clone(),
        "dateModified" | "date_modified" => mapping.date_modified.clone(),
        "recurrence" => mapping.recurrence.clone(),
        "recurrenceAnchor" | "recurrence_anchor" => mapping.recurrence_anchor.clone(),
        "timeEntries" | "time_entries" => mapping.time_entries.clone(),
        "completeInstances" | "complete_instances" => mapping.complete_instances.clone(),
        "skippedInstances" | "skipped_instances" => mapping.skipped_instances.clone(),
        "blockedBy" | "blocked_by" | "blocked-by" => mapping.blocked_by.clone(),
        "pomodoros" => mapping.pomodoros.clone(),
        "reminders" => mapping.reminders.clone(),
        other => other.to_string(),
    }
}

fn parse_tasknote_cli_value(value: &str) -> YamlValue {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return YamlValue::String(String::new());
    }
    match serde_yaml::from_str::<YamlValue>(trimmed) {
        Ok(parsed) => parsed,
        Err(_) => YamlValue::String(value.to_string()),
    }
}

fn tasknote_change_summary(value: Option<&YamlValue>) -> String {
    match value {
        None => "<missing>".to_string(),
        Some(YamlValue::String(text)) => text.clone(),
        Some(value) => serde_json::to_string(&serde_json::to_value(value).unwrap_or(Value::Null))
            .unwrap_or_else(|_| "<unserializable>".to_string()),
    }
}

fn set_tasknote_frontmatter_value(
    frontmatter: &mut YamlMapping,
    key: &str,
    value: Option<YamlValue>,
) -> Option<RefactorChange> {
    let yaml_key = YamlValue::String(key.to_string());
    let before = frontmatter.get(&yaml_key).cloned();

    if let Some(value) = value {
        if before.as_ref() == Some(&value) {
            return None;
        }
        frontmatter.insert(yaml_key, value.clone());
        Some(RefactorChange {
            before: format!("{key}: {}", tasknote_change_summary(before.as_ref())),
            after: format!("{key}: {}", tasknote_change_summary(Some(&value))),
        })
    } else {
        before.as_ref()?;
        frontmatter.remove(&yaml_key);
        Some(RefactorChange {
            before: format!("{key}: {}", tasknote_change_summary(before.as_ref())),
            after: format!("{key}: <removed>"),
        })
    }
}

fn yaml_string_list(value: Option<&YamlValue>) -> Vec<String> {
    match value {
        Some(YamlValue::String(text)) => vec![text.clone()],
        Some(YamlValue::Sequence(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(ToOwned::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn yaml_string(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::Bool(flag) => Some(flag.to_string()),
        YamlValue::Number(number) => Some(number.to_string()),
        YamlValue::String(text) => Some(text.clone()),
        _ => None,
    }
}

fn default_tasknote_reminders_yaml_value(
    config: &VaultConfig,
) -> Result<Option<YamlValue>, AppError> {
    let reminders = tasknotes_default_reminder_values(
        &config.tasknotes.task_creation_defaults.default_reminders,
    );
    if reminders.is_empty() {
        return Ok(None);
    }

    serde_yaml::to_value(reminders)
        .map(Some)
        .map_err(AppError::operation)
}

fn first_completed_tasknote_status(config: &VaultConfig) -> String {
    config
        .tasknotes
        .statuses
        .iter()
        .find(|status| status.is_completed)
        .map_or_else(|| "done".to_string(), |status| status.value.clone())
}

fn current_utc_timestamp_string() -> String {
    TemplateTimestamp::current().default_strings().datetime
}

fn current_utc_date_string() -> String {
    TemplateTimestamp::current().default_date_string()
}

fn tasknote_reference_ms() -> i64 {
    parse_date_like_string(&TemplateTimestamp::current().default_date_string()).unwrap_or_default()
}

fn normalize_tasknote_context(context: &str) -> Option<String> {
    let trimmed = context.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.starts_with('@') {
        Some(trimmed.to_string())
    } else {
        Some(format!("@{trimmed}"))
    }
}

fn normalize_tasknote_tag(tag: &str) -> Option<String> {
    let trimmed = tag.trim().trim_matches('"').trim().trim_start_matches('#');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn normalize_tasknote_project(project: &str) -> Option<String> {
    let trimmed = project.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
        Some(trimmed.to_string())
    } else {
        Some(format!("[[{trimmed}]]"))
    }
}

fn dedup_tasknote_values<I, F>(values: I, normalize: F) -> Vec<String>
where
    I: IntoIterator<Item = String>,
    F: Fn(&str) -> Option<String>,
{
    let mut deduped = Vec::new();
    for value in values {
        let Some(normalized) = normalize(&value) else {
            continue;
        };
        if !deduped
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&normalized))
        {
            deduped.push(normalized);
        }
    }
    deduped
}

fn resolve_tasknote_date_input(
    config: &VaultConfig,
    value: &str,
    scheduled: bool,
) -> Result<String, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::operation("date value cannot be empty"));
    }
    if parse_date_like_string(trimmed).is_some() {
        return Ok(trimmed.to_string());
    }

    let prefix = if scheduled { "scheduled" } else { "due" };
    let parsed = parse_tasknote_natural_language(
        &format!("placeholder {prefix} {trimmed}"),
        &config.tasknotes,
        tasknote_reference_ms(),
    );
    let resolved = if scheduled {
        parsed.scheduled
    } else {
        parsed.due
    };
    resolved.ok_or_else(|| AppError::operation(format!("failed to parse date value: {value}")))
}

fn prepare_tasknote_archive_plan(
    frontmatter: &mut YamlMapping,
    loaded: &LoadedTaskNote,
) -> Result<TaskMutationPlan, AppError> {
    let status_state = tasknotes_status_state(&loaded.config.tasknotes, &loaded.indexed.status);
    if !loaded.indexed.archived && !status_state.completed {
        return Err(AppError::operation(format!(
            "task must be completed before archiving: {}",
            loaded.path
        )));
    }

    let mut changes = Vec::new();
    let archive_tag = &loaded.config.tasknotes.field_mapping.archive_tag;
    let tags_key = YamlValue::String("tags".to_string());
    let mut tags = yaml_string_list(frontmatter.get(&tags_key));
    if !tags.iter().any(|tag| tag.eq_ignore_ascii_case(archive_tag)) {
        tags.push(archive_tag.clone());
        tags.sort();
        if let Some(change) = set_tasknote_frontmatter_value(
            frontmatter,
            "tags",
            Some(YamlValue::Sequence(
                tags.iter().cloned().map(YamlValue::String).collect(),
            )),
        ) {
            changes.push(change);
        }
    }

    let modified_key = &loaded.config.tasknotes.field_mapping.date_modified;
    if let Some(change) = set_tasknote_frontmatter_value(
        frontmatter,
        modified_key,
        Some(YamlValue::String(current_utc_timestamp_string())),
    ) {
        changes.push(change);
    }

    let moved_to = Path::new(&loaded.path)
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| {
            let archive_folder = loaded
                .config
                .tasknotes
                .archive_folder
                .trim()
                .trim_matches('/');
            (!archive_folder.is_empty()).then(|| format!("{archive_folder}/{name}"))
        });

    Ok(TaskMutationPlan { changes, moved_to })
}

#[cfg(test)]
mod tests;
