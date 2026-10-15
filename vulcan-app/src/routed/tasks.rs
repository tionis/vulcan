//! The routed `tasks query` operation: a Tasks-plugin query over the
//! vault's tasks.
//!
//! A [`TasksQueryResult`] serializes every field, so the result itself is
//! the wire form. The `TaskNotes` auto-archive upkeep the CLI runs before a
//! task command may write, so it stays with the client.

use serde::{Deserialize, Serialize};
use vulcan_core::note_session::NoteStoreSession;
use vulcan_core::tasks::TasksQueryResult;
use vulcan_core::{ProfilePermissionGuard, VaultPaths};

use crate::AppError;

/// A `tasks query`: the query source, before the configured global filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksQueryRequest {
    pub source: String,
}

/// Run `request` through `session` when a host retains one, or the cache
/// directly.
pub fn run_tasks_query(
    session: Option<&NoteStoreSession>,
    paths: &VaultPaths,
    request: &TasksQueryRequest,
    guard: &ProfilePermissionGuard,
) -> Result<TasksQueryResult, AppError> {
    crate::tasks::build_tasks_query_result_in(session, paths, &request.source, guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    /// Results survive JSON in every field, and a retained session answers
    /// as the cache does.
    #[test]
    fn tasks_query_results_round_trip_and_match_through_a_session() {
        let mut compared = 0;
        for fixture in ["tasknotes", "dataview"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("vault");
            copy_fixture(fixture, &root);
            let paths = VaultPaths::new(&root);
            std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
            scan_vault(&paths, ScanMode::Full).unwrap();
            let guard = ProfilePermissionGuard::new(
                &paths,
                resolve_permission_profile(&paths, None).unwrap(),
            );
            let session = NoteStoreSession::new(paths.clone());
            for source in [
                "not done",
                "done",
                "not done\nsort by due\ngroup by path",
                "not done\nlimit 2\nexplain",
                "",
            ] {
                let request = TasksQueryRequest {
                    source: source.to_string(),
                };
                let mut direct = run_tasks_query(None, &paths, &request, &guard).unwrap();
                compared += direct.result_count;
                let decoded: TasksQueryResult =
                    serde_json::from_slice(&serde_json::to_vec(&direct).unwrap()).unwrap();
                assert_eq!(decoded, direct, "{fixture}: {source}");
                let mut retained =
                    run_tasks_query(Some(&session), &paths, &request, &guard).unwrap();
                // The note plan says how the tasks were loaded and carries
                // timings; only the `explain` instruction reports it.
                direct.note_plan = None;
                retained.note_plan = None;
                assert_eq!(retained, direct, "{fixture}: {source}");
            }
        }
        assert!(compared > 0, "the fixtures yield tasks to compare");
    }
}
