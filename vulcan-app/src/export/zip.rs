use super::{
    collect_export_attachment_paths, prepare_export_output_path, render_json_export_payload,
    ExportLinkRecord, ExportedNoteDocument, ZipExportManifest, ZipExportSummary,
};
use crate::AppError;
use std::fs;
use std::io::Write;
use std::path::Path;
use vulcan_core::{QueryReport, VaultPaths};
use zip::write::FileOptions;

pub fn write_zip_export(
    paths: &VaultPaths,
    output_path: &Path,
    report: &QueryReport,
    notes: &[ExportedNoteDocument],
    links: &[ExportLinkRecord],
) -> Result<ZipExportSummary, AppError> {
    prepare_export_output_path(output_path)?;

    let attachments = collect_export_attachment_paths(links);
    let file = fs::File::create(output_path).map_err(AppError::operation)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for note in notes {
        writer
            .start_file(&note.note.document_path, options)
            .map_err(AppError::operation)?;
        writer
            .write_all(note.content.as_bytes())
            .map_err(AppError::operation)?;
    }

    for attachment in &attachments {
        writer
            .start_file(attachment, options)
            .map_err(AppError::operation)?;
        let bytes = fs::read(paths.vault_root().join(attachment)).map_err(AppError::operation)?;
        writer.write_all(&bytes).map_err(AppError::operation)?;
    }

    let notes_json = render_json_export_payload(report, notes, true)?;
    writer
        .start_file(".vulcan-export/notes.json", options)
        .map_err(AppError::operation)?;
    writer
        .write_all(notes_json.as_bytes())
        .map_err(AppError::operation)?;

    let manifest = ZipExportManifest {
        query: report.query.clone(),
        result_count: notes.len(),
        notes: notes
            .iter()
            .map(|entry| entry.note.document_path.clone())
            .collect(),
        attachments,
    };
    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(AppError::operation)?;
    writer
        .start_file(".vulcan-export/manifest.json", options)
        .map_err(AppError::operation)?;
    writer
        .write_all(manifest_json.as_bytes())
        .map_err(AppError::operation)?;

    writer.finish().map_err(AppError::operation)?;

    Ok(ZipExportSummary {
        path: output_path.display().to_string(),
        result_count: notes.len(),
        attachment_count: manifest.attachments.len(),
    })
}
