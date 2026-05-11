use std::fs;

use tempfile::tempdir;
use vulcan_core::{
    initialize_vulcan_dir, query_notes, render_note_html, scan_vault, NoteQuery, ScanMode,
    VaultPaths,
};

#[test]
fn minimal_build_initializes_scans_queries_and_renders_markdown() {
    let temp = tempdir().expect("tempdir should be created");
    let root = temp.path();
    fs::write(
        root.join("Home.md"),
        "---\ntags: [project]\n---\n\n# Home\n\nHello [[Project]].",
    )
    .expect("note should be written");
    fs::write(root.join("Project.md"), "# Project\n\nBacklink target.")
        .expect("note should be written");

    let paths = VaultPaths::new(root);
    initialize_vulcan_dir(&paths).expect("vault should initialize");
    let summary = scan_vault(&paths, ScanMode::Full).expect("vault should scan");
    assert_eq!(summary.added, 2);

    let report = query_notes(
        &paths,
        &NoteQuery {
            filters: vec!["file.tags has_tag #project".to_string()],
            sort_by: Some("file.name".to_string()),
            sort_descending: false,
        },
    )
    .expect("query should succeed");
    assert_eq!(report.notes.len(), 1);
    assert_eq!(report.notes[0].document_path, "Home.md");

    let rendered = render_note_html(
        &paths,
        "Home.md",
        &fs::read_to_string(root.join("Home.md")).expect("note should be readable"),
    );
    assert!(rendered.html.contains("Home</h1>"));
    assert!(rendered.html.contains("Project"));
}
