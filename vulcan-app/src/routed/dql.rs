//! The routed DQL `query` operation (`query --language dql`, or a query
//! that looks like DQL).
//!
//! A [`DqlQueryResult`] serializes every field, so the result itself is the
//! wire form.

use serde::{Deserialize, Serialize};
use vulcan_core::dql::DqlQueryResult;
use vulcan_core::note_session::NoteStoreSession;
use vulcan_core::{ProfilePermissionGuard, VaultPaths};

use crate::AppError;

/// A DQL query as the CLI runs it: no source note, so `this` is unbound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DqlRequest {
    pub source: String,
    /// Whether to report the note plan (`--explain`).
    pub explain: bool,
}

/// Run `request` through `session` when a host retains one, or the cache
/// directly.
pub fn run_dql(
    session: Option<&NoteStoreSession>,
    paths: &VaultPaths,
    request: &DqlRequest,
    guard: &ProfilePermissionGuard,
) -> Result<DqlQueryResult, AppError> {
    crate::browse::build_dataview_query_report_in(
        session,
        paths,
        &request.source,
        None,
        guard,
        request.explain,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use vulcan_core::{resolve_permission_profile, scan_vault, ScanMode};

    /// Results survive JSON in every field, and a retained session answers
    /// as the cache does.
    #[test]
    fn dql_results_round_trip_and_match_through_a_session() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_fixture("dataview", &root);
        let paths = VaultPaths::new(&root);
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let guard =
            ProfilePermissionGuard::new(&paths, resolve_permission_profile(&paths, None).unwrap());
        let session = NoteStoreSession::new(paths.clone());
        let mut rows = 0;
        for source in [
            "TABLE status, priority FROM \"\" SORT file.name ASC",
            "LIST FROM \"\" WHERE status",
            "TASK FROM \"\"",
            "TABLE length(rows) AS n FROM \"\" GROUP BY status",
        ] {
            let request = DqlRequest {
                source: source.to_string(),
                explain: false,
            };
            let direct = run_dql(None, &paths, &request, &guard).unwrap();
            rows += direct.rows.len();
            let decoded: DqlQueryResult =
                serde_json::from_slice(&serde_json::to_vec(&direct).unwrap()).unwrap();
            assert_eq!(decoded, direct, "{source}");
            let retained = run_dql(Some(&session), &paths, &request, &guard).unwrap();
            assert_eq!(retained, direct, "{source}");
        }
        assert!(rows > 0, "the fixture yields rows to compare");
    }
}
