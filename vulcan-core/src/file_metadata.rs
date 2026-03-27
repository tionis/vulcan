use crate::expression::functions::date_components;
use crate::parser::parse_document;
use crate::properties::{NoteListItemRecord, NoteRecord, NoteTaskRecord};
use crate::VaultConfig;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, Default)]
pub struct FileMetadataResolver;

impl FileMetadataResolver {
    #[must_use]
    pub fn field(note: &NoteRecord, field: &str) -> Value {
        match field {
            "path" => Value::String(note.document_path.clone()),
            "name" | "basename" => Value::String(note.file_name.clone()),
            "ext" => Value::String(note.file_ext.clone()),
            "folder" => Value::String(folder_for_path(&note.document_path)),
            "link" => Value::String(synthetic_file_link(&note.document_path, &note.file_ext)),
            "size" => Value::Number(note.file_size.into()),
            "mtime" | "ctime" => Value::Number(note.file_mtime.into()),
            "mday" | "cday" => Value::String(day_string_for_timestamp(note.file_mtime)),
            "tags" => json_string_array(expand_explicit_tags(&note.tags)),
            "etags" => json_string_array(note.tags.clone()),
            "outlinks" | "links" => json_string_array(note.links.clone()),
            "inlinks" => json_string_array(note.inlinks.clone()),
            "aliases" => json_string_array(note.aliases.clone()),
            "tasks" => Value::Array(task_objects(note)),
            "lists" => Value::Array(list_item_objects(note)),
            "frontmatter" => note.frontmatter.clone(),
            "properties" => note.properties.clone(),
            "day" => resolve_file_day(note).map_or(Value::Null, Value::String),
            "starred" => Value::Bool(false),
            _ => Value::Null,
        }
    }

    #[must_use]
    pub fn object(note: &NoteRecord) -> Value {
        let mut object = Map::new();
        for field in [
            "path",
            "name",
            "basename",
            "ext",
            "folder",
            "link",
            "size",
            "ctime",
            "cday",
            "mtime",
            "mday",
            "tags",
            "etags",
            "inlinks",
            "outlinks",
            "aliases",
            "tasks",
            "lists",
            "frontmatter",
            "day",
            "starred",
            "properties",
        ] {
            object.insert(field.to_string(), Self::field(note, field));
        }
        Value::Object(object)
    }
}

#[must_use]
pub(crate) fn synthetic_file_link(path: &str, ext: &str) -> String {
    if ext == "md" {
        format!("[[{}]]", path.strip_suffix(".md").unwrap_or(path))
    } else {
        format!("[[{path}]]")
    }
}

fn json_string_array(values: Vec<String>) -> Value {
    Value::Array(values.into_iter().map(Value::String).collect())
}

fn list_item_objects(note: &NoteRecord) -> Vec<Value> {
    let by_id = note
        .list_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let mut children_by_parent: HashMap<&str, Vec<&NoteListItemRecord>> = HashMap::new();
    for item in &note.list_items {
        if let Some(parent_id) = item.parent_item_id.as_deref() {
            children_by_parent.entry(parent_id).or_default().push(item);
        }
    }
    for children in children_by_parent.values_mut() {
        children.sort_by_key(|item| (item.line_number, item.byte_offset));
    }

    note.list_items
        .iter()
        .map(|item| list_item_object(item, note, &by_id, &children_by_parent))
        .collect()
}

fn list_item_object(
    item: &NoteListItemRecord,
    note: &NoteRecord,
    by_id: &HashMap<&str, &NoteListItemRecord>,
    children_by_parent: &HashMap<&str, Vec<&NoteListItemRecord>>,
) -> Value {
    let (tags, outlinks) = parsed_list_metadata(&item.text);
    let parent = item
        .parent_item_id
        .as_deref()
        .and_then(|parent_id| by_id.get(parent_id))
        .map_or(Value::Null, |parent| {
            Value::Number(parent.line_number.into())
        });
    let children = children_by_parent
        .get(item.id.as_str())
        .map(|children| {
            children
                .iter()
                .map(|child| list_item_object(child, note, by_id, children_by_parent))
                .collect()
        })
        .unwrap_or_default();

    let mut object = Map::new();
    object.insert("text".to_string(), Value::String(item.text.clone()));
    object.insert("line".to_string(), Value::Number(item.line_number.into()));
    object.insert(
        "lineCount".to_string(),
        Value::Number(item.line_count.into()),
    );
    object.insert(
        "path".to_string(),
        Value::String(note.document_path.clone()),
    );
    object.insert(
        "section".to_string(),
        section_link(&note.document_path, &item.section_heading).map_or(Value::Null, Value::String),
    );
    object.insert(
        "link".to_string(),
        Value::String(list_item_link(note, item)),
    );
    object.insert("tags".to_string(), json_string_array(tags));
    object.insert("outlinks".to_string(), json_string_array(outlinks));
    object.insert("children".to_string(), Value::Array(children));
    object.insert("parent".to_string(), parent);
    object.insert("task".to_string(), Value::Bool(item.is_task));
    object.insert("annotated".to_string(), Value::Bool(item.annotated));
    object.insert(
        "blockId".to_string(),
        item.block_id.clone().map_or(Value::Null, Value::String),
    );
    Value::Object(object)
}

fn task_objects(note: &NoteRecord) -> Vec<Value> {
    let list_by_id = note
        .list_items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let task_by_id = note
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let mut task_children: HashMap<&str, Vec<&NoteTaskRecord>> = HashMap::new();
    for task in &note.tasks {
        if let Some(parent_task_id) = task.parent_task_id.as_deref() {
            task_children.entry(parent_task_id).or_default().push(task);
        }
    }
    for children in task_children.values_mut() {
        children.sort_by_key(|task| (task.line_number, task.byte_offset));
    }

    note.tasks
        .iter()
        .map(|task| task_object(task, note, &list_by_id, &task_by_id, &task_children))
        .collect()
}

fn task_object(
    task: &NoteTaskRecord,
    note: &NoteRecord,
    list_by_id: &HashMap<&str, &NoteListItemRecord>,
    task_by_id: &HashMap<&str, &NoteTaskRecord>,
    task_children: &HashMap<&str, Vec<&NoteTaskRecord>>,
) -> Value {
    let mut object = list_by_id
        .get(task.list_item_id.as_str())
        .map_or_else(Map::new, |item| {
            match list_item_object(item, note, list_by_id, &HashMap::new()) {
                Value::Object(object) => object,
                _ => Map::new(),
            }
        });

    let status = task.status_char.clone();
    let completed = status.eq_ignore_ascii_case("x");
    object.insert("status".to_string(), Value::String(status.clone()));
    object.insert("checked".to_string(), Value::Bool(status.trim() != ""));
    object.insert("completed".to_string(), Value::Bool(completed));
    object.insert(
        "fullyCompleted".to_string(),
        Value::Bool(task_fully_completed(task, task_by_id, task_children)),
    );
    object.insert("visual".to_string(), Value::String(task.text.clone()));

    let children = task_children
        .get(task.id.as_str())
        .map(|children| {
            children
                .iter()
                .map(|child| task_object(child, note, list_by_id, task_by_id, task_children))
                .collect()
        })
        .unwrap_or_default();
    object.insert("children".to_string(), Value::Array(children));

    for (key, value) in &task.properties {
        object.insert(key.clone(), value.clone());
    }

    Value::Object(object)
}

fn task_fully_completed(
    task: &NoteTaskRecord,
    task_by_id: &HashMap<&str, &NoteTaskRecord>,
    task_children: &HashMap<&str, Vec<&NoteTaskRecord>>,
) -> bool {
    if !task.status_char.eq_ignore_ascii_case("x") {
        return false;
    }
    task_children.get(task.id.as_str()).is_none_or(|children| {
        children.iter().all(|child| {
            task_by_id
                .get(child.id.as_str())
                .is_some_and(|task| task_fully_completed(task, task_by_id, task_children))
        })
    })
}

fn parsed_list_metadata(text: &str) -> (Vec<String>, Vec<String>) {
    let parsed = parse_document(text, &VaultConfig::default());
    let mut tags = Vec::new();
    let mut seen_tags = HashSet::new();
    for tag in parsed.tags {
        let tag_text = format!("#{}", tag.tag_text);
        if seen_tags.insert(tag_text.clone()) {
            tags.push(tag_text);
        }
    }

    let mut outlinks = Vec::new();
    let mut seen_links = HashSet::new();
    for link in parsed.links {
        if link.link_kind == crate::LinkKind::External {
            continue;
        }
        if seen_links.insert(link.raw_text.clone()) {
            outlinks.push(link.raw_text);
        }
    }

    (tags, outlinks)
}

fn list_item_link(note: &NoteRecord, item: &NoteListItemRecord) -> String {
    if let Some(block_id) = &item.block_id {
        return format!(
            "[[{}#^{}]]",
            note_link_target(&note.document_path),
            block_id
        );
    }
    section_link(&note.document_path, &item.section_heading)
        .unwrap_or_else(|| synthetic_file_link(&note.document_path, &note.file_ext))
}

fn section_link(path: &str, heading: &Option<String>) -> Option<String> {
    heading
        .as_ref()
        .map(|heading| format!("[[{}#{}]]", note_link_target(path), heading))
}

fn note_link_target(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

fn folder_for_path(path: &str) -> String {
    path.rfind('/')
        .map_or_else(String::new, |index| path[..index].to_string())
}

fn day_string_for_timestamp(timestamp_ms: i64) -> String {
    let (year, month, day, _, _, _, _) = date_components(timestamp_ms);
    format!("{year:04}-{month:02}-{day:02}")
}

fn expand_explicit_tags(tags: &[String]) -> Vec<String> {
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();

    for tag in tags {
        let Some(without_hash) = tag.strip_prefix('#') else {
            if seen.insert(tag.clone()) {
                expanded.push(tag.clone());
            }
            continue;
        };
        let mut prefix = String::new();
        for segment in without_hash.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            let expanded_tag = format!("#{prefix}");
            if seen.insert(expanded_tag.clone()) {
                expanded.push(expanded_tag);
            }
        }
    }

    expanded
}

fn resolve_file_day(note: &NoteRecord) -> Option<String> {
    filename_day(&note.file_name).or_else(|| frontmatter_day(&note.frontmatter))
}

fn filename_day(file_name: &str) -> Option<String> {
    if matches_iso_date(file_name) {
        return Some(file_name.to_string());
    }
    if file_name.len() == 8 && file_name.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(format!(
            "{}-{}-{}",
            &file_name[0..4],
            &file_name[4..6],
            &file_name[6..8]
        ));
    }
    None
}

fn frontmatter_day(frontmatter: &Value) -> Option<String> {
    let Value::Object(object) = frontmatter else {
        return None;
    };
    object.iter().find_map(|(key, value)| {
        if !key.eq_ignore_ascii_case("date") {
            return None;
        }
        match value {
            Value::String(text) => normalize_day_like_value(text),
            _ => None,
        }
    })
}

fn normalize_day_like_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if matches_iso_date(trimmed) {
        return Some(trimmed.to_string());
    }
    if let Some((date, _)) = trimmed.split_once('T') {
        if matches_iso_date(date) {
            return Some(date.to_string());
        }
    }
    None
}

fn matches_iso_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note_record() -> NoteRecord {
        NoteRecord {
            document_id: "note-id".to_string(),
            document_path: "projects/2026-04-18-note.md".to_string(),
            file_name: "2026-04-18-note".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1_700_000_000_000,
            file_size: 1234,
            properties: serde_json::json!({"status": "done"}),
            tags: vec!["#project/alpha".to_string()],
            links: vec!["[[Other]]".to_string()],
            inlinks: vec!["[[Home]]".to_string()],
            aliases: vec!["Sprint Note".to_string()],
            frontmatter: serde_json::json!({"Date": "2026-04-18"}),
            list_items: vec![],
            tasks: vec![],
        }
    }

    #[test]
    fn expands_hierarchical_tags_for_file_tags() {
        assert_eq!(
            expand_explicit_tags(&["#project/alpha/beta".to_string(), "#project".to_string()]),
            vec![
                "#project".to_string(),
                "#project/alpha".to_string(),
                "#project/alpha/beta".to_string(),
            ]
        );
    }

    #[test]
    fn resolves_core_file_namespace_fields() {
        let note = note_record();

        assert_eq!(
            FileMetadataResolver::field(&note, "folder"),
            Value::String("projects".to_string())
        );
        assert_eq!(
            FileMetadataResolver::field(&note, "link"),
            Value::String("[[projects/2026-04-18-note]]".to_string())
        );
        assert_eq!(
            FileMetadataResolver::field(&note, "tags"),
            serde_json::json!(["#project", "#project/alpha"])
        );
        assert_eq!(
            FileMetadataResolver::field(&note, "etags"),
            serde_json::json!(["#project/alpha"])
        );
        assert_eq!(
            FileMetadataResolver::field(&note, "inlinks"),
            serde_json::json!(["[[Home]]"])
        );
        assert_eq!(
            FileMetadataResolver::field(&note, "aliases"),
            serde_json::json!(["Sprint Note"])
        );
        assert_eq!(
            FileMetadataResolver::field(&note, "day"),
            Value::String("2026-04-18".to_string())
        );
    }

    #[test]
    fn falls_back_to_frontmatter_date_for_file_day() {
        let mut note = note_record();
        note.file_name = "meeting-notes".to_string();

        assert_eq!(
            FileMetadataResolver::field(&note, "day"),
            Value::String("2026-04-18".to_string())
        );
    }

    #[test]
    fn resolves_task_and_list_metadata() {
        let mut note = note_record();
        note.list_items = vec![
            NoteListItemRecord {
                id: "list-1".to_string(),
                text: "Parent item [kind:: note]".to_string(),
                line_number: 10,
                line_count: 1,
                byte_offset: 100,
                section_heading: Some("Lists".to_string()),
                parent_item_id: None,
                is_task: false,
                block_id: None,
                annotated: true,
                symbol: "-".to_string(),
            },
            NoteListItemRecord {
                id: "list-2".to_string(),
                text: "Nested task [[Other]] #project ^child".to_string(),
                line_number: 11,
                line_count: 1,
                byte_offset: 120,
                section_heading: Some("Lists".to_string()),
                parent_item_id: Some("list-1".to_string()),
                is_task: true,
                block_id: Some("child".to_string()),
                annotated: false,
                symbol: "-".to_string(),
            },
        ];
        note.tasks = vec![NoteTaskRecord {
            id: "task-1".to_string(),
            list_item_id: "list-2".to_string(),
            status_char: "x".to_string(),
            text: "Nested task [[Other]] #project ^child".to_string(),
            byte_offset: 120,
            parent_task_id: None,
            section_heading: Some("Lists".to_string()),
            line_number: 11,
            properties: serde_json::Map::from_iter([(
                "due".to_string(),
                Value::String("2026-04-18".to_string()),
            )]),
        }];

        let lists = FileMetadataResolver::field(&note, "lists");
        let lists = lists.as_array().expect("lists should be an array");
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0]["line"], Value::Number(10.into()));
        assert_eq!(lists[0]["annotated"], Value::Bool(true));
        assert_eq!(lists[0]["children"][0]["line"], Value::Number(11.into()));
        assert_eq!(
            lists[1]["link"],
            Value::String("[[projects/2026-04-18-note#^child]]".to_string())
        );
        assert_eq!(lists[1]["tags"], serde_json::json!(["#project"]));
        assert_eq!(lists[1]["outlinks"], serde_json::json!(["[[Other]]"]));

        let tasks = FileMetadataResolver::field(&note, "tasks");
        let tasks = tasks.as_array().expect("tasks should be an array");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["status"], Value::String("x".to_string()));
        assert_eq!(tasks[0]["checked"], Value::Bool(true));
        assert_eq!(tasks[0]["completed"], Value::Bool(true));
        assert_eq!(tasks[0]["fullyCompleted"], Value::Bool(true));
        assert_eq!(tasks[0]["due"], Value::String("2026-04-18".to_string()));
    }
}
