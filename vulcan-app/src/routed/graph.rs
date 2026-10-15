//! The routed whole-graph reports: `graph stats`, `graph hubs`, and
//! `graph path`.
//!
//! These read every note and every resolved link, so a host keeps the
//! loaded graph ([`RetainedGraph`]) for as long as the cache state it was
//! loaded from is unchanged, and answers from it. The reports serialize
//! every field, so they are their own wire form.

use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use vulcan_core::note_session::{NoteStoreSession, NoteStoreSnapshot};
use vulcan_core::{
    query_graph_analytics_with_filter, query_graph_hubs_with_filter, query_graph_path_with_filter,
    GraphAnalyticsReport, GraphHubsReport, GraphPathReport, LoadedGraph, PermissionFilter,
    VaultPaths,
};

use crate::AppError;

/// A whole-graph report request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "report", rename_all = "snake_case")]
pub enum GraphRequest {
    Stats,
    Hubs,
    Path { from: String, to: String },
}

/// A whole-graph report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "report", content = "value", rename_all = "snake_case")]
pub enum GraphAnswer {
    Stats(GraphAnalyticsReport),
    Hubs(GraphHubsReport),
    Path(GraphPathReport),
}

/// Answer `request` from the cache directly.
pub fn run_graph(
    paths: &VaultPaths,
    request: &GraphRequest,
    filter: Option<&PermissionFilter>,
) -> Result<GraphAnswer, AppError> {
    match request {
        GraphRequest::Stats => {
            query_graph_analytics_with_filter(paths, filter).map(GraphAnswer::Stats)
        }
        GraphRequest::Hubs => query_graph_hubs_with_filter(paths, filter).map(GraphAnswer::Hubs),
        GraphRequest::Path { from, to } => {
            query_graph_path_with_filter(paths, from, to, filter).map(GraphAnswer::Path)
        }
    }
    .map_err(AppError::operation)
}

/// The graph a host keeps for readers without a path restriction, with the
/// argument-free reports computed from it.
struct Retained {
    /// The cache state it was loaded from (`NoteStoreSnapshot::state_key`).
    key: (String, i64),
    graph: LoadedGraph,
    stats: OnceLock<GraphAnalyticsReport>,
    hubs: OnceLock<GraphHubsReport>,
}

/// A host's loaded graph, reused while the cache is unchanged: any write to
/// the cache, by this host or another process, advances the store clock and
/// the next request loads the graph again.
#[derive(Default)]
pub struct RetainedGraph {
    slot: Mutex<Option<Arc<Retained>>>,
}

impl RetainedGraph {
    /// Answer `request` through `session`, from the retained graph when no
    /// path restriction applies.
    pub fn run(
        &self,
        session: &NoteStoreSession,
        paths: &VaultPaths,
        request: &GraphRequest,
        filter: Option<&PermissionFilter>,
    ) -> Result<GraphAnswer, AppError> {
        let Some(snapshot) = session.snapshot() else {
            return run_graph(paths, request, filter);
        };
        if filter.is_some() {
            let graph =
                LoadedGraph::load(snapshot.connection(), filter).map_err(AppError::operation)?;
            return answer(&snapshot, &graph, request);
        }
        let retained = self.current(&snapshot)?;
        match request {
            GraphRequest::Stats => {
                if let Some(report) = retained.stats.get() {
                    return Ok(GraphAnswer::Stats(report.clone()));
                }
                let report = retained
                    .graph
                    .analytics_report(snapshot.connection())
                    .map_err(AppError::operation)?;
                let _ = retained.stats.set(report.clone());
                Ok(GraphAnswer::Stats(report))
            }
            GraphRequest::Hubs => Ok(GraphAnswer::Hubs(
                retained
                    .hubs
                    .get_or_init(|| retained.graph.hubs_report())
                    .clone(),
            )),
            GraphRequest::Path { .. } => answer(&snapshot, &retained.graph, request),
        }
    }

    /// The graph for the snapshot's cache state, loading it on a change.
    /// Requests wait for one load rather than each loading.
    fn current(&self, snapshot: &NoteStoreSnapshot<'_>) -> Result<Arc<Retained>, AppError> {
        let key = snapshot.state_key();
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(retained) = slot.as_ref().filter(|retained| retained.key == key) {
            return Ok(Arc::clone(retained));
        }
        let graph = LoadedGraph::load(snapshot.connection(), None).map_err(AppError::operation)?;
        let retained = Arc::new(Retained {
            key,
            graph,
            stats: OnceLock::new(),
            hubs: OnceLock::new(),
        });
        *slot = Some(Arc::clone(&retained));
        Ok(retained)
    }
}

fn answer(
    snapshot: &NoteStoreSnapshot<'_>,
    graph: &LoadedGraph,
    request: &GraphRequest,
) -> Result<GraphAnswer, AppError> {
    match request {
        GraphRequest::Stats => graph
            .analytics_report(snapshot.connection())
            .map(GraphAnswer::Stats),
        GraphRequest::Hubs => Ok(GraphAnswer::Hubs(graph.hubs_report())),
        GraphRequest::Path { from, to } => graph.path_report(from, to).map(GraphAnswer::Path),
    }
    .map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routed::test_support::copy_fixture;
    use vulcan_core::{
        resolve_permission_profile, scan_vault, PermissionGuard, ProfilePermissionGuard, ScanMode,
    };

    fn requests() -> Vec<GraphRequest> {
        vec![
            GraphRequest::Stats,
            GraphRequest::Hubs,
            GraphRequest::Path {
                from: "Home".to_string(),
                to: "Projects/Alpha".to_string(),
            },
            GraphRequest::Path {
                from: "Home".to_string(),
                to: "Missing".to_string(),
            },
        ]
    }

    fn same(
        retained: Result<GraphAnswer, AppError>,
        direct: Result<GraphAnswer, AppError>,
        label: &str,
    ) {
        match (retained, direct) {
            (Ok(retained), Ok(direct)) => {
                let decoded: GraphAnswer =
                    serde_json::from_slice(&serde_json::to_vec(&retained).unwrap()).unwrap();
                assert_eq!(decoded, retained, "{label}");
                assert_eq!(retained, direct, "{label}");
            }
            (Err(retained), Err(direct)) => {
                assert_eq!(retained.to_string(), direct.to_string(), "{label}");
            }
            (retained, direct) => panic!("{label}: {retained:?} vs {direct:?}"),
        }
    }

    /// The retained graph answers as the cache does, follows a change to the
    /// vault, and leaves restricted readers to a graph of their own.
    #[test]
    fn retained_graph_answers_like_the_cache_and_follows_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_fixture("basic", &root);
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::write(
            root.join(".vulcan/config.toml"),
            "[permissions.profiles.projects]\nread = { allow = [\"folder:Projects/**\", \"note:Home.md\"] }\n",
        )
        .unwrap();
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).unwrap();
        let session = NoteStoreSession::new(paths.clone());
        let retained = RetainedGraph::default();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("projects")).unwrap(),
        );
        let restricted = guard.read_filter();
        for round in 0..2 {
            for request in requests() {
                for _ in 0..2 {
                    same(
                        retained.run(&session, &paths, &request, None),
                        run_graph(&paths, &request, None),
                        &format!("round {round}: {request:?}"),
                    );
                }
                same(
                    retained.run(&session, &paths, &request, Some(&restricted)),
                    run_graph(&paths, &request, Some(&restricted)),
                    &format!("round {round} restricted: {request:?}"),
                );
            }
            // A new note linking into the graph changes every report.
            std::fs::write(
                root.join("Bridge.md"),
                "[[Home]] [[Projects/Alpha]] #bridge\n",
            )
            .unwrap();
            scan_vault(&paths, ScanMode::Incremental).unwrap();
        }
        let GraphAnswer::Stats(stats) = retained
            .run(&session, &paths, &GraphRequest::Stats, None)
            .unwrap()
        else {
            panic!("stats");
        };
        assert_eq!(stats.note_count, 4, "the retained graph saw both scans");
    }
}
