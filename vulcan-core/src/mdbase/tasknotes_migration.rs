use serde_json::{Map, Value};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseTaskNotesTypeMigration {
    pub source_version: String,
    pub rendered_source: String,
}

#[derive(Debug)]
pub enum MdbaseTaskNotesMigrationError {
    InvalidFrontmatter(String),
    UnsupportedField(String),
    Serialize(yaml_serde::Error),
}

impl Display for MdbaseTaskNotesMigrationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFrontmatter(message) | Self::UnsupportedField(message) => {
                formatter.write_str(message)
            }
            Self::Serialize(error) => write!(formatter, "failed to render migrated type: {error}"),
        }
    }
}

impl std::error::Error for MdbaseTaskNotesMigrationError {}

/// Convert the TaskNotes-generated mdbase v0.2 task type into a v0.3 type.
///
/// `None` means the source is not the recognizable generated `TaskNotes` v0.2
/// shape. Callers must not rewrite unrecognized type files.
#[allow(clippy::too_many_lines)]
pub fn migrate_tasknotes_v02_type(
    source: &str,
) -> Result<Option<MdbaseTaskNotesTypeMigration>, MdbaseTaskNotesMigrationError> {
    let Some((frontmatter_source, body)) = split_frontmatter(source) else {
        return Ok(None);
    };
    let yaml: yaml_serde::Value = yaml_serde::from_str(frontmatter_source)
        .map_err(|error| MdbaseTaskNotesMigrationError::InvalidFrontmatter(error.to_string()))?;
    let frontmatter = serde_json::to_value(yaml)
        .map_err(|error| MdbaseTaskNotesMigrationError::InvalidFrontmatter(error.to_string()))?;
    let Some(root) = frontmatter.as_object() else {
        return Ok(None);
    };
    if root.get("kind").and_then(Value::as_str) == Some("mdbase.type") {
        return repair_migrated_tasknotes_dates(root, body);
    }
    if root.get("name").and_then(Value::as_str) != Some("task")
        || !root.get("fields").is_some_and(Value::is_object)
        || !root.contains_key("x-tasknotes")
    {
        return Ok(None);
    }

    let fields = root["fields"].as_object().expect("checked above");
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut defaults = Map::new();
    let mut links = Map::new();
    let mut create_values = Map::new();
    let mut update_values = Map::new();
    let mut tasknotes_fields = Map::new();

    for (name, field) in fields {
        let converted = convert_field(
            name,
            field,
            name,
            None,
            &mut defaults,
            &mut links,
            &mut create_values,
            &mut update_values,
            &mut tasknotes_fields,
        )?;
        if field.get("required").and_then(Value::as_bool) == Some(true) {
            required.push(Value::String(name.clone()));
        }
        properties.insert(name.clone(), converted);
    }

    let mut schema_value = Map::from_iter([
        (
            "$schema".to_string(),
            Value::String("https://json-schema.org/draft/2020-12/schema".to_string()),
        ),
        ("type".to_string(), Value::String("object".to_string())),
        (
            "additionalProperties".to_string(),
            Value::Bool(root.get("strict").and_then(Value::as_bool) != Some(true)),
        ),
        ("properties".to_string(), Value::Object(properties)),
    ]);
    if !required.is_empty() {
        schema_value.insert("required".to_string(), Value::Array(required));
    }

    let mut collection = Map::new();
    if let Some(name_field) = root.get("display_name_key").and_then(Value::as_str) {
        collection.insert(
            "display".to_string(),
            Value::Object(Map::from_iter([(
                "name_field".to_string(),
                Value::String(name_field.to_string()),
            )])),
        );
    }
    if !defaults.is_empty() {
        collection.insert("read_defaults".to_string(), Value::Object(defaults));
    }
    if !links.is_empty() {
        collection.insert("links".to_string(), Value::Object(links));
    }
    if let Some(pattern) = root.get("path_pattern").and_then(Value::as_str) {
        collection.insert(
            "path".to_string(),
            Value::Object(Map::from_iter([(
                "pattern".to_string(),
                Value::String(pattern.to_string()),
            )])),
        );
    }

    let mut migrated = Map::new();
    migrated.insert("kind".to_string(), Value::String("mdbase.type".to_string()));
    migrated.insert("name".to_string(), Value::String("task".to_string()));
    migrated.insert("version".to_string(), Value::Number(1.into()));
    if let Some(description) = root.get("description") {
        migrated.insert("description".to_string(), description.clone());
    }
    if let Some(match_rule) = root.get("match") {
        migrated.insert("match".to_string(), match_rule.clone());
    }
    migrated.insert(
        "schema".to_string(),
        Value::Object(Map::from_iter([
            (
                "dialect".to_string(),
                Value::String("json-schema-2020-12".to_string()),
            ),
            ("value".to_string(), Value::Object(schema_value)),
        ])),
    );
    if !collection.is_empty() {
        migrated.insert("collection".to_string(), Value::Object(collection));
    }

    let mut lifecycle = Map::new();
    add_lifecycle_event(&mut lifecycle, "on_create", create_values);
    add_lifecycle_event(&mut lifecycle, "on_update", update_values);
    if !lifecycle.is_empty() {
        migrated.insert("lifecycle".to_string(), Value::Object(lifecycle));
    }

    let mut tasknotes = root
        .get("x-tasknotes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !tasknotes_fields.is_empty() {
        tasknotes.insert("fields".to_string(), Value::Object(tasknotes_fields));
    }
    tasknotes.insert(
        "migrated_from".to_string(),
        Value::String("mdbase-0.2-tasknotes".to_string()),
    );
    migrated.insert("x-tasknotes".to_string(), Value::Object(tasknotes));

    let yaml = yaml_serde::to_string(&Value::Object(migrated))
        .map_err(MdbaseTaskNotesMigrationError::Serialize)?;
    let body = body.replace("mdbase-spec) v0.2.0", "mdbase-spec) v0.3.0");
    Ok(Some(MdbaseTaskNotesTypeMigration {
        source_version: "0.2".to_string(),
        rendered_source: format!("---\n{yaml}---{body}"),
    }))
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn convert_field(
    name: &str,
    field: &Value,
    path: &str,
    parent_role: Option<&str>,
    defaults: &mut Map<String, Value>,
    links: &mut Map<String, Value>,
    create_values: &mut Map<String, Value>,
    update_values: &mut Map<String, Value>,
    tasknotes_fields: &mut Map<String, Value>,
) -> Result<Value, MdbaseTaskNotesMigrationError> {
    let Some(field) = field.as_object() else {
        return Err(MdbaseTaskNotesMigrationError::UnsupportedField(format!(
            "TaskNotes field `{path}` is not an object"
        )));
    };
    let kind = field.get("type").and_then(Value::as_str).ok_or_else(|| {
        MdbaseTaskNotesMigrationError::UnsupportedField(format!(
            "TaskNotes field `{path}` has no string type"
        ))
    })?;
    let role = field.get("tn_role").and_then(Value::as_str);
    let json_type = match kind {
        "string" | "date" | "datetime" | "enum" | "link" => "string",
        "integer" => "integer",
        "number" => "number",
        "boolean" => "boolean",
        "list" => "array",
        "object" => "object",
        other => {
            return Err(MdbaseTaskNotesMigrationError::UnsupportedField(format!(
                "TaskNotes field `{path}` uses unsupported type `{other}`"
            )))
        }
    };
    let mut schema = Map::from_iter([("type".to_string(), Value::String(json_type.to_string()))]);
    if kind == "date" && matches!(role, Some("scheduled" | "due")) {
        // TaskNotes declares these as dates but stores a time in them when one
        // is given, as local `YYYY-MM-DDTHH:MM` without an offset.
        schema.insert("anyOf".to_string(), tasknotes_date_or_time_schema());
    } else if kind == "date" {
        schema.insert("format".to_string(), Value::String("date".to_string()));
    } else if kind == "datetime" {
        schema.insert("format".to_string(), Value::String("date-time".to_string()));
    }
    if let Some(description) = field.get("description") {
        schema.insert("description".to_string(), description.clone());
    }
    if let Some(values) = field.get("values") {
        schema.insert("enum".to_string(), values.clone());
    }
    for (old, new) in [("min", "minimum"), ("max", "maximum")] {
        if let Some(value) = field.get(old) {
            schema.insert(new.to_string(), value.clone());
        }
    }
    if let Some(default) = field.get("default") {
        schema.insert("default".to_string(), default.clone());
        defaults.insert(name.to_string(), default.clone());
    }

    if kind == "list" {
        let item = field.get("items").ok_or_else(|| {
            MdbaseTaskNotesMigrationError::UnsupportedField(format!(
                "TaskNotes list field `{path}` has no items definition"
            ))
        })?;
        schema.insert(
            "items".to_string(),
            convert_field(
                name,
                item,
                &format!("{path}[]"),
                role.or(parent_role),
                &mut Map::new(),
                links,
                create_values,
                update_values,
                tasknotes_fields,
            )?,
        );
    } else if kind == "object" {
        let child_fields = field
            .get("fields")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                MdbaseTaskNotesMigrationError::UnsupportedField(format!(
                    "TaskNotes object field `{path}` has no fields definition"
                ))
            })?;
        let mut child_properties = Map::new();
        let mut child_required = Vec::new();
        for (child_name, child) in child_fields {
            let child_path = format!("{path}.{child_name}");
            child_properties.insert(
                child_name.clone(),
                convert_field(
                    child_name,
                    child,
                    &child_path,
                    role.or(parent_role),
                    &mut Map::new(),
                    links,
                    create_values,
                    update_values,
                    tasknotes_fields,
                )?,
            );
            if child.get("required").and_then(Value::as_bool) == Some(true) {
                child_required.push(Value::String(child_name.clone()));
            }
        }
        schema.insert("properties".to_string(), Value::Object(child_properties));
        if !child_required.is_empty() {
            schema.insert("required".to_string(), Value::Array(child_required));
        }
    }

    if kind == "link" {
        let target_type = if role == Some("recurrenceParent") || parent_role == Some("blockedBy") {
            "task"
        } else {
            "any"
        };
        links.insert(
            path.to_string(),
            Value::Object(Map::from_iter([
                (
                    "target_type".to_string(),
                    Value::String(target_type.to_string()),
                ),
                ("validate_exists".to_string(), Value::Bool(false)),
            ])),
        );
    }

    if let Some(generated) = field.get("generated").and_then(Value::as_str) {
        let now = Value::Object(Map::from_iter([("now".to_string(), Value::Bool(true))]));
        match generated {
            "now" => {
                create_values.insert(path.to_string(), now);
            }
            "now_on_write" => {
                create_values.insert(path.to_string(), now.clone());
                update_values.insert(path.to_string(), now);
            }
            other => {
                return Err(MdbaseTaskNotesMigrationError::UnsupportedField(format!(
                    "TaskNotes field `{path}` uses unsupported generator `{other}`"
                )))
            }
        }
    }

    let annotations = field
        .iter()
        .filter(|(key, _)| key.starts_with("tn_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Map<_, _>>();
    if !annotations.is_empty() {
        tasknotes_fields.insert(path.to_string(), Value::Object(annotations));
    }
    Ok(Value::Object(schema))
}

/// Earlier migrations declared `scheduled` and `due` as plain dates, which
/// rejects the times `TaskNotes` stores there. Widen them in a type this
/// migration produced; `None` when nothing needs changing.
fn repair_migrated_tasknotes_dates(
    root: &Map<String, Value>,
    body: &str,
) -> Result<Option<MdbaseTaskNotesTypeMigration>, MdbaseTaskNotesMigrationError> {
    if root
        .get("x-tasknotes")
        .and_then(|tasknotes| tasknotes.get("migrated_from"))
        .and_then(Value::as_str)
        != Some("mdbase-0.2-tasknotes")
    {
        return Ok(None);
    }
    let mut repaired = root.clone();
    let roles = root
        .get("x-tasknotes")
        .and_then(|tasknotes| tasknotes.get("fields"))
        .and_then(Value::as_object);
    let Some(properties) = repaired
        .get_mut("schema")
        .and_then(|schema| schema.get_mut("value"))
        .and_then(|value| value.get_mut("properties"))
        .and_then(Value::as_object_mut)
    else {
        return Ok(None);
    };
    let mut changed = false;
    for (name, property) in properties.iter_mut() {
        let role = roles
            .and_then(|roles| roles.get(name))
            .and_then(|field| field.get("tn_role"))
            .and_then(Value::as_str);
        let Some(property) = property.as_object_mut() else {
            continue;
        };
        if matches!(role, Some("scheduled" | "due"))
            && property.get("format").and_then(Value::as_str) == Some("date")
        {
            property.remove("format");
            property.insert("anyOf".to_string(), tasknotes_date_or_time_schema());
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    let yaml = yaml_serde::to_string(&Value::Object(repaired))
        .map_err(MdbaseTaskNotesMigrationError::Serialize)?;
    Ok(Some(MdbaseTaskNotesTypeMigration {
        source_version: "0.3".to_string(),
        rendered_source: format!("---\n{yaml}---{body}"),
    }))
}

/// The forms `TaskNotes` writes to `scheduled` and `due`: a date, a local time
/// without an offset, or a full RFC 3339 timestamp.
fn tasknotes_date_or_time_schema() -> Value {
    serde_json::json!([
        {"format": "date"},
        {"pattern": TASKNOTES_LOCAL_TIME_PATTERN},
        {"format": "date-time"}
    ])
}

const TASKNOTES_LOCAL_TIME_PATTERN: &str =
    r"^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])T([01]\d|2[0-3]):[0-5]\d(:[0-5]\d(\.\d+)?)?$";

fn add_lifecycle_event(
    lifecycle: &mut Map<String, Value>,
    event: &str,
    values: Map<String, Value>,
) {
    if !values.is_empty() {
        lifecycle.insert(
            event.to_string(),
            Value::Object(Map::from_iter([("set".to_string(), Value::Object(values))])),
        );
    }
}

fn split_frontmatter(source: &str) -> Option<(&str, &str)> {
    let after_open = source.strip_prefix("---\n")?;
    let end = after_open.find("\n---")?;
    let frontmatter = &after_open[..end];
    let body = &after_open[end + 4..];
    Some((frontmatter, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::{
        bundled_mdbase_schema, validate_mdbase_schema_value, MDBASE_CANONICAL_SCHEMA_BASE,
    };

    #[test]
    fn migrates_generated_tasknotes_type_and_validates_as_v03() {
        let source = r"---
name: task
description: Generated task
display_name_key: title
strict: false
path_pattern: Tasks/{title}.md
match:
  where:
    tags: {contains: task}
fields:
  title: {type: string, required: true, tn_role: title}
  status: {type: enum, values: [open, done], default: open, tn_role: status, tn_completed_values: [done]}
  dateCreated: {type: datetime, required: true, generated: now, tn_role: dateCreated}
  dateModified: {type: datetime, generated: now_on_write, tn_role: dateModified}
  projects: {type: list, items: {type: link}, tn_role: projects}
  scheduled: {type: date, tn_role: scheduled}
  completedDate: {type: date, tn_role: completedDate}
  blockedBy:
    type: list
    tn_role: blockedBy
    items:
      type: object
      fields:
        uid: {type: link, required: true}
x-tasknotes:
  nlp: {triggers: []}
---

It conforms to mdbase-spec) v0.2.0.
";
        let migration = migrate_tasknotes_v02_type(source)
            .expect("migration should succeed")
            .expect("source should be recognized");
        let (yaml, _) = split_frontmatter(&migration.rendered_source).expect("frontmatter");
        let value: Value = yaml_serde::from_str(yaml).expect("migrated yaml");
        let schema = bundled_mdbase_schema(&format!(
            "{MDBASE_CANONICAL_SCHEMA_BASE}type-file.schema.json"
        ))
        .expect("schema");
        let schema: Value = serde_json::from_str(schema.json).expect("schema json");
        assert!(validate_mdbase_schema_value(&schema, &value)
            .expect("schema should compile")
            .is_empty());
        assert_eq!(value["collection"]["read_defaults"]["status"], "open");
        assert_eq!(
            value["collection"]["links"]["projects[]"]["target_type"],
            "any"
        );
        assert_eq!(
            value["collection"]["links"]["blockedBy[].uid"]["target_type"],
            "task"
        );
        assert_eq!(
            value["lifecycle"]["on_update"]["set"]["dateModified"]["now"],
            true
        );
        assert_eq!(
            value["x-tasknotes"]["fields"]["status"]["tn_completed_values"][0],
            "done"
        );
        assert!(migration.rendered_source.contains("mdbase-spec) v0.3.0"));

        // `scheduled` takes the times TaskNotes writes; plain date fields stay strict.
        let note = |field: &str, date: &str| {
            serde_json::json!({
                "title": "t", "status": "open",
                "dateCreated": "2026-11-07T19:43:23+01:00", field: date
            })
        };
        let instance_errors = |field: &str, date: &str| {
            validate_mdbase_schema_value(&value["schema"]["value"], &note(field, date))
                .expect("type schema should compile")
                .len()
        };
        for accepted in [
            "2026-11-08",
            "2026-11-08T12:00",
            "2026-11-08T12:00:00",
            "2026-11-08T12:00:00+01:00",
        ] {
            assert_eq!(instance_errors("scheduled", accepted), 0, "{accepted}");
        }
        for rejected in ["tomorrow", "2026-13-08T12:00", "2026-11-08T25:00"] {
            assert_ne!(instance_errors("scheduled", rejected), 0, "{rejected}");
        }
        assert_eq!(instance_errors("completedDate", "2026-11-08"), 0);
        assert_ne!(instance_errors("completedDate", "2026-11-08T12:00"), 0);
    }

    /// The type `TaskNotes` generated for a real vault, and tasks in the shapes
    /// `TaskNotes` writes them. A migrated type that rejects any of these
    /// would make every later write to that task fail.
    #[test]
    fn migrated_generated_type_accepts_tasks_as_tasknotes_writes_them() {
        let source = include_str!("../../../tests/fixtures/mdbase/tasknotes-v02-task-type.md");
        let migration = migrate_tasknotes_v02_type(source)
            .expect("migration should succeed")
            .expect("the generated type should be recognized");
        let (yaml, _) = split_frontmatter(&migration.rendered_source).expect("frontmatter");
        let migrated: Value = yaml_serde::from_str(yaml).expect("migrated yaml");
        let schema = &migrated["schema"]["value"];
        let tasks = [
            // Created with a scheduled time and the default reminder.
            r#"title: think about master thesis topic
status: open
priority: "0"
dateCreated: 2026-11-07T19:43:23.922+01:00
dateModified: 2026-11-07T20:29:07.477+01:00
scheduled: 2026-11-08T12:00
tags: [task]
timeEstimate: 0
reminders:
  - {id: at-time, type: relative, relatedTo: scheduled, offset: -PT0M, description: Scheduled}"#,
            // A recurring task with a completed instance and a timed due date.
            r#"title: Water the plants
status: open
priority: "1"
dateCreated: 2026-10-01T08:00:00.000+02:00
scheduled: 2026-11-09
due: 2026-11-10T17:00
recurrence: DTSTART:20261005;FREQ=WEEKLY;BYDAY=MO
recurrence_anchor: scheduled
complete_instances: [2026-11-02]
skipped_instances: []
tags: [task]"#,
            // Done, with tracked time, a dependency, projects, contexts, and
            // an absolute reminder.
            r#"title: Write the report
status: done
priority: "2"
dateCreated: 2026-11-01T09:00:00.000+01:00
dateModified: 2026-11-07T18:00:00.000+01:00
completedDate: 2026-11-07
due: 2026-11-07
contexts: [office]
projects: ["[[Projects/Report|Report]]"]
blockedBy:
  - {uid: "[[Collect data]]", reltype: FINISHTOSTART}
timeEstimate: 90
timeEntries:
  - {startTime: 2026-11-07T10:00:00.000Z, endTime: 2026-11-07T10:25:00.000Z, description: Draft}
reminders:
  - {id: rem1, type: absolute, absoluteTime: 2026-11-07T08:00:00.000Z}
tags: [task, work]"#,
        ];
        for task in tasks {
            let value: Value = yaml_serde::from_str(task).expect("task yaml");
            let problems = validate_mdbase_schema_value(schema, &value).expect("schema compiles");
            assert!(problems.is_empty(), "{task}\n{problems:?}");
        }
        let bad: Value = yaml_serde::from_str(
            "title: t\nstatus: open\ndateCreated: 2026-11-07T19:43:23+01:00\nscheduled: tomorrow\n",
        )
        .expect("task yaml");
        assert!(!validate_mdbase_schema_value(schema, &bad)
            .expect("schema compiles")
            .is_empty());
    }

    #[test]
    fn leaves_non_tasknotes_and_v03_types_alone() {
        assert!(
            migrate_tasknotes_v02_type("---\nname: other\nfields: {}\n---\n")
                .expect("parse")
                .is_none()
        );
        assert!(migrate_tasknotes_v02_type(
            "---\nkind: mdbase.type\nname: task\nschema: {}\nx-tasknotes: {}\n---\n"
        )
        .expect("parse")
        .is_none());
    }

    #[test]
    fn repairs_strict_dates_left_by_an_earlier_migration_once() {
        let source = r"---
kind: mdbase.type
name: task
schema:
  value:
    type: object
    properties:
      scheduled: {type: string, format: date}
      completedDate: {type: string, format: date}
x-tasknotes:
  fields:
    scheduled: {tn_role: scheduled}
    completedDate: {tn_role: completedDate}
  migrated_from: mdbase-0.2-tasknotes
---

# Task
";
        let repair = migrate_tasknotes_v02_type(source)
            .expect("repair should succeed")
            .expect("strict scheduled should be repaired");
        assert_eq!(repair.source_version, "0.3");
        assert!(repair.rendered_source.ends_with("---\n\n# Task\n"));
        let (yaml, _) = split_frontmatter(&repair.rendered_source).expect("frontmatter");
        let value: Value = yaml_serde::from_str(yaml).expect("repaired yaml");
        let properties = &value["schema"]["value"]["properties"];
        assert!(properties["scheduled"].get("format").is_none());
        assert_eq!(
            properties["scheduled"]["anyOf"][1]["pattern"],
            TASKNOTES_LOCAL_TIME_PATTERN
        );
        assert_eq!(properties["completedDate"]["format"], "date");
        assert!(migrate_tasknotes_v02_type(&repair.rendered_source)
            .expect("parse")
            .is_none());
    }
}
