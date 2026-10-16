//! Single-note reads a client may route: `note info`, and the links and
//! backlinks of one note (`note links`/`links`, `note backlinks`/`backlinks`).

use crate::notes::{build_note_info_report, NoteInfoReport};
use crate::AppError;
use serde::{Deserialize, Serialize};
use vulcan_core::{
    query_backlinks_with_filter, query_links_with_filter, resolve_note_reference, BacklinksReport,
    OutgoingLinksReport, PermissionFilter, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "read", rename_all = "snake_case")]
pub enum NoteReadRequest {
    Info { note: String },
    Links { note: String },
    Backlinks { note: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "report", content = "value", rename_all = "snake_case")]
pub enum NoteReadAnswer {
    Info(NoteInfoReport),
    Links(OutgoingLinksReport),
    Backlinks(BacklinksReport),
}

/// Answers `request` from the vault's cache under `guard`. `note info`
/// checks that the note itself is readable and reports it unfiltered, as the
/// CLI always has; links and backlinks apply the guard's read filter.
pub fn run_note_read(
    paths: &VaultPaths,
    request: &NoteReadRequest,
    guard: &ProfilePermissionGuard,
) -> Result<NoteReadAnswer, AppError> {
    match request {
        NoteReadRequest::Info { note } => {
            if !guard.read_filter().path_permission().is_unrestricted() || guard.has_policy_hook() {
                let resolved = resolve_note_reference(paths, note).map_err(AppError::operation)?;
                guard
                    .check_read_path(&resolved.path)
                    .map_err(AppError::operation)?;
            }
            build_note_info_report(paths, note, None).map(NoteReadAnswer::Info)
        }
        NoteReadRequest::Links { note } => {
            query_links_with_filter(paths, note, restricting_filter(guard).as_ref())
                .map(NoteReadAnswer::Links)
                .map_err(AppError::operation)
        }
        NoteReadRequest::Backlinks { note } => {
            query_backlinks_with_filter(paths, note, restricting_filter(guard).as_ref())
                .map(NoteReadAnswer::Backlinks)
                .map_err(AppError::operation)
        }
    }
}

fn restricting_filter(guard: &ProfilePermissionGuard) -> Option<PermissionFilter> {
    let filter = guard.read_filter();
    (!filter.path_permission().is_unrestricted()).then_some(filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    #[test]
    fn note_reads_answer_like_the_direct_queries_and_survive_the_wire() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("vault");
        copy_fixture("basic", &root);
        let paths = VaultPaths::new(&root);
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard =
            ProfilePermissionGuard::new(&paths, resolve_permission_profile(&paths, None).unwrap());
        for note in ["Home", "Projects/Alpha"] {
            for request in [
                NoteReadRequest::Info {
                    note: note.to_string(),
                },
                NoteReadRequest::Links {
                    note: note.to_string(),
                },
                NoteReadRequest::Backlinks {
                    note: note.to_string(),
                },
            ] {
                let answer = run_note_read(&paths, &request, &guard).unwrap();
                let direct = match &request {
                    NoteReadRequest::Info { note } => {
                        NoteReadAnswer::Info(build_note_info_report(&paths, note, None).unwrap())
                    }
                    NoteReadRequest::Links { note } => {
                        NoteReadAnswer::Links(query_links_with_filter(&paths, note, None).unwrap())
                    }
                    NoteReadRequest::Backlinks { note } => NoteReadAnswer::Backlinks(
                        query_backlinks_with_filter(&paths, note, None).unwrap(),
                    ),
                };
                assert_eq!(answer, direct, "{request:?}");
                let decoded: NoteReadAnswer =
                    serde_json::from_slice(&serde_json::to_vec(&answer).unwrap()).unwrap();
                assert_eq!(decoded, answer, "{request:?}");
                let request_decoded: NoteReadRequest =
                    serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
                assert_eq!(request_decoded, request);
            }
        }
        assert!(run_note_read(
            &paths,
            &NoteReadRequest::Info {
                note: "Missing".to_string()
            },
            &guard
        )
        .is_err());
    }

    #[test]
    fn a_restricted_guard_hides_notes_it_cannot_read() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("vault");
        copy_fixture("basic", &root);
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::write(
            root.join(".vulcan/config.toml"),
            "[permissions.profiles.projects]\nread = { allow = [\"folder:Projects/**\"] }\n",
        )
        .unwrap();
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("projects")).unwrap(),
        );
        assert!(run_note_read(
            &paths,
            &NoteReadRequest::Info {
                note: "Home".to_string()
            },
            &guard
        )
        .is_err());
        let NoteReadAnswer::Backlinks(report) = run_note_read(
            &paths,
            &NoteReadRequest::Backlinks {
                note: "Projects/Alpha".to_string(),
            },
            &guard,
        )
        .unwrap() else {
            panic!("backlinks");
        };
        assert!(report
            .backlinks
            .iter()
            .all(|backlink| backlink.source_path.starts_with("Projects/")));
    }
}
