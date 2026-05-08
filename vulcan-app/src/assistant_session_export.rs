use crate::AppError;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssistantSessionExportReport {
    pub source_path: String,
    pub export_path: String,
    pub message_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSession {
    metadata: BTreeMap<String, String>,
    messages: Vec<ParsedMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedMessage {
    role: MessageRole,
    content: String,
    metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageRole {
    User,
    Assistant,
    Tool,
    System,
}

pub fn export_assistant_session_file(
    vault_root: &Path,
    session_path: &Path,
    export_dir: &Path,
) -> Result<AssistantSessionExportReport, AppError> {
    let contents = fs::read_to_string(session_path)?;
    let session = parse_session(&contents)?;
    fs::create_dir_all(export_dir)?;
    let export_path = export_dir.join(export_filename(&session, session_path));
    fs::write(&export_path, render_markdown_export(&session))?;
    Ok(AssistantSessionExportReport {
        source_path: display_path(vault_root, session_path),
        export_path: display_path(vault_root, &export_path),
        message_count: session.messages.len(),
    })
}

fn parse_session(contents: &str) -> Result<ParsedSession, AppError> {
    if contents.trim_start().starts_with("---") {
        return parse_markdown_session(contents);
    }
    parse_json_session(contents)
}

fn parse_markdown_session(contents: &str) -> Result<ParsedSession, AppError> {
    let (metadata, body) = parse_frontmatter(contents)?;
    let mut messages = Vec::new();
    let mut current_role = None;
    let mut current = Vec::new();
    let mut in_message_callout = false;

    for line in body.lines() {
        if let Some(role) = heading_role(line) {
            flush_message(&mut messages, current_role.take(), &mut current);
            current_role = Some(role);
            in_message_callout = false;
            continue;
        }
        if line.trim_start().starts_with("> [!metadata]") {
            in_message_callout = false;
            continue;
        }
        if let Some(role) = callout_role(line) {
            if current_role.is_none() {
                current_role = Some(role);
            }
            in_message_callout = true;
            continue;
        }
        if in_message_callout {
            if let Some(stripped) = line.strip_prefix("> ") {
                current.push(stripped.to_string());
            } else if line.trim() == ">" {
                current.push(String::new());
            } else if line.trim() == "---" {
                flush_message(&mut messages, current_role.take(), &mut current);
                in_message_callout = false;
            }
        }
    }
    flush_message(&mut messages, current_role, &mut current);
    Ok(ParsedSession { metadata, messages })
}

fn parse_frontmatter(contents: &str) -> Result<(BTreeMap<String, String>, &str), AppError> {
    let Some(rest) = contents.strip_prefix("---") else {
        return Ok((BTreeMap::new(), contents));
    };
    let Some((frontmatter, body)) = rest.split_once("\n---") else {
        return Ok((BTreeMap::new(), contents));
    };
    let value =
        serde_yaml::from_str::<serde_yaml::Value>(frontmatter).map_err(AppError::operation)?;
    Ok((
        yaml_mapping_to_strings(&value),
        body.trim_start_matches('\n'),
    ))
}

fn parse_json_session(contents: &str) -> Result<ParsedSession, AppError> {
    let mut metadata = BTreeMap::new();
    let mut messages = Vec::new();
    let mut assistant_buffer = String::new();

    for value in json_values(contents)? {
        collect_metadata(&mut metadata, &value);
        collect_messages(&mut messages, &value);
        collect_event_message(&mut messages, &mut assistant_buffer, &value);
    }
    flush_assistant_buffer(&mut messages, &mut assistant_buffer);
    Ok(ParsedSession { metadata, messages })
}

fn json_values(contents: &str) -> Result<Vec<Value>, AppError> {
    if let Ok(value) = serde_json::from_str::<Value>(contents) {
        return Ok(vec![value]);
    }
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str::<Value>(line).map_err(AppError::operation))
        .collect()
}

fn collect_metadata(metadata: &mut BTreeMap<String, String>, value: &Value) {
    let object = value
        .get("session")
        .and_then(Value::as_object)
        .or_else(|| value.as_object());
    let Some(object) = object else {
        return;
    };
    for key in [
        "session_id",
        "id",
        "title",
        "name",
        "session_name",
        "created",
        "created_at",
        "last_active",
        "updated_at",
        "provider",
        "model",
    ] {
        if let Some(value) = object.get(key).and_then(scalar_to_string) {
            let normalized = match key {
                "id" => "session_id",
                "name" | "session_name" => "title",
                "created_at" => "created",
                "updated_at" => "last_active",
                other => other,
            };
            metadata.entry(normalized.to_string()).or_insert(value);
        }
    }
}

fn collect_messages(messages: &mut Vec<ParsedMessage>, value: &Value) {
    if let Some(items) = value.get("messages").and_then(Value::as_array) {
        for item in items {
            if let Some(message) = message_from_value(item) {
                messages.push(message);
            }
        }
    }
    if let Some(message) = message_from_value(value) {
        messages.push(message);
    }
}

fn collect_event_message(messages: &mut Vec<ParsedMessage>, buffer: &mut String, value: &Value) {
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return;
    };
    match event_type {
        "message_update" => {
            let assistant_event = value
                .get("assistant_event")
                .or_else(|| value.get("event"))
                .and_then(Value::as_object);
            let Some(assistant_event) = assistant_event else {
                return;
            };
            match assistant_event.get("type").and_then(Value::as_str) {
                Some("text_delta" | "thinking_delta") => {
                    if let Some(text) = assistant_event.get("text").and_then(Value::as_str) {
                        buffer.push_str(text);
                    }
                }
                Some("done") => flush_assistant_buffer(messages, buffer),
                _ => {}
            }
        }
        "agent_end" | "message_end" => flush_assistant_buffer(messages, buffer),
        "tool_execution_end" => {
            flush_assistant_buffer(messages, buffer);
            let mut metadata = BTreeMap::new();
            if let Some(name) = value.get("name").and_then(scalar_to_string) {
                metadata.insert("tool".to_string(), name);
            }
            if let Some(error) = value.get("error").and_then(scalar_to_string) {
                metadata.insert("error".to_string(), error);
            }
            let content = value
                .get("output")
                .and_then(scalar_to_string)
                .unwrap_or_default();
            messages.push(ParsedMessage {
                role: MessageRole::Tool,
                content,
                metadata,
            });
        }
        _ => {}
    }
}

fn message_from_value(value: &Value) -> Option<ParsedMessage> {
    let object = value.as_object()?;
    let role = object
        .get("role")
        .or_else(|| object.get("speaker"))
        .and_then(Value::as_str)
        .and_then(MessageRole::parse)?;
    let content = object
        .get("content")
        .or_else(|| object.get("text"))
        .or_else(|| object.get("message"))
        .and_then(scalar_to_string)?;
    let mut metadata = BTreeMap::new();
    for key in ["id", "message_id", "created", "time", "model", "provider"] {
        if let Some(value) = object.get(key).and_then(scalar_to_string) {
            metadata.insert(key.to_string(), value);
        }
    }
    Some(ParsedMessage {
        role,
        content,
        metadata,
    })
}

fn flush_assistant_buffer(messages: &mut Vec<ParsedMessage>, buffer: &mut String) {
    let content = buffer.trim().to_string();
    if !content.is_empty() {
        messages.push(ParsedMessage {
            role: MessageRole::Assistant,
            content,
            metadata: BTreeMap::new(),
        });
        buffer.clear();
    }
}

fn flush_message(
    messages: &mut Vec<ParsedMessage>,
    role: Option<MessageRole>,
    current: &mut Vec<String>,
) {
    let content = current.join("\n").trim().to_string();
    if let Some(role) = role.filter(|_| !content.is_empty()) {
        messages.push(ParsedMessage {
            role,
            content,
            metadata: BTreeMap::new(),
        });
    }
    current.clear();
}

fn render_markdown_export(session: &ParsedSession) -> String {
    let mut metadata = session.metadata.clone();
    metadata
        .entry("type".to_string())
        .or_insert_with(|| "agent-session".to_string());
    metadata
        .entry("schema_version".to_string())
        .or_insert_with(|| "1".to_string());
    metadata
        .entry("source".to_string())
        .or_insert_with(|| "vulcan-assistant".to_string());
    let title = metadata
        .get("title")
        .cloned()
        .unwrap_or_else(|| "Assistant Session".to_string());

    let mut output = String::new();
    output.push_str("---\n");
    output.push_str(&serde_yaml::to_string(&metadata).unwrap_or_default());
    output.push_str("---\n\n");
    let _ = writeln!(output, "# Agent Session: {title}\n");
    for message in &session.messages {
        let _ = writeln!(output, "## {}\n", message.role.heading());
        if !message.metadata.is_empty() {
            output.push_str("> [!metadata]- Message Info\n");
            output.push_str("> | Property | Value |\n");
            output.push_str("> | -------- | ----- |\n");
            for (key, value) in &message.metadata {
                let _ = writeln!(output, "> | {} | {} |", key, value.replace('|', "\\|"));
            }
            output.push('\n');
        }
        let _ = writeln!(output, "> [!{}]+", message.role.callout());
        for line in message.content.lines() {
            if line.is_empty() {
                output.push_str(">\n");
            } else {
                output.push_str("> ");
                output.push_str(line);
                output.push('\n');
            }
        }
        output.push_str("\n---\n\n");
    }
    output
}

fn export_filename(session: &ParsedSession, session_path: &Path) -> String {
    let stem = session
        .metadata
        .get("title")
        .or_else(|| session.metadata.get("session_id"))
        .cloned()
        .or_else(|| {
            session_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "assistant-session".to_string());
    format!("{}.md", slugify(&stem))
}

fn heading_role(line: &str) -> Option<MessageRole> {
    line.strip_prefix("## ")
        .and_then(|heading| MessageRole::parse(heading.trim()))
}

fn callout_role(line: &str) -> Option<MessageRole> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("[!user]") {
        Some(MessageRole::User)
    } else if lower.contains("[!assistant]") {
        Some(MessageRole::Assistant)
    } else if lower.contains("[!tool]") {
        Some(MessageRole::Tool)
    } else {
        None
    }
}

impl MessageRole {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "user" => Some(Self::User),
            "assistant" | "model" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Assistant => "Assistant",
            Self::Tool => "Tool",
            Self::System => "System",
        }
    }

    fn callout(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::System => "note",
        }
    }
}

fn yaml_mapping_to_strings(value: &serde_yaml::Value) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    let Some(mapping) = value.as_mapping() else {
        return metadata;
    };
    for (key, value) in mapping {
        if let Some(key) = key.as_str() {
            metadata.insert(key.to_string(), yaml_scalar_to_string(value));
        }
    }
    metadata
}

fn yaml_scalar_to_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Null => String::new(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::String(value) => value.clone(),
        _ => serde_yaml::to_string(value)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        other => Some(other.to_string()),
    }
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if matches!(character, ' ' | '-' | '_' | '.') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "assistant-session".to_string()
    } else {
        slug.to_string()
    }
}

fn display_path(vault_root: &Path, path: &Path) -> String {
    path.strip_prefix(vault_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_jsonl_messages_and_events() {
        let contents =
            r#"{"session":{"id":"s1","title":"Daily Review","created":"2026-05-09T10:00:00Z"}}"#
                .to_string()
                + "\n"
                + r#"{"role":"user","content":"What changed?"}"#
                + "\n"
                + r#"{"type":"message_update","assistant_event":{"type":"text_delta","text":"A lot"}}"#
                + "\n"
                + r#"{"type":"message_update","assistant_event":{"type":"done"}}"#;
        let session = parse_session(&contents).expect("session should parse");

        assert_eq!(
            session.metadata.get("session_id").map(String::as_str),
            Some("s1")
        );
        assert_eq!(
            session.metadata.get("title").map(String::as_str),
            Some("Daily Review")
        );
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].role, MessageRole::User);
        assert_eq!(session.messages[1].content, "A lot");
    }

    #[test]
    fn parses_obsidian_callout_session() {
        let session = parse_session(
            r"---
session_id: session_1
type: agent-session
title: Yggdrasil Political Landscape
---

# Agent Session

## User

> [!metadata]- Message Info
> | Time | 2026-03-01T16:32:45.067Z |

> [!user]+
> Summarize the politics.

---
## Model

> [!assistant]+
> The major powers are in tension.
",
        )
        .expect("session should parse");

        assert_eq!(
            session.metadata.get("title").map(String::as_str),
            Some("Yggdrasil Political Landscape")
        );
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].content, "Summarize the politics.");
        assert_eq!(session.messages[1].role, MessageRole::Assistant);
    }

    #[test]
    fn exports_markdown_callout_note() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault = temp_dir.path();
        let session_dir = vault.join("AI/Sessions");
        fs::create_dir_all(&session_dir).expect("session dir should be created");
        let session_path = session_dir.join("s1.jsonl");
        fs::write(
            &session_path,
            r#"{"session":{"id":"s1","title":"Daily Review"}}"#.to_string()
                + "\n"
                + r#"{"role":"user","content":"Review today"}"#,
        )
        .expect("session should be written");

        let report = export_assistant_session_file(
            vault,
            &session_path,
            &vault.join("AI/Assistant Sessions"),
        )
        .expect("session should export");
        let export_path = vault.join(&report.export_path);
        let exported = fs::read_to_string(export_path).expect("export should be readable");

        assert_eq!(report.message_count, 1);
        assert!(exported.contains("source: vulcan-assistant"));
        assert!(exported.contains("> [!user]+"));
        assert!(exported.contains("> Review today"));
    }
}
