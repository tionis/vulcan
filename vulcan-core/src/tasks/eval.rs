use std::cmp::Ordering;
use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::expression::eval::compare_values;
use crate::expression::functions::parse_date_like_string;
use crate::file_metadata::FileMetadataResolver;
use crate::paths::VaultPaths;
use crate::properties::load_note_index;

use super::{
    parse_tasks_query, TasksDateRelation, TasksError, TasksFilter, TasksQuery, TasksQueryCommand,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksQueryResult {
    pub tasks: Vec<Value>,
    pub groups: Vec<TasksQueryGroup>,
    pub result_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hidden_fields: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub shown_fields: Vec<String>,
    pub short_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<TasksQuery>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TasksQueryGroup {
    pub field: String,
    pub key: Value,
    pub tasks: Vec<Value>,
}

#[derive(Debug, Clone)]
struct TaskRow {
    task: Map<String, Value>,
    path: String,
    line: i64,
}

impl TaskRow {
    fn value(&self) -> Value {
        Value::Object(self.task.clone())
    }

    fn field(&self, field: &str) -> Value {
        match field {
            "source" => self.task.get("taskSource").cloned().unwrap_or(Value::Null),
            "status.name" => self.task.get("statusName").cloned().unwrap_or(Value::Null),
            "status.type" => self.task.get("statusType").cloned().unwrap_or(Value::Null),
            "status.next" | "status.nextSymbol" => {
                self.task.get("statusNext").cloned().unwrap_or(Value::Null)
            }
            other => self.task.get(other).cloned().unwrap_or(Value::Null),
        }
    }
}

pub fn evaluate_tasks_query(
    paths: &VaultPaths,
    source: &str,
) -> Result<TasksQueryResult, TasksError> {
    let query = parse_tasks_query(source).map_err(TasksError::Parse)?;
    evaluate_parsed_tasks_query(paths, &query)
}

pub fn evaluate_parsed_tasks_query(
    paths: &VaultPaths,
    query: &TasksQuery,
) -> Result<TasksQueryResult, TasksError> {
    let note_index = load_note_index(paths)?;
    Ok(build_tasks_query_result(
        task_rows(&note_index),
        query,
        true,
    ))
}

#[must_use]
pub fn shape_tasks_query_result(tasks: Vec<Value>, query: &TasksQuery) -> TasksQueryResult {
    build_tasks_query_result(task_rows_from_values(tasks), query, false)
}

fn task_rows(note_index: &HashMap<String, crate::NoteRecord>) -> Vec<TaskRow> {
    let mut notes = note_index.values().cloned().collect::<Vec<_>>();
    notes.sort_by(|left, right| left.document_path.cmp(&right.document_path));

    let mut rows = Vec::new();
    for note in &notes {
        let tasks = match FileMetadataResolver::field(note, "tasks") {
            Value::Array(tasks) => tasks,
            _ => Vec::new(),
        };

        for (task_record, task_value) in note.tasks.iter().zip(tasks) {
            let Value::Object(mut task) = task_value else {
                continue;
            };
            if let Some(heading) = &task_record.section_heading {
                task.insert("heading".to_string(), Value::String(heading.clone()));
            }
            rows.push(TaskRow {
                task,
                path: note.document_path.clone(),
                line: task_record.line_number,
            });
        }
    }

    rows.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    rows
}

fn task_rows_from_values(tasks: Vec<Value>) -> Vec<TaskRow> {
    let mut rows = Vec::new();

    for task in tasks {
        let Value::Object(task) = task else {
            continue;
        };
        let path = task
            .get("path")
            .and_then(Value::as_str)
            .map_or_else(String::new, ToOwned::to_owned);
        let line = task.get("line").and_then(Value::as_i64).unwrap_or_default();
        rows.push(TaskRow { task, path, line });
    }

    rows.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    rows
}

fn build_tasks_query_result(
    mut rows: Vec<TaskRow>,
    query: &TasksQuery,
    apply_filters: bool,
) -> TasksQueryResult {
    let completion_by_id = task_completion_by_id(&rows);
    let mut hidden_fields = Vec::new();
    let mut shown_fields = Vec::new();
    let mut short_mode = false;
    let mut group_field: Option<(String, bool)> = None;
    let mut limit_groups = None;
    let mut explain = false;

    for command in &query.commands {
        match command {
            TasksQueryCommand::Filter { filter } if apply_filters => {
                rows.retain(|row| task_matches_filter(row, filter, &completion_by_id));
            }
            TasksQueryCommand::Filter { .. } => {}
            TasksQueryCommand::Sort { field, reverse } => {
                rows.sort_by(|left, right| compare_task_rows(left, right, field, *reverse));
            }
            TasksQueryCommand::Group { field, reverse } => {
                group_field = Some((field.clone(), *reverse));
            }
            TasksQueryCommand::Limit { value } => rows.truncate(*value),
            TasksQueryCommand::LimitGroups { value } => limit_groups = Some(*value),
            TasksQueryCommand::Hide { field } => hidden_fields.push(field.clone()),
            TasksQueryCommand::Show { field } => shown_fields.push(field.clone()),
            TasksQueryCommand::ShortMode => short_mode = true,
            TasksQueryCommand::Explain => explain = true,
        }
    }

    let groups = group_field
        .map(|(field, reverse)| build_groups(&rows, &field, reverse, limit_groups))
        .unwrap_or_default();
    let tasks = rows.into_iter().map(|row| row.value()).collect::<Vec<_>>();

    TasksQueryResult {
        result_count: tasks.len(),
        tasks,
        groups,
        hidden_fields,
        shown_fields,
        short_mode,
        plan: explain.then(|| query.clone()),
    }
}

fn task_completion_by_id(rows: &[TaskRow]) -> HashMap<String, bool> {
    let mut completion_by_id = HashMap::new();
    for row in rows {
        if let Some(id) = non_empty_string(&row.field("id")) {
            completion_by_id.insert(id.to_string(), bool_field(&row.field("completed")));
        }
    }
    completion_by_id
}

fn task_matches_filter(
    row: &TaskRow,
    filter: &TasksFilter,
    completion_by_id: &HashMap<String, bool>,
) -> bool {
    match filter {
        TasksFilter::Done { value } => bool_field(&row.field("completed")) == *value,
        TasksFilter::StatusIs { value } => task_status_matches(row, value),
        TasksFilter::StatusNameIncludes { value } => {
            string_contains(&row.field("statusName"), value)
        }
        TasksFilter::StatusTypeIs { value } => row
            .field("statusType")
            .as_str()
            .is_some_and(|status| status.eq_ignore_ascii_case(value)),
        TasksFilter::Date {
            field,
            relation,
            value,
        } => {
            let task_date_value = row.field(date_field_name(*field));
            let Some(task_date) = task_date_value.as_str() else {
                return false;
            };
            let Some(task_ms) = parse_date_like_string(task_date) else {
                return false;
            };
            let Some(query_ms) = parse_date_like_string(value) else {
                return false;
            };
            match relation {
                TasksDateRelation::Before => task_ms < query_ms,
                TasksDateRelation::After => task_ms > query_ms,
                TasksDateRelation::On => task_ms == query_ms,
            }
        }
        TasksFilter::HasDate { field, value } => {
            let has_value = non_empty_string(&row.field(date_field_name(*field))).is_some();
            has_value == *value
        }
        TasksFilter::TextIncludes { field, value } => {
            string_contains(&row.field(text_field_name(*field)), value)
        }
        TasksFilter::TagIncludes { value } => row
            .field("tags")
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag_matches(tag, value))),
        TasksFilter::ContextIncludes { value } => row
            .field("contexts")
            .as_array()
            .is_some_and(|values| values.iter().any(|item| tag_matches(item, value))),
        TasksFilter::ProjectIncludes { value } => row
            .field("projects")
            .as_array()
            .is_some_and(|values| values.iter().any(|item| tag_matches(item, value))),
        TasksFilter::PriorityIs { value } => row
            .field("priority")
            .as_str()
            .is_some_and(|priority| priority.eq_ignore_ascii_case(value)),
        TasksFilter::SourceIs { value } => row
            .field("source")
            .as_str()
            .is_some_and(|source| source.eq_ignore_ascii_case(value)),
        TasksFilter::Archived { value } => bool_field(&row.field("archived")) == *value,
        TasksFilter::Recurring { value } => {
            let recurring = non_empty_string(&row.field("recurrence")).is_some();
            recurring == *value
        }
        TasksFilter::Blocked { value } => task_is_blocked(row, completion_by_id) == *value,
        TasksFilter::HasId => non_empty_string(&row.field("id")).is_some(),
        TasksFilter::Not { filter } => !task_matches_filter(row, filter, completion_by_id),
        TasksFilter::And { filters } => filters
            .iter()
            .all(|filter| task_matches_filter(row, filter, completion_by_id)),
        TasksFilter::Or { filters } => filters
            .iter()
            .any(|filter| task_matches_filter(row, filter, completion_by_id)),
    }
}

fn task_is_blocked(row: &TaskRow, completion_by_id: &HashMap<String, bool>) -> bool {
    let blocker_ids = dependency_ids(&row.field("blocked-by"));
    if blocker_ids.is_empty() {
        return false;
    }

    blocker_ids
        .into_iter()
        .any(|blocker_id| match completion_by_id.get(&blocker_id) {
            Some(completed) => !completed,
            None => true,
        })
}

fn compare_task_rows(left: &TaskRow, right: &TaskRow, field: &str, reverse: bool) -> Ordering {
    let left_value = left.field(field);
    let right_value = right.field(field);
    let ordering = compare_values(&left_value, &right_value)
        .unwrap_or_else(|| stringify_value(&left_value).cmp(&stringify_value(&right_value)));
    let ordering = if reverse {
        ordering.reverse()
    } else {
        ordering
    };
    ordering.then_with(|| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    })
}

fn build_groups(
    rows: &[TaskRow],
    field: &str,
    reverse: bool,
    limit_groups: Option<usize>,
) -> Vec<TasksQueryGroup> {
    let mut buckets: Vec<(Value, Vec<Value>)> = Vec::new();

    for row in rows {
        let key = row.field(field);
        if let Some((_, tasks)) = buckets.iter_mut().find(|(existing, _)| existing == &key) {
            tasks.push(row.value());
        } else {
            buckets.push((key, vec![row.value()]));
        }
    }

    buckets.sort_by(|left, right| {
        let ordering = compare_values(&left.0, &right.0)
            .unwrap_or_else(|| stringify_value(&left.0).cmp(&stringify_value(&right.0)));
        if reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });

    if let Some(limit) = limit_groups {
        buckets.truncate(limit);
    }

    buckets
        .into_iter()
        .map(|(key, tasks)| TasksQueryGroup {
            field: field.to_string(),
            key,
            tasks,
        })
        .collect()
}

fn date_field_name(field: super::TasksDateField) -> &'static str {
    match field {
        super::TasksDateField::Due => "due",
        super::TasksDateField::Created => "created",
        super::TasksDateField::Start => "start",
        super::TasksDateField::Scheduled => "scheduled",
        super::TasksDateField::Done => "done",
    }
}

fn text_field_name(field: super::TasksTextField) -> &'static str {
    match field {
        super::TasksTextField::Description => "text",
        super::TasksTextField::Path => "path",
        super::TasksTextField::Heading => "heading",
    }
}

fn bool_field(value: &Value) -> bool {
    value.as_bool().unwrap_or(false)
}

fn non_empty_string(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

fn string_contains(haystack: &Value, needle: &str) -> bool {
    haystack.as_str().is_some_and(|value| {
        value
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    })
}

fn task_status_matches(row: &TaskRow, needle: &str) -> bool {
    let normalized = normalize_status_value(needle);
    [
        row.field("status"),
        row.field("statusName"),
        row.field("statusType"),
    ]
    .iter()
    .any(|value| {
        value.as_str().is_some_and(|status| {
            status.eq_ignore_ascii_case(needle) || normalize_status_value(status) == normalized
        })
    })
}

fn normalize_status_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '-' | '_'))
        .collect::<String>()
        .to_ascii_lowercase()
}

fn tag_matches(tag: &Value, needle: &str) -> bool {
    let Some(tag) = tag.as_str() else {
        return false;
    };
    let normalized_tag = tag.trim_start_matches('#');
    let normalized_needle = needle.trim_start_matches('#');
    normalized_tag.eq_ignore_ascii_case(normalized_needle)
}

fn stringify_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn dependency_ids(value: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    collect_dependency_ids(value, &mut ids);
    ids
}

fn collect_dependency_ids(value: &Value, ids: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let text = text.trim();
            if !text.is_empty() {
                ids.push(text.to_string());
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_dependency_ids(value, ids);
            }
        }
        Value::Object(object) => {
            if let Some(uid) = object.get("uid").and_then(Value::as_str).map(str::trim) {
                if !uid.is_empty() {
                    ids.push(uid.to_string());
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use crate::{scan_vault, ScanMode, VaultPaths};

    use super::*;

    fn write_eval_fixture(vault_root: &std::path::Path) {
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join("Tasks.md"),
            concat!(
                "# Sprint Board\n\n",
                "- [ ] Alpha task #work 🗓️ 2026-04-02 ➕ 2026-04-01 🔼 🆔 ALPHA-1\n",
                "- [ ] Beta blocked #ops 🗓️ 2026-04-03 🔺 ⛔ ALPHA-1\n",
                "- [x] Gamma done #ops ✅ 2026-04-04 ⏬\n",
                "- [/] Delta recurring #work 🔼 🔁 every week\n",
            ),
        )
        .expect("fixture note should be written");
    }

    #[test]
    fn evaluates_status_and_property_filters() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        write_eval_fixture(&vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let done = evaluate_tasks_query(&paths, "status.name includes done")
            .expect("done query should succeed");
        assert_eq!(
            task_texts(&done),
            vec!["Gamma done #ops ✅ 2026-04-04 ⏬".to_string()]
        );

        let recurring = evaluate_tasks_query(
            &paths,
            "not done\nis recurring\npath includes Tasks\nheading includes Sprint\n\
             tag includes #work\npriority is medium",
        )
        .expect("recurring query should succeed");
        assert_eq!(
            task_texts(&recurring),
            vec!["Delta recurring #work 🔼 🔁 every week".to_string()]
        );
    }

    #[test]
    fn evaluates_date_dependency_and_id_filters() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        write_eval_fixture(&vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let due = evaluate_tasks_query(&paths, "due before 2026-04-03")
            .expect("date query should succeed");
        assert_eq!(
            task_texts(&due),
            vec!["Alpha task #work 🗓️ 2026-04-02 ➕ 2026-04-01 🔼 🆔 ALPHA-1".to_string()]
        );

        let blocked =
            evaluate_tasks_query(&paths, "is blocked").expect("blocked query should succeed");
        assert_eq!(
            task_texts(&blocked),
            vec!["Beta blocked #ops 🗓️ 2026-04-03 🔺 ⛔ ALPHA-1".to_string()]
        );

        let has_id = evaluate_tasks_query(&paths, "has id").expect("id query should succeed");
        assert_eq!(
            task_texts(&has_id),
            vec!["Alpha task #work 🗓️ 2026-04-02 ➕ 2026-04-01 🔼 🆔 ALPHA-1".to_string()]
        );
    }

    #[test]
    fn sorts_groups_limits_and_explains_queries() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        write_eval_fixture(&vault_root);
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let result = evaluate_tasks_query(
            &paths,
            "not done\nsort by due reverse\ngroup by status.type reverse\nlimit 3\n\
             limit groups 1\nhide backlink\nshow urgency\nshort mode\nexplain",
        )
        .expect("query should succeed");

        assert_eq!(
            task_texts(&result),
            vec![
                "Beta blocked #ops 🗓️ 2026-04-03 🔺 ⛔ ALPHA-1".to_string(),
                "Alpha task #work 🗓️ 2026-04-02 ➕ 2026-04-01 🔼 🆔 ALPHA-1".to_string(),
                "Delta recurring #work 🔼 🔁 every week".to_string(),
            ]
        );
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].field, "status.type");
        assert_eq!(result.groups[0].key, Value::String("TODO".to_string()));
        assert_eq!(result.hidden_fields, vec!["backlink".to_string()]);
        assert_eq!(result.shown_fields, vec!["urgency".to_string()]);
        assert!(result.short_mode);
        assert!(result.plan.is_some());
    }

    #[test]
    fn evaluates_tasknotes_file_tasks_through_the_shared_query_engine() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path().join("vault");
        copy_fixture_vault("tasknotes", &vault_root);
        fs::write(
            vault_root.join("TaskNotes/Tasks/Archived Flag.md"),
            concat!(
                "---\n",
                "title: \"Archived flag\"\n",
                "status: \"done\"\n",
                "priority: \"low\"\n",
                "tags: [\"task\", \"archived\"]\n",
                "---\n"
            ),
        )
        .expect("archived tasknote should be written");
        let paths = VaultPaths::new(&vault_root);

        scan_vault(&paths, ScanMode::Full).expect("scan should succeed");

        let in_progress = evaluate_tasks_query(&paths, "status.type is in_progress")
            .expect("status query should succeed");
        assert_eq!(task_texts(&in_progress), vec!["Write docs".to_string()]);

        let recurring =
            evaluate_tasks_query(&paths, "is recurring").expect("recurring query should succeed");
        assert_eq!(task_texts(&recurring), vec!["Write docs".to_string()]);

        let blocked =
            evaluate_tasks_query(&paths, "is blocked").expect("blocked query should succeed");
        assert!(task_texts(&blocked).is_empty());

        let has_id = evaluate_tasks_query(&paths, "has id").expect("id query should succeed");
        assert_eq!(
            task_texts(&has_id),
            vec![
                "Archived flag".to_string(),
                "Prep outline".to_string(),
                "Write docs".to_string()
            ]
        );

        let source =
            evaluate_tasks_query(&paths, "source is file").expect("source query should succeed");
        assert_eq!(
            task_texts(&source),
            vec![
                "Archived flag".to_string(),
                "Prep outline".to_string(),
                "Write docs".to_string()
            ]
        );

        let archived =
            evaluate_tasks_query(&paths, "is archived").expect("archived query should succeed");
        assert_eq!(task_texts(&archived), vec!["Archived flag".to_string()]);

        let scoped = evaluate_tasks_query(
            &paths,
            "status is in progress\ncontext includes @desk\nproject includes [[Projects/Website]]",
        )
        .expect("scoped query should succeed");
        assert_eq!(task_texts(&scoped), vec!["Write docs".to_string()]);
    }

    #[test]
    fn shapes_prefiltered_results_with_sorts_and_groups() {
        let query = parse_tasks_query("sort by source reverse\ngroup by source")
            .expect("query should parse");
        let result = shape_tasks_query_result(
            vec![
                serde_json::json!({
                    "text": "Inline task",
                    "path": "Inbox.md",
                    "line": 3,
                    "taskSource": "inline"
                }),
                serde_json::json!({
                    "text": "File task",
                    "path": "TaskNotes/Tasks/Write Docs.md",
                    "line": 1,
                    "taskSource": "file"
                }),
            ],
            &query,
        );

        assert_eq!(
            task_texts(&result),
            vec!["Inline task".to_string(), "File task".to_string()]
        );
        assert_eq!(result.groups.len(), 2);
        assert_eq!(result.groups[0].field, "source");
        assert_eq!(result.groups[0].key, Value::String("file".to_string()));
    }

    fn task_texts(result: &TasksQueryResult) -> Vec<String> {
        result
            .tasks
            .iter()
            .map(|task| task["text"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn copy_fixture_vault(name: &str, destination: &Path) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/vaults")
            .join(name);

        copy_dir_recursive(&source, destination);
    }

    fn copy_dir_recursive(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).expect("destination directory should be created");

        for entry in fs::read_dir(source).expect("source directory should be readable") {
            let entry = entry.expect("directory entry should be readable");
            let file_type = entry.file_type().expect("file type should be readable");
            let target = destination.join(entry.file_name());

            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &target);
            } else if file_type.is_file() {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).expect("parent directory should exist");
                }
                fs::copy(entry.path(), target).expect("file should be copied");
            }
        }
    }
}
