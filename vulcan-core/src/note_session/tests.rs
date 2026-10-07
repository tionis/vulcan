use super::*;
use crate::note_store::DirectNoteStore;
use crate::permissions::{
    resolve_permission_profile, PathPermission, PermissionGuard, ProfilePermissionGuard,
    ResourceSpecifier,
};
use crate::{scan_vault, NoteQuery, ScanMode};
use std::fs;
use std::sync::atomic::Ordering::Relaxed;
use tempfile::TempDir;

fn vault() -> (TempDir, VaultPaths) {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan")).unwrap();
    for (path, contents) in [
        (
            "A/One.md",
            "---\ntags: [t, visible]\nstatus: open\nup: '[[Two]]'\n---\n[[Two]]\n- [ ] task one\n",
        ),
        (
            "A/Three.md",
            "---\ntags: [visible]\nstatus: done\n---\n[[Deux]]\n- [x] done\n",
        ),
        (
            "B/Two.md",
            "---\ntags: [t, visible]\naliases: [Deux]\nstatus: open\n---\n[[One]]\n",
        ),
        ("Hidden.md", "---\ntags: [t]\nstatus: open\n---\n[[One]] [[Two]]\n"),
        (
            "view.base",
            "filters:\n  and:\n    - 'status == \"open\"'\nformulas:\n  up_status: 'up.status'\nviews:\n  - type: table\n    name: open\n    order:\n      - file.name\n      - formula.up_status\n  - type: table\n    name: tagged\n    order:\n      - file.name\n      - file.tags\n      - file.inlinks\n",
        ),
    ] {
        let target = root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, contents).unwrap();
    }
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
    (temp_dir, paths)
}

fn guards(paths: &VaultPaths) -> Vec<ProfilePermissionGuard> {
    let unrestricted =
        ProfilePermissionGuard::new(paths, resolve_permission_profile(paths, None).unwrap());
    let mut profile = resolve_permission_profile(paths, None).unwrap();
    profile.grant.read = PathPermission {
        allow: vec![
            ResourceSpecifier::Tag("visible".into()),
            ResourceSpecifier::Note("view.base".into()),
        ],
        deny: Vec::new(),
    };
    vec![unrestricted, ProfilePermissionGuard::new(paths, profile)]
}

const DQL: &[&str] = &[
    "LIST FROM \"A\" WHERE status = \"open\"",
    "TABLE status, up.status AS up FROM #t SORT file.name",
    "TABLE file.tags AS tags, file.inlinks AS inlinks, length(file.tasks) AS tasks",
    "TASK WHERE !completed",
];

/// Every frontend's answer through `store`, as JSON.
fn answers(
    store: &dyn NoteStore,
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Vec<serde_json::Value> {
    let mut answers = DQL
        .iter()
        .map(|source| {
            let result =
                crate::dql::evaluate_dql_in(store, paths, source, Some("A/One.md"), guard, false)
                    .unwrap_or_else(|error| panic!("{source}: {error}"));
            serde_json::to_value(result).unwrap()
        })
        .collect::<Vec<_>>();
    let filter = guard.read_filter();
    let query = NoteQuery {
        filters: vec!["status = open".to_string()],
        sort_by: Some("file.name".to_string()),
        sort_descending: false,
    };
    let notes = crate::properties::query_notes_in(store, paths, &query, Some(&filter)).unwrap();
    answers.push(serde_json::to_value(notes.notes).unwrap());
    let ast =
        crate::QueryAst::from_dsl("from notes where status = open order by file.path").unwrap();
    let report = crate::query::execute_query_report_in(store, paths, ast, Some(&filter)).unwrap();
    answers.push(serde_json::to_value(report.notes).unwrap());
    let bases =
        crate::bases::evaluate_base_file_in(store, paths, "view.base", guard, false).unwrap();
    answers.push(serde_json::to_value(bases).unwrap());
    answers
}

fn assert_session_equals_direct(session: &NoteStoreSession, paths: &VaultPaths, round: &str) {
    for (scope, guard) in guards(paths).iter().enumerate() {
        let snapshot = session.snapshot().expect("no writer is active");
        let retained = answers(&snapshot, paths, guard);
        drop(snapshot);
        let direct = answers(&DirectNoteStore::new(paths), paths, guard);
        for (index, (retained, direct)) in retained.iter().zip(&direct).enumerate() {
            assert_eq!(retained, direct, "{round}: scope {scope}, answer {index}");
        }
    }
}

fn counter(counter: &AtomicU64) -> u64 {
    counter.load(Relaxed)
}

#[test]
fn snapshots_answer_like_the_direct_store_and_reuse_unchanged_notes() {
    fn shareable<T: Send + Sync>() {}
    shareable::<NoteStoreSession>();

    let (_temp_dir, paths) = vault();
    let session = NoteStoreSession::new(paths.clone());
    assert_session_equals_direct(&session, &paths, "first");
    let counters = session.counters();
    let (loaded, hydrated) = (
        counter(&counters.stored_loaded),
        counter(&counters.hydrated_loaded),
    );
    assert!(loaded > 0 && hydrated > 0);

    // Nothing changed: identities, stored rows, and file objects are reused.
    assert_session_equals_direct(&session, &paths, "repeat");
    assert_eq!(counter(&counters.stored_loaded), loaded);
    assert_eq!(counter(&counters.hydrated_loaded), hydrated);
    assert!(counter(&counters.identity_reuses) > 0);
    // Pooled connections serve later requests.
    assert!(counter(&counters.snapshots) > counter(&counters.connections_opened));

    // An edit loads only the changed row; file objects reload at the new
    // clock because incoming links depend on other rows.
    let reused = counter(&counters.stored_reused);
    fs::write(
        paths.vault_root().join("B/Two.md"),
        "---\ntags: [t, visible]\naliases: [Deux]\nstatus: done\n---\n[[Three]]\n",
    )
    .unwrap();
    scan_vault(&paths, ScanMode::Incremental).unwrap();
    assert_session_equals_direct(&session, &paths, "after edit");
    assert!(counter(&counters.stored_reused) > reused);
    assert!(counter(&counters.hydrated_loaded) > hydrated);
    // Scopes refresh from the rows that changed rather than reloading, and
    // hydrated notes the edit cannot reach carry over.
    let refreshes = counter(&counters.identity_refreshes);
    assert!(refreshes > 0);
    assert!(counter(&counters.hydrated_carried) > 0);

    // A rename keeps the document; a note leaving the restricted scope and
    // a deletion make the scope's count disagree, so it reloads whole.
    fs::rename(
        paths.vault_root().join("A/Three.md"),
        paths.vault_root().join("A/Four.md"),
    )
    .unwrap();
    scan_vault(&paths, ScanMode::Incremental).unwrap();
    assert_session_equals_direct(&session, &paths, "after rename");
    assert!(counter(&counters.identity_refreshes) > refreshes);
    fs::write(
        paths.vault_root().join("A/One.md"),
        "---\ntags: [t]\nstatus: open\nup: '[[Two]]'\n---\n[[Two]]\n- [ ] task one\n",
    )
    .unwrap();
    scan_vault(&paths, ScanMode::Incremental).unwrap();
    assert_session_equals_direct(&session, &paths, "after leaving the scope");
    fs::remove_file(paths.vault_root().join("Hidden.md")).unwrap();
    scan_vault(&paths, ScanMode::Incremental).unwrap();
    assert_session_equals_direct(&session, &paths, "after deletion");

    // Bookmarks change starred records without a cache write.
    fs::create_dir_all(paths.vault_root().join(".obsidian")).unwrap();
    fs::write(
        paths.vault_root().join(".obsidian/bookmarks.json"),
        r#"{"items":[{"type":"file","path":"A/One.md"}]}"#,
    )
    .unwrap();
    assert_session_equals_direct(&session, &paths, "after bookmarks");

    // A recreated cache has a new store id; nothing retained matches it.
    fs::remove_file(paths.cache_db()).unwrap();
    scan_vault(&paths, ScanMode::Full).unwrap();
    assert_session_equals_direct(&session, &paths, "after recreation");
}

/// The status of `B/Two.md` as a snapshot sees it.
fn two_status(store: &dyn NoteStore, paths: &VaultPaths) -> serde_json::Value {
    let guard = &guards(paths)[0];
    let result =
        crate::dql::evaluate_dql_in(store, paths, "TABLE status FROM \"B\"", None, guard, false)
            .unwrap();
    result.rows[0]["status"].clone()
}

#[test]
fn snapshots_never_wait_for_writers_and_see_whole_commits() {
    let (_temp_dir, paths) = vault();
    let session = NoteStoreSession::new(paths.clone());
    let before = session.snapshot().expect("no writer is active");
    assert_eq!(two_status(&before, &paths), "open");

    // A writer holds the lock and commits its scan: readers neither wait
    // nor see anything but whole commits.
    let lock = crate::write_lock::acquire_write_lock(&paths).unwrap();
    fs::write(
        paths.vault_root().join("B/Two.md"),
        "---\naliases: [Deux]\nstatus: closed\n---\n",
    )
    .unwrap();
    crate::scan::scan_vault_unlocked(&paths, ScanMode::Incremental).unwrap();
    let after = session.snapshot().expect("readers do not wait for writers");
    assert_eq!(two_status(&after, &paths), "closed");
    assert!(after.clock_version() > before.clock_version());
    // A snapshot keeps reading the state it began with.
    assert_eq!(two_status(&before, &paths), "open");
    drop(lock);
    drop((before, after));

    // An interrupted ordinary write needs recovery: the direct path reports
    // it, so no snapshot is handed out while no writer holds the lock.
    let change = crate::ordinary_write::OrdinaryWriteChange {
        path: "C.md".to_string(),
        before: None,
        after: Some("new\n".to_string()),
    };
    let _ = crate::ordinary_write::apply_with_hook(&paths, &[change], |_| {
        Err(crate::ordinary_write::OrdinaryWriteError::new(
            "test_interruption",
            "stop after publishing the journal",
            None,
        ))
    });
    assert!(session.snapshot().is_none());
    assert_eq!(counter(&session.counters().snapshots_unavailable), 1);
    crate::ordinary_write::recover_ordinary_write_batch(&paths).unwrap();
    assert!(session.snapshot().is_some());
}

#[test]
fn concurrent_readers_see_only_completed_writes() {
    let (_temp_dir, paths) = vault();
    let session = NoteStoreSession::new(paths.clone());
    let open = |status: &str| {
        format!("---\ntags: [t, visible]\naliases: [Deux]\nstatus: {status}\n---\n[[One]]\n")
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let readers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let guard = &guards(&paths)[0];
                    let filter = guard.read_filter();
                    let query = NoteQuery {
                        filters: vec!["status = open".to_string()],
                        sort_by: None,
                        sort_descending: false,
                    };
                    let mut served = 0;
                    while !stop.load(Relaxed) || served < 5 {
                        let Some(snapshot) = session.snapshot() else {
                            continue;
                        };
                        let report = crate::properties::query_notes_in(
                            &snapshot,
                            &paths,
                            &query,
                            Some(&filter),
                        )
                        .unwrap();
                        // `B/Two.md` toggles; the other open notes never do.
                        let count = report.notes.len();
                        assert!(count == 2 || count == 3, "{count} open notes");
                        served += 1;
                    }
                    served
                })
            })
            .collect::<Vec<_>>();
        for round in 0..10 {
            let status = if round % 2 == 0 { "closed" } else { "open" };
            fs::write(paths.vault_root().join("B/Two.md"), open(status)).unwrap();
            scan_vault(&paths, ScanMode::Incremental).unwrap();
        }
        stop.store(true, Relaxed);
        for reader in readers {
            assert!(reader.join().unwrap() > 0);
        }
    });
    assert_session_equals_direct(&session, &paths, "after racing writes");
}

#[test]
fn the_clock_advances_with_every_note_visible_commit() {
    let (_temp_dir, paths) = vault();
    fs::write(paths.vault_root().join("Charlie.md"), "# Charlie\n").unwrap();
    fs::write(
        paths.vault_root().join("D.md"),
        "# D\n\nCharlie is mentioned here.\n",
    )
    .unwrap();
    scan_vault(&paths, ScanMode::Incremental).unwrap();
    let session = NoteStoreSession::new(paths.clone());
    let version = || session.snapshot().expect("snapshot").clock_version();
    let start = version();
    scan_vault(&paths, ScanMode::Incremental).unwrap();
    assert_eq!(
        version(),
        start,
        "a scan that changes nothing keeps the clock"
    );
    assert_session_equals_direct(&session, &paths, "warm");

    // An accepted suggestion adds an incoming link without changing any
    // note's row; the clock still names the new state.
    let report = crate::suggestions::suggest_links(&paths, None, None, 0.0, None).unwrap();
    let suggestion = report
        .suggestions
        .iter()
        .find(|suggestion| {
            suggestion.source_path == "D.md" && suggestion.target_path == "Charlie.md"
        })
        .expect("D mentions Charlie");
    crate::suggestions::accept_link_suggestion(&paths, &suggestion.id).unwrap();
    assert!(version() > start);
    assert_session_equals_direct(&session, &paths, "after an accepted suggestion");
}
