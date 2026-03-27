use crate::expression::functions::date_components;
use crate::properties::NoteRecord;
use serde_json::{Map, Value};
use std::collections::HashSet;

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
}
