//! Full-fidelity wire forms of routed reports.
//!
//! The JSON the CLI prints for a report omits internal fields that its
//! printers still use (aliases, inlinks, tasks, list items, and more), so it
//! cannot carry a report between processes. These types carry every field,
//! and converting a report to its wire form and back yields an equal report.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use vulcan_core::properties::{
    EvaluatedInlineExpression, NoteListItemRecord, NoteRecord, NoteTaskRecord,
};
use vulcan_core::{QueryAst, QueryReport};

/// Every field of a [`NoteRecord`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteRecordWire {
    pub document_id: String,
    pub document_path: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_mtime: i64,
    pub file_ctime: i64,
    pub file_size: i64,
    pub properties: Value,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub starred: bool,
    pub inlinks: Vec<String>,
    pub aliases: Vec<String>,
    pub frontmatter: Value,
    pub periodic_type: Option<String>,
    pub periodic_date: Option<String>,
    pub list_items: Vec<NoteListItemRecord>,
    pub tasks: Vec<NoteTaskRecord>,
    pub raw_inline_expressions: Vec<String>,
    pub inline_expressions: Vec<EvaluatedInlineExpression>,
}

impl From<NoteRecord> for NoteRecordWire {
    fn from(note: NoteRecord) -> Self {
        Self {
            document_id: note.document_id,
            document_path: note.document_path,
            file_name: note.file_name,
            file_ext: note.file_ext,
            file_mtime: note.file_mtime,
            file_ctime: note.file_ctime,
            file_size: note.file_size,
            properties: note.properties,
            tags: note.tags,
            links: note.links,
            starred: note.starred,
            inlinks: note.inlinks,
            aliases: note.aliases,
            frontmatter: note.frontmatter,
            periodic_type: note.periodic_type,
            periodic_date: note.periodic_date,
            list_items: note.list_items,
            tasks: note.tasks,
            raw_inline_expressions: note.raw_inline_expressions,
            inline_expressions: note.inline_expressions,
        }
    }
}

impl NoteRecordWire {
    /// A record carrying only its path, for printers that read nothing else.
    #[must_use]
    pub fn path_only(document_path: String) -> Self {
        Self {
            document_id: String::new(),
            document_path,
            file_name: String::new(),
            file_ext: String::new(),
            file_mtime: 0,
            file_ctime: 0,
            file_size: 0,
            properties: Value::Null,
            tags: Vec::new(),
            links: Vec::new(),
            starred: false,
            inlinks: Vec::new(),
            aliases: Vec::new(),
            frontmatter: Value::Null,
            periodic_type: None,
            periodic_date: None,
            list_items: Vec::new(),
            tasks: Vec::new(),
            raw_inline_expressions: Vec::new(),
            inline_expressions: Vec::new(),
        }
    }
}

impl From<NoteRecordWire> for NoteRecord {
    fn from(note: NoteRecordWire) -> Self {
        Self {
            document_id: note.document_id,
            document_path: note.document_path,
            file_name: note.file_name,
            file_ext: note.file_ext,
            file_mtime: note.file_mtime,
            file_ctime: note.file_ctime,
            file_size: note.file_size,
            properties: note.properties,
            tags: note.tags,
            links: note.links,
            starred: note.starred,
            inlinks: note.inlinks,
            aliases: note.aliases,
            frontmatter: note.frontmatter,
            periodic_type: note.periodic_type,
            periodic_date: note.periodic_date,
            list_items: note.list_items,
            tasks: note.tasks,
            raw_inline_expressions: note.raw_inline_expressions,
            inline_expressions: note.inline_expressions,
        }
    }
}

/// A [`QueryReport`] from one canonical query. Publication selections are
/// not routed, so a report with a selection has no wire form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryReportWire {
    pub query: QueryAst,
    pub notes: Vec<NoteRecordWire>,
    pub plan: Option<vulcan_core::plan::QueryPlanExplain>,
}

impl QueryReportWire {
    /// `None` for a report that carries a publication selection.
    #[must_use]
    pub fn from_report(report: QueryReport) -> Option<Self> {
        if report.selection.is_some() || !report.selection_provenance.is_empty() {
            return None;
        }
        Some(Self {
            query: report.query,
            notes: report.notes.into_iter().map(NoteRecordWire::from).collect(),
            plan: report.plan,
        })
    }
}

impl From<QueryReportWire> for QueryReport {
    fn from(report: QueryReportWire) -> Self {
        Self {
            query: report.query,
            notes: report.notes.into_iter().map(NoteRecord::from).collect(),
            selection: None,
            selection_provenance: Vec::new(),
            plan: report.plan,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use vulcan_core::{scan_vault, ScanMode, VaultPaths};

    /// Every field of a record survives, including those a query might
    /// leave empty (tasks, list items, periodic metadata).
    #[test]
    fn a_fully_populated_note_record_round_trips() {
        let list_item = NoteListItemRecord {
            id: "item".into(),
            text: "- [ ] task".into(),
            tags: vec!["#t".into()],
            outlinks: vec!["[[B]]".into()],
            line_number: 3,
            line_count: 1,
            byte_offset: 10,
            section_heading: Some("Heading".into()),
            parent_item_id: Some("parent".into()),
            is_task: true,
            block_id: Some("block".into()),
            annotated: true,
            symbol: "-".into(),
        };
        let mut task_properties = serde_json::Map::new();
        task_properties.insert("due".into(), serde_json::json!("2026-10-15"));
        let task = NoteTaskRecord {
            id: "task".into(),
            list_item_id: "item".into(),
            status_char: " ".into(),
            status_name: "Todo".into(),
            status_type: "TODO".into(),
            status_next_symbol: Some("x".into()),
            checked: false,
            completed: false,
            text: "task".into(),
            byte_offset: 10,
            parent_task_id: Some("parent".into()),
            section_heading: Some("Heading".into()),
            line_number: 3,
            properties: task_properties,
        };
        let note = NoteRecord {
            document_id: "id".into(),
            document_path: "A.md".into(),
            file_name: "A".into(),
            file_ext: "md".into(),
            file_mtime: 1,
            file_ctime: 2,
            file_size: 3,
            properties: serde_json::json!({"status": "open", "n": 1.5}),
            tags: vec!["#a".into()],
            links: vec!["[[B]]".into()],
            starred: true,
            inlinks: vec!["[[C]]".into()],
            aliases: vec!["Alpha".into()],
            frontmatter: serde_json::json!({"aliases": ["Alpha"]}),
            periodic_type: Some("daily".into()),
            periodic_date: Some("2026-10-15".into()),
            list_items: vec![list_item],
            tasks: vec![task],
            raw_inline_expressions: vec!["= 1 + 1".into()],
            inline_expressions: vec![EvaluatedInlineExpression {
                expression: "1 + 1".into(),
                value: serde_json::json!(2),
                error: Some("none".into()),
            }],
        };
        let text = serde_json::to_string(&NoteRecordWire::from(note.clone())).unwrap();
        let decoded: NoteRecordWire = serde_json::from_str(&text).unwrap();
        assert_eq!(NoteRecord::from(decoded), note);
    }

    /// A report crosses the wire unchanged in every field, including those
    /// the printed JSON omits, for vaults with tasks, list items, aliases,
    /// periodic notes, inline fields, and links.
    #[test]
    fn query_reports_round_trip_through_the_wire_in_every_field() {
        for fixture in ["basic", "dataview", "tasknotes", "mixed-properties"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("vault");
            copy_fixture(fixture, &root);
            let paths = VaultPaths::new(&root);
            std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
            scan_vault(&paths, ScanMode::Full).unwrap();
            let report = vulcan_core::execute_query_report_explained(
                &paths,
                QueryAst::from_dsl("from notes").unwrap(),
                None,
            )
            .unwrap();
            assert!(!report.notes.is_empty(), "{fixture}");
            let wire = QueryReportWire::from_report(report.clone()).unwrap();
            let text = serde_json::to_string(&wire).unwrap();
            let decoded: QueryReportWire = serde_json::from_str(&text).unwrap();
            assert_eq!(QueryReport::from(decoded), report, "{fixture}");
        }
    }
}
