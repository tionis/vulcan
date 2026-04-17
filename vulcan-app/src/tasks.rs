use crate::notes::{normalize_date_argument, normalize_note_path, resolve_existing_note_path};
use crate::templates::{
    load_named_template, merge_template_frontmatter, parse_frontmatter_document,
    render_loaded_template, render_note_from_parts, LoadedTemplateRenderRequest,
    TemplateEngineKind, TemplateRunMode, TemplateTimestamp,
};
use crate::AppError;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use vulcan_core::expression::functions::parse_date_like_string;
use vulcan_core::properties::{extract_indexed_properties, load_note_index};
use vulcan_core::{
    extract_tasknote, load_vault_config, parse_tasknote_natural_language, resolve_note_reference,
    tasknotes_default_date_value, tasknotes_default_recurrence_rule,
    tasknotes_default_reminder_values, tasknotes_status_state, GraphQueryError, IndexedTaskNote,
    NoteRecord, ParsedTaskNoteInput, RefactorChange, VaultConfig, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskMutationReport {
    pub action: String,
    pub dry_run: bool,
    pub path: String,
    pub moved_from: Option<String>,
    pub moved_to: Option<String>,
    pub changes: Vec<RefactorChange>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskSetRequest {
    pub task: String,
    pub property: String,
    pub value: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskRescheduleRequest {
    pub task: String,
    pub due: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskCompleteRequest {
    pub task: String,
    pub date: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskArchiveRequest {
    pub task: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct TaskAddRequest {
    pub text: String,
    pub no_nlp: bool,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub template: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskAddReport {
    pub action: String,
    pub dry_run: bool,
    pub created: bool,
    pub used_nlp: bool,
    pub path: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub time_estimate: Option<usize>,
    pub recurrence: Option<String>,
    pub template: Option<String>,
    pub frontmatter: Value,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_input: Option<ParsedTaskNoteInput>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskCreateRequest {
    pub text: String,
    pub note: Option<String>,
    pub due: Option<String>,
    pub priority: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskCreateReport {
    pub action: String,
    pub dry_run: bool,
    pub path: String,
    pub task: String,
    pub created_note: bool,
    pub line_number: i64,
    pub used_nlp: bool,
    pub line: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub priority: Option<String>,
    pub recurrence: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub changes: Vec<RefactorChange>,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TaskConvertRequest {
    pub file: String,
    pub line: Option<i64>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskConvertReport {
    pub action: String,
    pub dry_run: bool,
    pub mode: String,
    pub source_path: String,
    pub target_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<i64>,
    pub title: String,
    pub created: bool,
    pub source_changes: Vec<RefactorChange>,
    pub task_changes: Vec<RefactorChange>,
    pub frontmatter: Value,
    pub body: String,
    #[serde(skip)]
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct LoadedTaskNote {
    path: String,
    body: String,
    frontmatter: YamlMapping,
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

#[derive(Debug)]
struct TaskMutationPlan {
    changes: Vec<RefactorChange>,
    moved_to: Option<String>,
}

pub fn apply_task_set(
    paths: &VaultPaths,
    request: &TaskSetRequest,
) -> Result<TaskMutationReport, AppError> {
    apply_tasknote_mutation(
        paths,
        &request.task,
        "set",
        request.dry_run,
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
    if let Ok(loaded) = load_tasknote_note(paths, &request.task) {
        let due_value = resolve_tasknote_date_input(&loaded.config, &request.due, false)?;
        return apply_loaded_tasknote_mutation(
            paths,
            &loaded,
            "reschedule",
            request.dry_run,
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

    apply_inline_task_reschedule(paths, request)
}

pub fn apply_task_complete(
    paths: &VaultPaths,
    request: &TaskCompleteRequest,
) -> Result<TaskMutationReport, AppError> {
    if let Ok(loaded) = load_tasknote_note(paths, &request.task) {
        return apply_loaded_tasknote_mutation(
            paths,
            &loaded,
            "complete",
            request.dry_run,
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

    apply_inline_task_complete(paths, request)
}

pub fn apply_task_archive(
    paths: &VaultPaths,
    request: &TaskArchiveRequest,
) -> Result<TaskMutationReport, AppError> {
    apply_tasknote_mutation(
        paths,
        &request.task,
        "archive",
        request.dry_run,
        prepare_tasknote_archive_plan,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn apply_task_add(
    paths: &VaultPaths,
    request: &TaskAddRequest,
) -> Result<TaskAddReport, AppError> {
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
        Some(template_name) => {
            load_tasknote_template(paths, &config, template_name, &relative_path)?
        }
        None => (None, String::new()),
    };
    let merged_frontmatter =
        merge_template_frontmatter(Some(frontmatter), template_frontmatter).unwrap_or_default();
    let rendered = render_note_from_parts(Some(&merged_frontmatter), &template_body)
        .map_err(AppError::operation)?;
    let frontmatter_json = tasknote_frontmatter_json(&merged_frontmatter);

    if !request.dry_run {
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        fs::write(&absolute_path, rendered).map_err(AppError::operation)?;
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
        changed_paths: if request.dry_run {
            Vec::new()
        } else {
            vec![relative_path]
        },
    })
}

pub fn apply_task_create(
    paths: &VaultPaths,
    request: &TaskCreateRequest,
) -> Result<TaskCreateReport, AppError> {
    let config = load_vault_config(paths).config;
    let (relative_path, heading) = resolve_tasks_create_target(paths, request.note.as_deref())?;
    let absolute_path = paths.vault_root().join(&relative_path);
    if absolute_path.exists() && !absolute_path.is_file() {
        return Err(AppError::operation(format!(
            "target note is not a file: {relative_path}"
        )));
    }

    let existing = fs::read_to_string(&absolute_path).unwrap_or_default();
    let created_note = !absolute_path.exists();
    let planned = build_inline_task_create_plan(
        &config,
        &request.text,
        request.due.as_deref(),
        request.priority.as_deref(),
    )?;
    let insertion = append_entry_to_note(&existing, &planned.line, heading.as_deref());
    let task = format!("{}:{}", relative_path, insertion.line_number);
    let changed_paths = if request.dry_run {
        Vec::new()
    } else {
        vec![relative_path.clone()]
    };

    if !request.dry_run {
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        fs::write(&absolute_path, insertion.updated).map_err(AppError::operation)?;
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
    if let Some(line_number) = request.line {
        return apply_task_convert_line(paths, &request.file, line_number, request.dry_run);
    }

    let config = load_vault_config(paths).config;
    let (relative_path, source) = read_existing_note_source(paths, &request.file)?;
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
    let changed_paths = if request.dry_run || task_changes.is_empty() {
        Vec::new()
    } else {
        vec![relative_path.clone()]
    };

    if !request.dry_run && !task_changes.is_empty() {
        fs::write(paths.vault_root().join(&relative_path), rendered)
            .map_err(AppError::operation)?;
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

fn apply_task_convert_line(
    paths: &VaultPaths,
    file: &str,
    line_number: i64,
    dry_run: bool,
) -> Result<TaskConvertReport, AppError> {
    let config = load_vault_config(paths).config;
    let (source_path, source) = read_existing_note_source(paths, file)?;
    let selection = resolve_task_convert_line(&source, line_number)?;
    let planned = build_converted_tasknote(
        paths,
        &config,
        &selection.title_input,
        &selection.details,
        selection.completed,
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
    let changed_paths = if dry_run {
        Vec::new()
    } else {
        vec![source_path.clone(), planned.relative_path.clone()]
    };

    if !dry_run {
        let task_path = paths.vault_root().join(&planned.relative_path);
        if let Some(parent) = task_path.parent() {
            fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        fs::write(&task_path, rendered_task).map_err(AppError::operation)?;
        fs::write(paths.vault_root().join(&source_path), updated_source)
            .map_err(AppError::operation)?;
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

fn read_existing_note_source(paths: &VaultPaths, note: &str) -> Result<(String, String), AppError> {
    let relative_path = resolve_existing_note_path(paths, note)?;
    let source =
        fs::read_to_string(paths.vault_root().join(&relative_path)).map_err(AppError::operation)?;
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
        "highest" => Some("⏫"),
        "high" | "urgent" => Some("🔺"),
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
                3 => Some("🔺"),
                _ => Some("⏫"),
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
        tokens.push(format!("🗓️ {due}"));
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
) -> Result<(Option<YamlMapping>, String), AppError> {
    let loaded = load_named_template(paths, config, template_name)?;
    let vars = HashMap::new();
    let rendered = render_loaded_template(
        paths,
        config,
        &loaded,
        &LoadedTemplateRenderRequest {
            target_path,
            target_contents: None,
            engine: TemplateEngineKind::Auto,
            vars: &vars,
            allow_mutations: true,
            run_mode: TemplateRunMode::Create,
        },
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

fn apply_tasknote_mutation<F>(
    paths: &VaultPaths,
    task: &str,
    action: &str,
    dry_run: bool,
    mutate: F,
) -> Result<TaskMutationReport, AppError>
where
    F: FnOnce(&mut YamlMapping, &LoadedTaskNote) -> Result<TaskMutationPlan, AppError>,
{
    let loaded = load_tasknote_note(paths, task)?;
    apply_loaded_tasknote_mutation(paths, &loaded, action, dry_run, mutate)
}

fn apply_loaded_tasknote_mutation<F>(
    paths: &VaultPaths,
    loaded: &LoadedTaskNote,
    action: &str,
    dry_run: bool,
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
    changed_paths.sort();
    changed_paths.dedup();

    if !dry_run && !changed_paths.is_empty() {
        let source_path = paths.vault_root().join(&loaded.path);
        if let Some(destination) = moved_to.as_ref() {
            let destination_path = paths.vault_root().join(destination);
            if destination_path.exists() {
                return Err(AppError::operation(format!(
                    "destination task already exists: {destination}"
                )));
            }
        }
        fs::write(&source_path, rendered).map_err(AppError::operation)?;

        if let Some(destination) = moved_to.as_ref() {
            let destination_path = paths.vault_root().join(destination);
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent).map_err(AppError::operation)?;
            }
            fs::rename(&source_path, &destination_path).map_err(AppError::operation)?;
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

fn load_tasknote_note(paths: &VaultPaths, task: &str) -> Result<LoadedTaskNote, AppError> {
    let path = resolve_existing_note_path(paths, task)?;
    let source = fs::read_to_string(paths.vault_root().join(&path)).map_err(AppError::operation)?;
    let config = load_vault_config(paths).config;
    let parsed = vulcan_core::parse_document(&source, &config);
    let indexed_properties = extract_indexed_properties(&parsed, &config)
        .map_err(AppError::operation)?
        .map(|properties| serde_json::from_str::<Value>(&properties.canonical_json))
        .transpose()
        .map_err(AppError::operation)?;
    let (frontmatter, body) =
        parse_frontmatter_document(&source, false).map_err(AppError::operation)?;
    let frontmatter = frontmatter.unwrap_or_default();
    let frontmatter_json = load_note_index(paths)
        .ok()
        .and_then(|index| {
            index
                .into_values()
                .find(|note| note.document_path == path)
                .map(|note| note.properties)
        })
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
        body: normalize_tasknote_body(&body),
        frontmatter,
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

fn resolve_inline_task(paths: &VaultPaths, task: &str) -> Result<ResolvedInlineTask, AppError> {
    let note_index = load_note_index(paths).map_err(AppError::operation)?;

    if let Some((note_ref, line_number)) = parse_task_line_reference(task) {
        let path = resolve_existing_note_path(paths, note_ref)?;
        if let Some(task) = find_inline_task_in_path(&note_index, &path, line_number) {
            return Ok(task);
        }
        return Err(AppError::operation(format!(
            "no inline task at {path}:{line_number}"
        )));
    }

    if let Ok(path) = resolve_existing_note_path(paths, task) {
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

fn apply_inline_task_reschedule(
    paths: &VaultPaths,
    request: &TaskRescheduleRequest,
) -> Result<TaskMutationReport, AppError> {
    let resolved = resolve_inline_task(paths, &request.task)?;
    let config = load_vault_config(paths).config;
    let due_value = resolve_tasknote_date_input(&config, &request.due, false)?;
    let absolute_path = paths.vault_root().join(&resolved.path);
    let source = fs::read_to_string(&absolute_path).map_err(AppError::operation)?;
    let (rendered, change) =
        reschedule_inline_task_source(&source, resolved.line_number, &due_value)?;
    let changes = change.into_iter().collect::<Vec<_>>();
    let changed_paths = if request.dry_run || changes.is_empty() {
        Vec::new()
    } else {
        vec![resolved.path.clone()]
    };

    if !request.dry_run && !changes.is_empty() {
        fs::write(&absolute_path, rendered).map_err(AppError::operation)?;
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
) -> Result<TaskMutationReport, AppError> {
    let resolved = resolve_inline_task(paths, &request.task)?;
    let config = load_vault_config(paths).config;
    let completed_symbol = first_completed_inline_status_symbol(&config);
    let completed_date = normalize_date_argument(request.date.as_deref())?;
    let absolute_path = paths.vault_root().join(&resolved.path);
    let source = fs::read_to_string(&absolute_path).map_err(AppError::operation)?;
    let (rendered, change) = complete_inline_task_source(
        &source,
        resolved.line_number,
        &completed_symbol,
        &completed_date,
    )?;
    let changes = change.into_iter().collect::<Vec<_>>();
    let changed_paths = if request.dry_run || changes.is_empty() {
        Vec::new()
    } else {
        vec![resolved.path.clone()]
    };

    if !request.dry_run && !changes.is_empty() {
        fs::write(&absolute_path, rendered).map_err(AppError::operation)?;
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

    let due_marker = Regex::new(r"🗓(?:️)?\s+\S+").expect("regex should compile");
    if due_marker.is_match(line) {
        Ok(due_marker.replace(line, format!("🗓️ {due}")).into_owned())
    } else {
        Ok(format!("{} 🗓️ {due}", line.trim_end()))
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
mod tests {
    use super::{
        apply_task_add, apply_task_archive, apply_task_complete, apply_task_convert,
        apply_task_create, apply_task_reschedule, apply_task_set, current_utc_date_string,
        TaskAddRequest, TaskArchiveRequest, TaskCompleteRequest, TaskConvertRequest,
        TaskCreateRequest, TaskRescheduleRequest, TaskSetRequest,
    };
    use crate::templates::render_note_from_parts;
    use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::{
        initialize_vulcan_dir, load_vault_config, scan_vault_with_progress, ScanMode, VaultPaths,
    };

    #[test]
    fn apply_task_set_marks_completed_tasks_with_completed_date() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        let config = load_vault_config(&paths).config;
        seed_tasknote(&paths, &config, "Tasks/Alpha.md", "Alpha", "open", &[], "")
            .expect("seed task");

        let report = apply_task_set(
            &paths,
            &TaskSetRequest {
                task: "Tasks/Alpha".to_string(),
                property: "status".to_string(),
                value: first_completed_status_for_test(&config),
                dry_run: false,
            },
        )
        .expect("set report");

        assert_eq!(report.action, "set");
        assert_eq!(report.path, "Tasks/Alpha.md");
        assert_eq!(report.changed_paths, vec!["Tasks/Alpha.md".to_string()]);

        let rendered = fs::read_to_string(temp_dir.path().join("Tasks/Alpha.md"))
            .expect("updated task")
            .replace("\r\n", "\n");
        assert!(rendered.contains(&format!(
            "{}: {}",
            config.tasknotes.field_mapping.completed_date,
            current_utc_date_string()
        )));
    }

    #[test]
    fn apply_task_complete_updates_recurring_instance_lists() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        let config = load_vault_config(&paths).config;
        let recurrence_key = config.tasknotes.field_mapping.recurrence.clone();
        let skipped_key = config.tasknotes.field_mapping.skipped_instances.clone();
        seed_tasknote(
            &paths,
            &config,
            "Tasks/Recurring.md",
            "Recurring",
            "open",
            &[
                (
                    recurrence_key.as_str(),
                    YamlValue::String("every day".to_string()),
                ),
                (
                    skipped_key.as_str(),
                    YamlValue::Sequence(vec![YamlValue::String("2026-04-21".to_string())]),
                ),
            ],
            "",
        )
        .expect("seed recurring task");

        let report = apply_task_complete(
            &paths,
            &TaskCompleteRequest {
                task: "Tasks/Recurring".to_string(),
                date: Some("2026-04-21".to_string()),
                dry_run: false,
            },
        )
        .expect("complete report");

        assert_eq!(report.action, "complete");
        assert_eq!(report.path, "Tasks/Recurring.md");

        let rendered = fs::read_to_string(temp_dir.path().join("Tasks/Recurring.md"))
            .expect("updated recurring task")
            .replace("\r\n", "\n");
        assert!(rendered.contains(&format!(
            "{}:\n- 2026-04-21",
            config.tasknotes.field_mapping.complete_instances
        )));
        assert!(!rendered.contains(&format!(
            "{}:\n- 2026-04-21",
            config.tasknotes.field_mapping.skipped_instances
        )));
    }

    #[test]
    fn apply_task_reschedule_updates_inline_task_due_marker() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::write(temp_dir.path().join("Inbox.md"), "- [ ] Call Alice\n").expect("seed note");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

        let report = apply_task_reschedule(
            &paths,
            &TaskRescheduleRequest {
                task: "Inbox.md:1".to_string(),
                due: "2026-04-20".to_string(),
                dry_run: false,
            },
        )
        .expect("reschedule report");

        assert_eq!(report.action, "reschedule");
        assert_eq!(report.path, "Inbox.md");
        assert_eq!(report.changed_paths, vec!["Inbox.md".to_string()]);
        let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("updated note");
        assert!(rendered.contains("- [ ] Call Alice 🗓️ 2026-04-20"));
    }

    #[test]
    fn apply_task_complete_updates_inline_task_checkbox_and_date() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::write(temp_dir.path().join("Inbox.md"), "- [ ] Call Alice\n").expect("seed note");
        scan_vault_with_progress(&paths, ScanMode::Full, |_| {}).expect("scan");

        let report = apply_task_complete(
            &paths,
            &TaskCompleteRequest {
                task: "Inbox.md:1".to_string(),
                date: Some("2026-04-20".to_string()),
                dry_run: false,
            },
        )
        .expect("complete report");

        assert_eq!(report.action, "complete");
        assert_eq!(report.path, "Inbox.md");
        let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md")).expect("updated note");
        assert!(rendered.contains("- [x] Call Alice ✅ 2026-04-20"));
    }

    #[test]
    fn apply_task_add_creates_tasknote_from_natural_language_input() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        let config = load_vault_config(&paths).config;

        let report = apply_task_add(
            &paths,
            &TaskAddRequest {
                text: "Review launch plan tomorrow @work #shipit".to_string(),
                no_nlp: false,
                status: None,
                priority: None,
                due: None,
                scheduled: None,
                contexts: Vec::new(),
                projects: Vec::new(),
                tags: Vec::new(),
                template: None,
                dry_run: false,
            },
        )
        .expect("add report");

        assert_eq!(report.action, "add");
        assert_eq!(report.title, "Review launch plan");
        let expected_path = format!(
            "{}/Review launch plan.md",
            config.tasknotes.tasks_folder.trim_end_matches('/')
        );
        assert_eq!(report.path, expected_path);
        assert_eq!(report.changed_paths, vec![report.path.clone()]);

        let rendered = fs::read_to_string(temp_dir.path().join(&report.path))
            .expect("created task")
            .replace("\r\n", "\n");
        assert!(rendered.contains("title: Review launch plan"));
        assert!(rendered.contains("@work"));
        assert!(rendered.contains("shipit"));
    }

    #[test]
    fn apply_task_create_appends_inline_task_to_target_note() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::write(temp_dir.path().join("Inbox.md"), "# Tasks\n").expect("seed inbox");

        let report = apply_task_create(
            &paths,
            &TaskCreateRequest {
                text: "Call Alice".to_string(),
                note: Some("Inbox".to_string()),
                due: Some("2026-04-20".to_string()),
                priority: Some("high".to_string()),
                dry_run: false,
            },
        )
        .expect("create report");

        assert_eq!(report.action, "create");
        assert_eq!(report.path, "Inbox.md");
        assert_eq!(report.line_number, 3);
        let rendered = fs::read_to_string(temp_dir.path().join("Inbox.md"))
            .expect("updated inbox")
            .replace("\r\n", "\n");
        assert!(rendered.contains("- [ ] Call Alice 🗓️ 2026-04-20 🔺"));
    }

    #[test]
    fn apply_task_convert_note_promotes_existing_note_to_tasknote() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::create_dir_all(temp_dir.path().join("Ideas")).expect("ideas dir");
        fs::write(temp_dir.path().join("Ideas/Alpha.md"), "Alpha details\n").expect("seed note");

        let report = apply_task_convert(
            &paths,
            &TaskConvertRequest {
                file: "Ideas/Alpha".to_string(),
                line: None,
                dry_run: false,
            },
        )
        .expect("convert note report");

        assert_eq!(report.mode, "note");
        assert_eq!(report.source_path, "Ideas/Alpha.md");
        assert_eq!(report.target_path, "Ideas/Alpha.md");
        let rendered = fs::read_to_string(temp_dir.path().join("Ideas/Alpha.md"))
            .expect("converted note")
            .replace("\r\n", "\n");
        assert!(rendered.contains("title: Alpha"));
        assert!(rendered.contains("status: open"));
    }

    #[test]
    fn apply_task_convert_line_creates_tasknote_and_rewrites_source() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        fs::write(
            temp_dir.path().join("Inbox.md"),
            "- [ ] Review launch plan tomorrow @work\n",
        )
        .expect("seed inbox");

        let report = apply_task_convert(
            &paths,
            &TaskConvertRequest {
                file: "Inbox".to_string(),
                line: Some(1),
                dry_run: false,
            },
        )
        .expect("convert line report");

        assert_eq!(report.mode, "line");
        assert_eq!(report.source_path, "Inbox.md");
        assert!(temp_dir.path().join(&report.target_path).exists());

        let source = fs::read_to_string(temp_dir.path().join("Inbox.md"))
            .expect("rewritten inbox")
            .replace("\r\n", "\n");
        let link_target = report.target_path.trim_end_matches(".md");
        assert!(source.contains(&format!("[[{link_target}]]")));
    }

    #[test]
    fn apply_task_archive_moves_completed_task_into_archive_folder() {
        let temp_dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(temp_dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        let config = load_vault_config(&paths).config;
        seed_tasknote(
            &paths,
            &config,
            "Tasks/Done.md",
            "Done",
            &first_completed_status_for_test(&config),
            &[],
            "",
        )
        .expect("seed completed task");

        let report = apply_task_archive(
            &paths,
            &TaskArchiveRequest {
                task: "Tasks/Done".to_string(),
                dry_run: false,
            },
        )
        .expect("archive report");

        let archived_path = format!("{}/Done.md", config.tasknotes.archive_folder);
        assert_eq!(report.action, "archive");
        assert_eq!(report.path, archived_path);
        assert_eq!(report.moved_from.as_deref(), Some("Tasks/Done.md"));
        assert_eq!(report.moved_to.as_deref(), Some(report.path.as_str()));
        assert!(temp_dir.path().join(&report.path).exists());
        let rendered = fs::read_to_string(temp_dir.path().join(&report.path))
            .expect("archived task")
            .replace("\r\n", "\n");
        assert!(rendered.contains(&format!("- {}", config.tasknotes.field_mapping.archive_tag)));
    }

    fn seed_tasknote(
        paths: &VaultPaths,
        config: &VaultConfig,
        relative_path: &str,
        title: &str,
        status: &str,
        extra_fields: &[(&str, YamlValue)],
        body: &str,
    ) -> Result<(), AppError> {
        let mapping = &config.tasknotes.field_mapping;
        let mut frontmatter = YamlMapping::new();
        frontmatter.insert(
            YamlValue::String(mapping.title.clone()),
            YamlValue::String(title.to_string()),
        );
        frontmatter.insert(
            YamlValue::String(mapping.status.clone()),
            YamlValue::String(status.to_string()),
        );
        frontmatter.insert(
            YamlValue::String(mapping.priority.clone()),
            YamlValue::String(config.tasknotes.default_priority.clone()),
        );
        frontmatter.insert(
            YamlValue::String(mapping.date_created.clone()),
            YamlValue::String("2026-04-17T09:00:00Z".to_string()),
        );
        frontmatter.insert(
            YamlValue::String(mapping.date_modified.clone()),
            YamlValue::String("2026-04-17T09:00:00Z".to_string()),
        );
        match config.tasknotes.identification_method {
            vulcan_core::TaskNotesIdentificationMethod::Tag => {
                frontmatter.insert(
                    YamlValue::String("tags".to_string()),
                    YamlValue::Sequence(vec![YamlValue::String(config.tasknotes.task_tag.clone())]),
                );
            }
            vulcan_core::TaskNotesIdentificationMethod::Property => {
                if let Some(property_name) = config.tasknotes.task_property_name.as_ref() {
                    let property_value = config
                        .tasknotes
                        .task_property_value
                        .as_ref()
                        .map_or(YamlValue::Bool(true), |value| {
                            YamlValue::String(value.clone())
                        });
                    frontmatter.insert(YamlValue::String(property_name.clone()), property_value);
                }
            }
        }
        for (key, value) in extra_fields {
            frontmatter.insert(YamlValue::String((*key).to_string()), value.clone());
        }

        let rendered =
            render_note_from_parts(Some(&frontmatter), body).map_err(AppError::operation)?;
        let absolute_path = paths.vault_root().join(relative_path);
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        fs::write(absolute_path, rendered).map_err(AppError::operation)
    }

    fn first_completed_status_for_test(config: &VaultConfig) -> String {
        config
            .tasknotes
            .statuses
            .iter()
            .find(|status| status.is_completed)
            .map_or_else(|| "done".to_string(), |status| status.value.clone())
    }

    use crate::AppError;
    use vulcan_core::VaultConfig;
}
