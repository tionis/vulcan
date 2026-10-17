//! The routed `tasks query` operation: a Tasks-plugin query over the
//! vault's tasks.
//!
//! A [`TasksQueryResult`] serializes every field, so the result itself is
//! the wire form. The `TaskNotes` auto-archive upkeep the CLI runs before a
//! task command may write, so it stays with the client.
//!
//! Building every task's row reads every task note, so a host keeps the
//! rows ([`RetainedTasks`]) for as long as the cache state and the vault
//! configuration they were built from are unchanged.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use vulcan_core::note_session::{NoteStoreSession, NoteStoreSnapshot};
use vulcan_core::plan::QueryPlanExplain;
use vulcan_core::tasks::{TaskRowIndex, TasksQueryResult};
use vulcan_core::{
    load_vault_config, PermissionGuard, ProfilePermissionGuard, VaultConfig, VaultPaths,
};

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

/// The task rows a host keeps for readers without a read restriction.
struct Retained {
    /// The cache state (`NoteStoreSnapshot::state_key`) they were built
    /// from, and the effective configuration: which notes are `TaskNotes`
    /// tasks, and so which rows exist, is configuration, which also merges
    /// Obsidian plugin settings the cache never sees.
    state: (String, i64),
    config: VaultConfig,
    rows: TaskRowIndex,
    plan: QueryPlanExplain,
}

/// A host's task rows, reused while the cache and configuration are
/// unchanged: any write to the cache, by this host or another process,
/// advances the store clock, and any change to the effective vault
/// configuration differs from the retained one, so the next request
/// rebuilds.
#[derive(Default)]
pub struct RetainedTasks {
    slot: Mutex<Option<Arc<Retained>>>,
}

impl RetainedTasks {
    /// Answer `request` through `session`, from the retained rows when the
    /// guard restricts no reads; a restricted reader loads its own.
    pub fn run(
        &self,
        session: &NoteStoreSession,
        paths: &VaultPaths,
        request: &TasksQueryRequest,
        guard: &ProfilePermissionGuard,
    ) -> Result<TasksQueryResult, AppError> {
        let unrestricted =
            guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook();
        let Some(snapshot) = session.snapshot().filter(|_| unrestricted) else {
            return run_tasks_query(Some(session), paths, request, guard);
        };
        let retained = self.current(&snapshot, paths)?;
        crate::tasks::build_tasks_query_result_from_rows(
            paths,
            &request.source,
            &retained.rows,
            &retained.plan,
            guard,
        )
    }

    /// The rows for the snapshot's cache state and the current
    /// configuration, rebuilding them on a change. Requests wait for one
    /// build rather than each building.
    fn current(
        &self,
        snapshot: &NoteStoreSnapshot<'_>,
        paths: &VaultPaths,
    ) -> Result<Arc<Retained>, AppError> {
        let state = snapshot.state_key();
        let config = load_vault_config(paths).config;
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(retained) = slot
            .as_ref()
            .filter(|retained| retained.state == state && retained.config == config)
        {
            return Ok(Arc::clone(retained));
        }
        let (rows, plan) = crate::tasks::load_task_rows_in(snapshot, paths)?;
        let retained = Arc::new(Retained {
            state,
            config,
            rows,
            plan,
        });
        *slot = Some(Arc::clone(&retained));
        Ok(retained)
    }
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

    /// Retained rows answer as the cache does across repeated requests,
    /// follow a change to the vault and to the effective configuration
    /// (`TaskNotes` settings come from an Obsidian plugin file the cache never
    /// sees), and leave restricted readers to load their own.
    #[test]
    fn retained_task_rows_answer_like_the_cache_and_follow_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        copy_fixture("tasknotes", &root);
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::write(
            root.join(".vulcan/config.toml"),
            "[permissions.profiles.projects]\nread = { allow = [\"folder:Projects/**\"] }\n",
        )
        .unwrap();
        let paths = VaultPaths::new(&root);
        scan_vault(&paths, ScanMode::Full).unwrap();
        let session = NoteStoreSession::new(paths.clone());
        let retained = RetainedTasks::default();
        let everyone =
            ProfilePermissionGuard::new(&paths, resolve_permission_profile(&paths, None).unwrap());
        let projects = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("projects")).unwrap(),
        );
        let answers = |guard: &ProfilePermissionGuard, source: &str| {
            let request = TasksQueryRequest {
                source: source.to_string(),
            };
            let mut direct = run_tasks_query(None, &paths, &request, guard).unwrap();
            let mut kept = retained.run(&session, &paths, &request, guard).unwrap();
            direct.note_plan = None;
            kept.note_plan = None;
            (kept, direct)
        };
        let sources = [
            "not done",
            "done",
            "not done\nsort by due\ngroup by path",
            "limit 1",
            "",
        ];
        let mut counts = Vec::new();
        for round in 0..3 {
            for source in sources {
                for _ in 0..2 {
                    let (kept, direct) = answers(&everyone, source);
                    assert_eq!(kept, direct, "round {round}: {source:?}");
                }
                let (kept, direct) = answers(&projects, source);
                assert_eq!(kept, direct, "round {round} restricted: {source:?}");
            }
            counts.push(answers(&everyone, "").0.result_count);
            match round {
                // A new task note, indexed.
                0 => {
                    std::fs::write(
                        root.join("TaskNotes/Tasks/New Task.md"),
                        "---\ntitle: New task\nstatus: open\ntags: [task]\n---\n",
                    )
                    .unwrap();
                    scan_vault(&paths, ScanMode::Incremental).unwrap();
                }
                // TaskNotes identifies tasks by another tag: the cache is
                // unchanged, but which notes are tasks is not.
                1 => {
                    let data = root.join(".obsidian/plugins/tasknotes/data.json");
                    let text = std::fs::read_to_string(&data).unwrap();
                    std::fs::write(
                        &data,
                        text.replace("\"taskTag\": \"task\"", "\"taskTag\": \"chore\""),
                    )
                    .unwrap();
                }
                _ => {}
            }
        }
        assert!(
            counts[1] > counts[0],
            "the retained rows saw the scan: {counts:?}"
        );
        assert!(
            counts[2] < counts[1],
            "and the configuration change: {counts:?}"
        );
    }
}
