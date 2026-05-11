use super::ExportedNoteDocument;
use crate::AppError;
use serde::Serialize;
use serde_json::Value;
use vulcan_core::{EvaluatedInlineExpression, QueryAst, QueryReport};

#[derive(Debug, Clone, Serialize)]
pub struct JsonNoteExportDocument {
    pub document_path: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_mtime: i64,
    pub file_size: i64,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub inlinks: Vec<String>,
    pub aliases: Vec<String>,
    pub frontmatter: Value,
    pub properties: Value,
    pub inline_expressions: Vec<EvaluatedInlineExpression>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonNotesExportReport {
    pub query: QueryAst,
    pub result_count: usize,
    pub notes: Vec<JsonNoteExportDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MarkdownExportSummary {
    pub path: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CsvExportSummary {
    pub path: String,
    pub result_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsonExportSummary {
    pub path: String,
    pub result_count: usize,
}

fn json_note_export_report(
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
) -> JsonNotesExportReport {
    JsonNotesExportReport {
        query: report.query.clone(),
        result_count: notes.len(),
        notes: notes
            .iter()
            .map(|entry| JsonNoteExportDocument {
                document_path: entry.note.document_path.clone(),
                file_name: entry.note.file_name.clone(),
                file_ext: entry.note.file_ext.clone(),
                file_mtime: entry.note.file_mtime,
                file_size: entry.note.file_size,
                tags: entry.note.tags.clone(),
                links: entry.note.links.clone(),
                inlinks: entry.note.inlinks.clone(),
                aliases: entry.note.aliases.clone(),
                frontmatter: entry.note.frontmatter.clone(),
                properties: entry.note.properties.clone(),
                inline_expressions: entry.note.inline_expressions.clone(),
                content: entry.content.clone(),
            })
            .collect(),
    }
}

pub fn render_json_export_payload(
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
    pretty: bool,
) -> Result<String, AppError> {
    let payload = json_note_export_report(report, notes);
    if pretty {
        serde_json::to_string_pretty(&payload).map_err(AppError::operation)
    } else {
        serde_json::to_string(&payload).map_err(AppError::operation)
    }
}

#[must_use]
pub fn render_markdown_export_payload(
    notes: &[ExportedNoteDocument],
    title: Option<&str>,
) -> String {
    if notes.len() == 1 && title.is_none() {
        let mut rendered = notes[0].content.clone();
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        return rendered;
    }

    let mut rendered = String::new();
    if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
        rendered.push_str("# ");
        rendered.push_str(title);
        rendered.push_str("\n\n");
    }

    for (index, note) in notes.iter().enumerate() {
        if index > 0 {
            rendered.push_str("\n\n---\n\n");
        }
        rendered.push_str("## ");
        rendered.push_str(&note.note.document_path);
        rendered.push_str("\n\n");
        rendered.push_str(&note.content);
        if !note.content.ends_with('\n') {
            rendered.push('\n');
        }
    }

    rendered
}

fn query_export_rows(report: &QueryReport) -> Result<Vec<Value>, AppError> {
    let query_value = serde_json::to_value(&report.query).map_err(AppError::operation)?;
    Ok(report
        .notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "document_path": note.document_path,
                "file_name": note.file_name,
                "file_ext": note.file_ext,
                "file_mtime": note.file_mtime,
                "tags": note.tags,
                "starred": note.starred,
                "properties": note.properties,
                "inline_expressions": note.inline_expressions,
                "query": query_value,
            })
        })
        .collect())
}

fn query_export_fields() -> &'static [&'static str] {
    &[
        "document_path",
        "file_name",
        "file_ext",
        "file_mtime",
        "tags",
        "starred",
        "properties",
        "inline_expressions",
        "query",
    ]
}

pub fn render_csv_export_payload(report: &QueryReport) -> Result<String, AppError> {
    let rows = query_export_rows(report)?;
    let fields = query_export_fields();
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(fields.iter().copied())
        .map_err(AppError::operation)?;
    for row in &rows {
        let selected = row
            .as_object()
            .ok_or_else(|| AppError::operation("query export row was not an object"))?;
        let record = fields
            .iter()
            .map(|field| csv_cell_for_value(selected.get(*field)))
            .collect::<Vec<_>>();
        writer.write_record(record).map_err(AppError::operation)?;
    }
    writer.flush().map_err(AppError::operation)?;
    let bytes = writer.into_inner().map_err(AppError::operation)?;
    String::from_utf8(bytes)
        .map_err(|error| AppError::operation(format!("csv export was not valid UTF-8: {error}")))
}

fn csv_cell_for_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}
