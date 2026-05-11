use serde_json::Value;
use std::collections::BTreeSet;

pub fn json_schema_to_typescript(schema: &Value, indent: usize) -> String {
    if let Some(value) = schema.get("const") {
        return json_value_to_typescript_literal(value);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let variants = values
            .iter()
            .map(json_value_to_typescript_literal)
            .collect::<Vec<_>>();
        return variants.join(" | ");
    }
    for union_key in ["anyOf", "oneOf"] {
        if let Some(variants) = schema.get(union_key).and_then(Value::as_array) {
            return variants
                .iter()
                .map(|variant| json_schema_to_typescript(variant, indent))
                .collect::<Vec<_>>()
                .join(" | ");
        }
    }
    match schema.get("type") {
        Some(Value::String(kind)) => json_schema_kind_to_typescript(kind, schema, indent),
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .map(|kind| json_schema_kind_to_typescript(kind, schema, indent))
            .collect::<Vec<_>>()
            .join(" | "),
        None => json_object_schema_to_typescript(schema, indent),
        Some(_) => "unknown".to_string(),
    }
}

fn json_schema_kind_to_typescript(kind: &str, schema: &Value, indent: usize) -> String {
    match kind {
        "string" => "string".to_string(),
        "integer" | "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            let item_type = schema.get("items").map_or_else(
                || "unknown".to_string(),
                |items| json_schema_to_typescript(items, indent),
            );
            if item_type.contains(" | ") {
                format!("({item_type})[]")
            } else {
                format!("{item_type}[]")
            }
        }
        "object" => json_object_schema_to_typescript(schema, indent),
        _ => "unknown".to_string(),
    }
}

fn json_object_schema_to_typescript(schema: &Value, indent: usize) -> String {
    let properties = schema.get("properties").and_then(Value::as_object);
    let additional_properties = schema.get("additionalProperties");
    if properties.is_none() {
        return additional_properties
            .and_then(|schema| {
                if schema == &Value::Bool(false) {
                    Some("Record<string, never>".to_string())
                } else if schema == &Value::Bool(true) {
                    Some("Record<string, unknown>".to_string())
                } else if schema.is_object() {
                    Some(format!(
                        "Record<string, {}>",
                        json_schema_to_typescript(schema, indent)
                    ))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "Record<string, unknown>".to_string());
    }
    let properties = properties.expect("properties checked above");
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let pad = " ".repeat(indent);
    let child_pad = " ".repeat(indent + 2);
    let mut lines = vec!["{".to_string()];
    for (name, schema) in properties {
        let optional = if required.contains(name.as_str()) {
            ""
        } else {
            "?"
        };
        let property = if is_ts_identifier(name) {
            name.clone()
        } else {
            serde_json::to_string(name).unwrap_or_else(|_| "\"<invalid>\"".to_string())
        };
        lines.push(format!(
            "{child_pad}{property}{optional}: {};",
            json_schema_to_typescript(schema, indent + 2)
        ));
    }
    if let Some(additional_schema) = additional_properties {
        match additional_schema {
            Value::Bool(true) => {
                lines.push(format!("{child_pad}[key: string]: unknown;"));
            }
            Value::Object(_) => {
                lines.push(format!(
                    "{child_pad}[key: string]: {};",
                    json_schema_to_typescript(additional_schema, indent + 2)
                ));
            }
            _ => {}
        }
    }
    lines.push(format!("{pad}}}"));
    lines.join("\n")
}

fn json_value_to_typescript_literal(value: &Value) -> String {
    match value {
        Value::String(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| "\"<invalid>\"".to_string())
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => compact_json(value),
        Value::Array(_) | Value::Object(_) => "unknown".to_string(),
    }
}

pub(super) fn ts_type_name(value: &str) -> String {
    let mut result = String::new();
    for part in value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
    {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars.map(|character| character.to_ascii_lowercase()));
        }
    }
    if result.is_empty() {
        "Tool".to_string()
    } else {
        result
    }
}

pub(super) fn ts_function_name(value: &str) -> String {
    let mut parts = value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return "callTool".to_string();
    };
    let mut result = first.to_ascii_lowercase();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars.map(|character| character.to_ascii_lowercase()));
        }
    }
    result
}

fn is_ts_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

pub(super) fn json_schema_required_fields(schema: &Value) -> BTreeSet<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect()
}

pub(super) fn top_level_field_name(value: &str) -> String {
    value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim_start_matches('-')
        .to_string()
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}
