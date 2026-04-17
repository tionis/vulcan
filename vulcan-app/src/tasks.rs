use crate::notes::{normalize_date_argument, resolve_existing_note_path};
use crate::templates::{parse_frontmatter_document, render_note_from_parts, TemplateTimestamp};
use crate::AppError;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use std::fs;
use std::path::Path;
use vulcan_core::expression::functions::parse_date_like_string;
use vulcan_core::properties::{extract_indexed_properties, load_note_index};
use vulcan_core::{
    extract_tasknote, load_vault_config, parse_tasknote_natural_language, tasknotes_status_state,
    IndexedTaskNote, NoteRecord, RefactorChange, VaultConfig, VaultPaths,
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
        apply_task_archive, apply_task_complete, apply_task_reschedule, apply_task_set,
        current_utc_date_string, TaskArchiveRequest, TaskCompleteRequest, TaskRescheduleRequest,
        TaskSetRequest,
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
