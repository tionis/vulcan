//! The routed `search` operation: keyword full-text search.
//!
//! A [`SearchReport`] serializes every field, so the report itself is the
//! wire form. Hybrid search embeds the query with the client's provider
//! configuration and credentials, so only keyword search is routable.

use vulcan_core::search::{search_vault_with_filter, SearchMode, SearchQuery, SearchReport};
use vulcan_core::{PermissionFilter, VaultPaths};

use crate::AppError;

/// Whether a daemon can answer `query` as this process would.
#[must_use]
pub fn routable(query: &SearchQuery) -> bool {
    query.mode == SearchMode::Keyword
}

/// Run `query` against the vault's cache.
pub fn run_search(
    paths: &VaultPaths,
    query: &SearchQuery,
    filter: Option<&PermissionFilter>,
) -> Result<SearchReport, AppError> {
    search_vault_with_filter(paths, query, filter).map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use vulcan_core::{scan_vault, ScanMode};

    #[test]
    fn only_keyword_search_is_routable() {
        let keyword = SearchQuery::default();
        assert!(routable(&keyword));
        let hybrid = SearchQuery {
            mode: SearchMode::Hybrid,
            ..SearchQuery::default()
        };
        assert!(!routable(&hybrid));
    }

    /// The report JSON carries every field: decoding it yields an equal
    /// report, explain plans, line spans, and filters included.
    #[test]
    fn search_reports_round_trip_through_json_in_every_field() {
        let mut hits = 0;
        for (fixture, text) in [
            ("basic", "dashboard"),
            ("basic", "release"),
            ("dataview", "status"),
            ("tasknotes", "task"),
            ("mixed-properties", "project"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join(fixture);
            copy_fixture(fixture, &root);
            let paths = VaultPaths::new(&root);
            std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
            scan_vault(&paths, ScanMode::Full).unwrap();
            for explain in [false, true] {
                let query = SearchQuery {
                    text: text.to_string(),
                    explain,
                    fuzzy: true,
                    ..SearchQuery::default()
                };
                let report = run_search(&paths, &query, None).unwrap();
                hits += report.hits.len();
                let decoded: SearchReport =
                    serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
                assert_eq!(decoded, report, "{fixture} {text} explain={explain}");
            }
        }
        assert!(hits > 0, "the fixtures produce hits to compare");
    }
}
