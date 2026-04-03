use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{
    TaskNotesConfig, TaskNotesIdentificationMethod, TaskNotesStatusConfig, TaskNotesUserFieldType,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexedTaskNote {
    pub title: String,
    pub status: String,
    pub priority: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub completed_date: Option<String>,
    pub date_created: Option<String>,
    pub date_modified: Option<String>,
    pub archived: bool,
    pub tags: Vec<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub time_estimate: Option<f64>,
    pub recurrence: Option<String>,
    pub recurrence_anchor: Option<String>,
    pub complete_instances: Vec<String>,
    pub skipped_instances: Vec<String>,
    pub blocked_by: Vec<Value>,
    pub reminders: Vec<Value>,
    pub time_entries: Vec<Value>,
    pub custom_fields: Map<String, Value>,
}

impl IndexedTaskNote {
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskNotesStatusState {
    pub name: String,
    pub status_type: String,
    pub completed: bool,
}

#[must_use]
pub fn extract_tasknote(
    document_path: &str,
    document_title: &str,
    properties: &Value,
    config: &TaskNotesConfig,
) -> Option<IndexedTaskNote> {
    let object = properties.as_object()?;
    if !is_tasknote_document(document_path, object, config) {
        return None;
    }

    let mapping = &config.field_mapping;
    let title = string_field(object, &mapping.title).unwrap_or_else(|| document_title.to_string());
    let status = status_value(object, config);
    let priority =
        string_field(object, &mapping.priority).unwrap_or_else(|| config.default_priority.clone());
    let tags = string_list_field(object, "tags");
    let archived = tags
        .iter()
        .any(|tag| normalized_tag(tag) == normalized_tag(&mapping.archive_tag));

    Some(IndexedTaskNote {
        title,
        status,
        priority,
        due: string_field(object, &mapping.due),
        scheduled: string_field(object, &mapping.scheduled),
        completed_date: string_field(object, &mapping.completed_date),
        date_created: string_field(object, &mapping.date_created),
        date_modified: string_field(object, &mapping.date_modified),
        archived,
        tags,
        contexts: string_list_field(object, &mapping.contexts),
        projects: string_list_field(object, &mapping.projects),
        time_estimate: numeric_field(object, &mapping.time_estimate),
        recurrence: string_field(object, &mapping.recurrence),
        recurrence_anchor: recurrence_anchor_field(object, &mapping.recurrence_anchor),
        complete_instances: string_list_field(object, &mapping.complete_instances),
        skipped_instances: string_list_field(object, &mapping.skipped_instances),
        blocked_by: value_list_field(object, &mapping.blocked_by),
        reminders: value_list_field(object, &mapping.reminders),
        time_entries: value_list_field(object, &mapping.time_entries),
        custom_fields: custom_fields(object, config),
    })
}

#[must_use]
pub fn is_tasknote_document(
    document_path: &str,
    properties: &Map<String, Value>,
    config: &TaskNotesConfig,
) -> bool {
    if is_excluded_path(document_path, &config.excluded_folders) {
        return false;
    }

    match config.identification_method {
        TaskNotesIdentificationMethod::Tag => {
            let task_tag = normalized_tag(&config.task_tag);
            string_list_field(properties, "tags")
                .iter()
                .any(|tag| normalized_tag(tag) == task_tag)
        }
        TaskNotesIdentificationMethod::Property => {
            if let Some(property_name) = &config.task_property_name {
                let Some(value) = properties.get(property_name) else {
                    return false;
                };
                if let Some(expected) = &config.task_property_value {
                    return value_matches_text(value, expected);
                }
                return true;
            }

            let mapping = &config.field_mapping;
            properties.contains_key(&mapping.status) && properties.contains_key(&mapping.priority)
        }
    }
}

#[must_use]
pub fn tasknotes_status_state(config: &TaskNotesConfig, status: &str) -> TaskNotesStatusState {
    let normalized = status.trim().to_ascii_lowercase();
    let definition = config
        .statuses
        .iter()
        .find(|candidate| candidate.value.trim().eq_ignore_ascii_case(status))
        .cloned()
        .unwrap_or_else(|| fallback_status_definition(config, &normalized));
    let status_type = if definition.is_completed {
        "DONE"
    } else if matches!(
        normalized.as_str(),
        "in-progress" | "in_progress" | "in progress" | "started" | "doing"
    ) {
        "IN_PROGRESS"
    } else if matches!(normalized.as_str(), "cancelled" | "canceled" | "abandoned") {
        "CANCELLED"
    } else {
        "TODO"
    };

    TaskNotesStatusState {
        name: definition.label,
        status_type: status_type.to_string(),
        completed: definition.is_completed,
    }
}

#[must_use]
pub fn tasknotes_priority_weight(config: &TaskNotesConfig, priority: &str) -> Option<f64> {
    config
        .priorities
        .iter()
        .find(|candidate| candidate.value.eq_ignore_ascii_case(priority))
        .map(|candidate| f64::from(candidate.weight))
}

fn custom_fields(properties: &Map<String, Value>, config: &TaskNotesConfig) -> Map<String, Value> {
    let reserved = config.field_mapping.reserved_property_names();
    let typed_fields = config
        .user_fields
        .iter()
        .map(|field| (field.key.as_str(), field.field_type))
        .collect::<std::collections::HashMap<_, _>>();
    let mut custom_fields = Map::new();

    for (key, value) in properties {
        if reserved.contains(key.as_str()) {
            continue;
        }
        if let Some(field_type) = typed_fields.get(key.as_str()) {
            if let Some(normalized) = normalize_user_field_value(value, *field_type) {
                custom_fields.insert(key.clone(), normalized);
            }
            continue;
        }
        custom_fields.insert(key.clone(), value.clone());
    }

    custom_fields
}

fn normalize_user_field_value(value: &Value, field_type: TaskNotesUserFieldType) -> Option<Value> {
    match field_type {
        TaskNotesUserFieldType::Number => value.as_f64().map(number_value),
        TaskNotesUserFieldType::Text | TaskNotesUserFieldType::Date => {
            string_scalar(value).map(Value::String)
        }
        TaskNotesUserFieldType::Boolean => value.as_bool().map(Value::Bool),
        TaskNotesUserFieldType::List => Some(Value::Array(
            string_list_from_value(value)
                .into_iter()
                .map(Value::String)
                .collect(),
        )),
    }
}

fn fallback_status_definition(config: &TaskNotesConfig, normalized: &str) -> TaskNotesStatusConfig {
    if normalized == "true" {
        return config
            .statuses
            .iter()
            .find(|status| status.is_completed)
            .cloned()
            .unwrap_or_else(|| TaskNotesStatusConfig {
                id: "done".to_string(),
                value: "done".to_string(),
                label: "Done".to_string(),
                color: "#16a34a".to_string(),
                is_completed: true,
                order: 0,
                auto_archive: false,
                auto_archive_delay: 5,
            });
    }

    if normalized == "false" {
        return config
            .statuses
            .iter()
            .find(|status| !status.is_completed)
            .cloned()
            .unwrap_or_else(|| TaskNotesStatusConfig {
                id: "open".to_string(),
                value: config.default_status.clone(),
                label: "Open".to_string(),
                color: "#808080".to_string(),
                is_completed: false,
                order: 0,
                auto_archive: false,
                auto_archive_delay: 5,
            });
    }

    TaskNotesStatusConfig {
        id: normalized.to_string(),
        value: normalized.to_string(),
        label: normalized.to_string(),
        color: "#808080".to_string(),
        is_completed: false,
        order: 0,
        auto_archive: false,
        auto_archive_delay: 5,
    }
}

fn status_value(properties: &Map<String, Value>, config: &TaskNotesConfig) -> String {
    let value = properties.get(&config.field_mapping.status);
    match value {
        Some(Value::Bool(flag)) => {
            if *flag {
                config
                    .statuses
                    .iter()
                    .find(|status| status.is_completed)
                    .map_or_else(|| "done".to_string(), |status| status.value.clone())
            } else {
                config.default_status.clone()
            }
        }
        Some(_) => string_field(properties, &config.field_mapping.status)
            .unwrap_or_else(|| config.default_status.clone()),
        None => config.default_status.clone(),
    }
}

fn recurrence_anchor_field(properties: &Map<String, Value>, key: &str) -> Option<String> {
    let value = string_field(properties, key)?;
    match value.as_str() {
        "scheduled" | "completion" => Some(value),
        _ => None,
    }
}

fn string_field(properties: &Map<String, Value>, key: &str) -> Option<String> {
    properties.get(key).and_then(string_scalar)
}

fn numeric_field(properties: &Map<String, Value>, key: &str) -> Option<f64> {
    properties.get(key).and_then(|value| match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    })
}

fn string_list_field(properties: &Map<String, Value>, key: &str) -> Vec<String> {
    properties
        .get(key)
        .map_or_else(Vec::new, string_list_from_value)
}

fn value_list_field(properties: &Map<String, Value>, key: &str) -> Vec<Value> {
    properties
        .get(key)
        .map_or_else(Vec::new, value_list_from_value)
}

fn string_list_from_value(value: &Value) -> Vec<String> {
    match value {
        Value::Array(values) => values.iter().filter_map(string_scalar).collect(),
        Value::String(text) => split_multivalue_string(text),
        other => string_scalar(other).into_iter().collect(),
    }
}

fn value_list_from_value(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(values) => values.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

fn split_multivalue_string(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.contains(',') {
        trimmed
            .split(',')
            .filter_map(|item| string_scalar(&Value::String(item.to_string())))
            .collect()
    } else {
        vec![trimmed.to_string()]
    }
}

fn string_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn normalized_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('#').to_ascii_lowercase()
}

fn value_matches_text(value: &Value, expected: &str) -> bool {
    let expected = expected.trim();
    match value {
        Value::Array(values) => values.iter().any(|item| value_matches_text(item, expected)),
        Value::String(text) => text.trim().eq_ignore_ascii_case(expected),
        Value::Bool(flag) => flag.to_string().eq_ignore_ascii_case(expected),
        Value::Number(number) => number.to_string().eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn is_excluded_path(document_path: &str, excluded_folders: &[String]) -> bool {
    excluded_folders.iter().any(|folder| {
        let normalized = folder.trim().trim_matches('/');
        !normalized.is_empty()
            && (document_path == normalized || document_path.starts_with(&format!("{normalized}/")))
    })
}

fn number_value(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::config::{
        TaskNotesConfig, TaskNotesIdentificationMethod, TaskNotesUserFieldConfig,
        TaskNotesUserFieldType,
    };

    use super::*;

    #[test]
    fn extracts_tag_identified_tasknotes_with_mapped_fields() {
        let mut config = TaskNotesConfig::default();
        config.field_mapping.due = "deadline".to_string();
        config.user_fields = vec![TaskNotesUserFieldConfig {
            id: "effort".to_string(),
            display_name: "Effort".to_string(),
            key: "effort".to_string(),
            field_type: TaskNotesUserFieldType::Number,
        }];

        let properties = json!({
            "title": "Write docs",
            "status": "in-progress",
            "priority": "high",
            "deadline": "2026-04-10",
            "tags": ["task", "docs"],
            "contexts": ["@desk"],
            "projects": ["[[Website]]"],
            "blockedBy": [{"uid": "[[Prep]]", "reltype": "FINISHTOSTART"}],
            "reminders": [{"id": "r1", "type": "relative"}],
            "timeEntries": [{"startTime": "2026-04-01T10:00:00Z"}],
            "effort": 3,
        });

        let indexed = extract_tasknote(
            "TaskNotes/Tasks/Write docs.md",
            "Write docs",
            &properties,
            &config,
        )
        .expect("tasknote should be indexed");

        assert_eq!(indexed.title, "Write docs");
        assert_eq!(indexed.status, "in-progress");
        assert_eq!(indexed.priority, "high");
        assert_eq!(indexed.due.as_deref(), Some("2026-04-10"));
        assert_eq!(indexed.contexts, vec!["@desk".to_string()]);
        assert_eq!(indexed.projects, vec!["[[Website]]".to_string()]);
        assert_eq!(indexed.blocked_by.len(), 1);
        assert_eq!(indexed.reminders.len(), 1);
        assert_eq!(indexed.time_entries.len(), 1);
        assert_eq!(indexed.custom_fields.get("effort"), Some(&json!(3.0)));
    }

    #[test]
    fn supports_property_identification_with_status_priority_fallback() {
        let mut config = TaskNotesConfig::default();
        config.identification_method = TaskNotesIdentificationMethod::Property;

        let fallback = json!({
            "title": "Fallback task",
            "status": "open",
            "priority": "normal"
        });
        assert!(extract_tasknote(
            "TaskNotes/Tasks/Fallback.md",
            "Fallback",
            &fallback,
            &config
        )
        .is_some());

        config.task_property_name = Some("isTask".to_string());
        config.task_property_value = Some("yes".to_string());
        let explicit = json!({
            "title": "Explicit task",
            "status": "open",
            "priority": "normal",
            "isTask": "yes"
        });
        assert!(extract_tasknote(
            "TaskNotes/Tasks/Explicit.md",
            "Explicit",
            &explicit,
            &config
        )
        .is_some());

        let not_a_task = json!({
            "title": "Project note",
            "status": "open",
            "priority": "normal",
            "isTask": "no"
        });
        assert!(extract_tasknote("Projects/Project.md", "Project", &not_a_task, &config).is_none());
    }

    #[test]
    fn excludes_configured_folders_and_computes_archived_tag() {
        let mut config = TaskNotesConfig::default();
        config.excluded_folders = vec!["TaskNotes/Archive".to_string()];

        let archived = json!({
            "title": "Archived task",
            "status": true,
            "priority": "low",
            "tags": ["task", "archived"]
        });

        assert!(extract_tasknote(
            "TaskNotes/Archive/Archived task.md",
            "Archived task",
            &archived,
            &config
        )
        .is_none());

        let indexed = extract_tasknote(
            "TaskNotes/Tasks/Archived task.md",
            "Archived task",
            &archived,
            &config,
        )
        .expect("task outside excluded folders should index");

        assert_eq!(indexed.status, "done");
        assert!(indexed.archived);
    }

    #[test]
    fn maps_status_values_into_unified_task_categories() {
        let config = TaskNotesConfig::default();

        assert_eq!(tasknotes_status_state(&config, "done").status_type, "DONE");
        assert_eq!(
            tasknotes_status_state(&config, "in-progress").status_type,
            "IN_PROGRESS"
        );
        assert_eq!(tasknotes_status_state(&config, "open").status_type, "TODO");
    }
}
