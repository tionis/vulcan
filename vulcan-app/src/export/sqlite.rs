use super::{prepare_export_output_path, ExportLinkRecord, ExportedNoteDocument};
use crate::templates::TemplateTimestamp;
use crate::AppError;
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;
use vulcan_core::QueryReport;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SqliteExportSummary {
    pub path: String,
    pub result_count: usize,
    pub link_count: usize,
    pub tag_count: usize,
    pub task_count: usize,
}

fn initialize_sqlite_export(connection: &Connection) -> Result<(), AppError> {
    connection
        .execute_batch(
            "
            PRAGMA user_version = 1;

            CREATE TABLE meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE notes (
                document_path TEXT PRIMARY KEY,
                file_name TEXT NOT NULL,
                file_ext TEXT NOT NULL,
                file_mtime INTEGER NOT NULL,
                file_size INTEGER NOT NULL,
                tags_json TEXT NOT NULL,
                aliases_json TEXT NOT NULL,
                frontmatter_json TEXT NOT NULL,
                properties_json TEXT NOT NULL,
                content TEXT NOT NULL
            );

            CREATE TABLE links (
                source_document_path TEXT NOT NULL,
                raw_text TEXT NOT NULL,
                link_kind TEXT NOT NULL,
                display_text TEXT,
                target_path_candidate TEXT,
                target_heading TEXT,
                target_block TEXT,
                resolved_target_path TEXT,
                origin_context TEXT NOT NULL,
                byte_offset INTEGER NOT NULL
            );

            CREATE TABLE tags (
                document_path TEXT NOT NULL,
                tag_text TEXT NOT NULL
            );

            CREATE TABLE tasks (
                task_id TEXT PRIMARY KEY,
                document_path TEXT NOT NULL,
                task_source TEXT NOT NULL,
                text TEXT NOT NULL,
                status_char TEXT NOT NULL,
                status_name TEXT NOT NULL,
                status_type TEXT NOT NULL,
                line_number INTEGER NOT NULL,
                byte_offset INTEGER NOT NULL,
                section_heading TEXT,
                properties_json TEXT NOT NULL
            );

            CREATE INDEX idx_links_source_document_path ON links(source_document_path);
            CREATE INDEX idx_tags_document_path ON tags(document_path);
            CREATE INDEX idx_tasks_document_path ON tasks(document_path);
            ",
        )
        .map_err(AppError::operation)
}

fn insert_sqlite_export_meta(
    connection: &Connection,
    report: &QueryReport,
    result_count: usize,
) -> Result<(), AppError> {
    let query_json = serde_json::to_string(&report.query).map_err(AppError::operation)?;
    let timestamp = TemplateTimestamp::current().default_strings().datetime;
    connection
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2), (?3, ?4), (?5, ?6)",
            rusqlite::params![
                "query_json",
                query_json,
                "result_count",
                result_count.to_string(),
                "generated_at",
                timestamp
            ],
        )
        .map_err(AppError::operation)?;
    Ok(())
}

fn insert_sqlite_export_notes(
    transaction: &rusqlite::Transaction<'_>,
    notes: &[ExportedNoteDocument],
) -> Result<(usize, usize), AppError> {
    let mut tag_count = 0;
    let mut task_count = 0;

    for note in notes {
        transaction
            .execute(
                "INSERT INTO notes (
                    document_path, file_name, file_ext, file_mtime, file_size,
                    tags_json, aliases_json, frontmatter_json, properties_json, content
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    &note.note.document_path,
                    &note.note.file_name,
                    &note.note.file_ext,
                    note.note.file_mtime,
                    note.note.file_size,
                    serde_json::to_string(&note.note.tags).map_err(AppError::operation)?,
                    serde_json::to_string(&note.note.aliases).map_err(AppError::operation)?,
                    serde_json::to_string(&note.note.frontmatter).map_err(AppError::operation)?,
                    serde_json::to_string(&note.note.properties).map_err(AppError::operation)?,
                    &note.content,
                ],
            )
            .map_err(AppError::operation)?;

        for tag in &note.note.tags {
            transaction
                .execute(
                    "INSERT INTO tags (document_path, tag_text) VALUES (?1, ?2)",
                    rusqlite::params![&note.note.document_path, tag],
                )
                .map_err(AppError::operation)?;
            tag_count += 1;
        }

        for task in &note.note.tasks {
            let task_source = task
                .properties
                .get("taskSource")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("inline");
            transaction
                .execute(
                    "INSERT INTO tasks (
                        task_id, document_path, task_source, text, status_char, status_name,
                        status_type, line_number, byte_offset, section_heading, properties_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    rusqlite::params![
                        &task.id,
                        &note.note.document_path,
                        task_source,
                        &task.text,
                        &task.status_char,
                        &task.status_name,
                        &task.status_type,
                        task.line_number,
                        task.byte_offset,
                        &task.section_heading,
                        serde_json::to_string(&task.properties).map_err(AppError::operation)?,
                    ],
                )
                .map_err(AppError::operation)?;
            task_count += 1;
        }
    }

    Ok((tag_count, task_count))
}

fn insert_sqlite_export_links(
    transaction: &rusqlite::Transaction<'_>,
    links: &[ExportLinkRecord],
) -> Result<(), AppError> {
    for link in links {
        transaction
            .execute(
                "INSERT INTO links (
                    source_document_path, raw_text, link_kind, display_text, target_path_candidate,
                    target_heading, target_block, resolved_target_path, origin_context, byte_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    &link.source_document_path,
                    &link.raw_text,
                    &link.link_kind,
                    &link.display_text,
                    &link.target_path_candidate,
                    &link.target_heading,
                    &link.target_block,
                    &link.resolved_target_path,
                    &link.origin_context,
                    link.byte_offset,
                ],
            )
            .map_err(AppError::operation)?;
    }
    Ok(())
}

pub fn write_sqlite_export(
    output_path: &Path,
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
) -> Result<SqliteExportSummary, AppError> {
    prepare_export_output_path(output_path)?;
    let mut connection = Connection::open(output_path).map_err(AppError::operation)?;
    initialize_sqlite_export(&connection)?;
    insert_sqlite_export_meta(&connection, report, notes.len())?;
    let transaction = connection.transaction().map_err(AppError::operation)?;
    let (tag_count, task_count) = insert_sqlite_export_notes(&transaction, notes)?;
    insert_sqlite_export_links(&transaction, links)?;
    transaction.commit().map_err(AppError::operation)?;

    Ok(SqliteExportSummary {
        path: output_path.display().to_string(),
        result_count: notes.len(),
        link_count: links.len(),
        tag_count,
        task_count,
    })
}
