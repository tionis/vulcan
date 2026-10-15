//! The routed `ls` operation: notes matching `--where` filters and a tag,
//! in path order.

use serde::{Deserialize, Serialize};
use vulcan_core::note_store::NoteStore;
use vulcan_core::properties::{query_notes_page_in, query_notes_stored_in, NoteQuery};
use vulcan_core::{PermissionFilter, QueryAst, QueryReport, VaultPaths};

use crate::routed::query::printed_page;
use crate::AppError;

/// An `ls` with the paging the client will print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LsRequest {
    /// `--where` filters.
    pub filters: Vec<String>,
    pub tag: Option<String>,
    /// Whether the client prints only paths or a count, which read nothing
    /// a hydrated note adds.
    pub stored_only: bool,
    /// The client's `--offset` (0 when not given).
    pub offset: usize,
    /// The client's `--limit`, when given.
    pub limit: Option<usize>,
    /// Whether a filename glob filters the notes before printing.
    pub glob: bool,
}

impl LsRequest {
    fn note_query(&self) -> NoteQuery {
        let mut filters = self.filters.clone();
        if let Some(tag) = &self.tag {
            filters.push(format!("file.tags has_tag {tag}"));
        }
        NoteQuery {
            filters,
            sort_by: Some("file.path".to_string()),
            sort_descending: false,
        }
    }
}

/// Run `request` over `store`: the cache directly for the CLI, a retained
/// session for a daemon. Only the printed page is hydrated.
pub fn run_ls(
    store: &dyn NoteStore,
    paths: &VaultPaths,
    request: &LsRequest,
    filter: Option<&PermissionFilter>,
) -> Result<QueryReport, AppError> {
    let note_query = request.note_query();
    let notes = if request.stored_only {
        query_notes_stored_in(store, paths, &note_query, filter)
    } else {
        query_notes_page_in(
            store,
            paths,
            &note_query,
            filter,
            printed_page(request.offset, request.limit, request.glob),
        )
    }
    .map_err(AppError::operation)?;
    Ok(QueryReport {
        query: QueryAst::from_note_query(&note_query),
        notes: notes.notes,
        selection: None,
        selection_provenance: Vec::new(),
        plan: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use crate::routed::wire::QueryReportWire;
    use vulcan_core::note_store::DirectNoteStore;
    use vulcan_core::properties::query_notes_with_filter;
    use vulcan_core::{scan_vault, ScanMode};

    fn request(filters: &[&str], tag: Option<&str>, stored_only: bool) -> LsRequest {
        LsRequest {
            filters: filters.iter().map(ToString::to_string).collect(),
            tag: tag.map(ToString::to_string),
            stored_only,
            offset: 0,
            limit: None,
            glob: false,
        }
    }

    /// Every note `ls` lists is the note the unpaged note query returns,
    /// and a page is that list's prefix; reports survive the wire.
    #[test]
    fn ls_matches_the_note_query_and_pages_its_prefix() {
        for fixture in ["basic", "dataview", "tasknotes", "mixed-properties"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("vault");
            copy_fixture(fixture, &root);
            let paths = VaultPaths::new(&root);
            std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
            scan_vault(&paths, ScanMode::Full).unwrap();
            let store = DirectNoteStore::new(&paths);
            for (filters, tag) in [
                (&[][..], None),
                (&["file.path starts_with \"\""][..], None),
                (&[][..], Some("project")),
            ] {
                let full = request(filters, tag, false);
                let expected = query_notes_with_filter(&paths, &full.note_query(), None)
                    .unwrap()
                    .notes;
                let report = run_ls(&store, &paths, &full, None).unwrap();
                assert_eq!(report.notes, expected, "{fixture} {filters:?} {tag:?}");
                let paged = LsRequest {
                    offset: 1,
                    limit: Some(2),
                    ..full.clone()
                };
                let page = run_ls(&store, &paths, &paged, None).unwrap();
                let shown = expected.len().min(3);
                assert!(page.notes.len() >= shown, "{fixture}");
                assert_eq!(page.notes[..shown], expected[..shown], "{fixture}");
                let stored = run_ls(&store, &paths, &request(filters, tag, true), None).unwrap();
                let stored_paths = stored
                    .notes
                    .iter()
                    .map(|note| &note.document_path)
                    .collect::<Vec<_>>();
                let expected_paths = expected
                    .iter()
                    .map(|note| &note.document_path)
                    .collect::<Vec<_>>();
                assert_eq!(stored_paths, expected_paths, "{fixture}");
                for report in [report, page, stored] {
                    let wire = QueryReportWire::from_report(report.clone()).unwrap();
                    let decoded: QueryReportWire =
                        serde_json::from_slice(&serde_json::to_vec(&wire).unwrap()).unwrap();
                    assert_eq!(QueryReport::from(decoded), report, "{fixture}");
                }
            }
        }
    }
}
