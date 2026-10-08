use super::*;
use crate::note_store::DirectNoteStore;
use crate::permissions::{
    resolve_permission_profile, PathPermission, PermissionGuard, ProfilePermissionGuard,
    ResourceSpecifier,
};
use crate::{scan_vault, NoteQuery, ScanMode};
use std::collections::BTreeSet;
use std::fmt::Write as _;
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

/// A vault whose sort keys tie, mix kinds, and go missing.
fn ranked_vault() -> (TempDir, VaultPaths) {
    let temp_dir = TempDir::new().expect("temp dir should be created");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan")).unwrap();
    let titles = [
        "\"b\"", "\"a\"", "\"b\"", "3", "true", "null", "\"A\"", "\"é\"", "2.5", "\"\"",
    ];
    for index in 0..60_usize {
        let kind = ["task", "project", "contact"][index % 3];
        let status = ["open", "done"][index / 3 % 2];
        let title = titles[index * 7 % titles.len()];
        let title = if index % 11 == 0 {
            String::new()
        } else {
            format!("title: {title}\n")
        };
        let folder = ["x", "y/z", "w"][index % 4 % 3];
        fs::create_dir_all(root.join(folder)).unwrap();
        fs::write(
            root.join(format!("{folder}/n{:02}.md", 59 - index)),
            format!(
                "---\ntype: {kind}\nstatus: {status}\n{title}priority: {}\nname: n{:02}\n\
                 due: 2026-01-{:02}\ntags: [g{}]\n---\nbody\n",
                index % 5,
                index * 13 % 60,
                index % 28 + 1,
                index % 3,
            ),
        )
        .unwrap();
    }
    fs::write(root.join("x/data.csv"), "a,b\n").unwrap();
    let mut views = String::new();
    for (index, (sort, limit)) in [
        (
            "    sort:\n      - property: title\n        direction: ASC\n",
            5,
        ),
        (
            "    sort:\n      - property: title\n        direction: DESC\n",
            7,
        ),
        (
            "    sort:\n      - property: priority\n        direction: ASC\n",
            0,
        ),
        (
            "    sort:\n      - property: rank\n        direction: DESC\n",
            4,
        ),
        // By the first column, `file.name`: an expression, not a property.
        ("", 6),
        (
            "    sort:\n      - property: file.name\n        direction: DESC\n",
            3,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let order = if sort.is_empty() {
            "      - file.name\n      - title\n"
        } else {
            "      - title\n      - status\n"
        };
        write!(
            views,
            "  - type: table\n    name: v{index}\n    order:\n{order}{sort}    limit: {limit}\n"
        )
        .unwrap();
    }
    for (name, filters) in [
        (
            "open",
            "  and:\n    - 'type == \"task\"'\n    - 'status == \"open\"'\n",
        ),
        ("all", "  and: []\n"),
        // Not total.
        ("ranked", "  and:\n    - 'priority > 2'\n"),
    ] {
        fs::write(
            root.join(format!("{name}.base")),
            format!("filters:\n{filters}views:\n{views}"),
        )
        .unwrap();
    }
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).expect("scan should succeed");
    (temp_dir, paths)
}

/// Answers, the number of ordered walks, and the index stages they ran.
fn ranked_answers(
    store: &dyn NoteStore,
    paths: &VaultPaths,
    retained: bool,
) -> (Vec<serde_json::Value>, usize, BTreeSet<String>) {
    let mut stages = BTreeSet::new();
    let mut record_stages = |plan: Option<&crate::plan::QueryPlanExplain>| {
        for stage in plan.iter().flat_map(|plan| &plan.stages) {
            if stage.name.contains("index") {
                stages.insert(stage.name.clone());
            }
        }
    };
    let filters: [&[&str]; 7] = [
        &[],
        &["type = task"],
        &["type = task", "status = open"],
        &["status != done"],
        // Sources: a folder and a tag.
        &["file.path starts_with y/"],
        &["file.tags has_tag g1", "status = open"],
        // Not total: numbers meet date-like strings.
        &["priority > 2"],
    ];
    let mut answers = Vec::new();
    let mut walks = 0;
    for filters in filters {
        for sort_by in [None, Some("title"), Some("priority"), Some("file.name")] {
            for descending in [false, true] {
                for (offset, limit) in [(0, 0), (0, 1), (0, 7), (3, 5), (10, 100), (59, 3)] {
                    let query = NoteQuery {
                        filters: filters.iter().map(ToString::to_string).collect(),
                        sort_by: sort_by.map(ToString::to_string),
                        sort_descending: descending,
                    };
                    let report = crate::properties::query_notes_page_in(
                        store,
                        paths,
                        &query,
                        None,
                        Some(crate::properties::NotePage {
                            offset,
                            limit: Some(limit),
                        }),
                    )
                    .unwrap();
                    walks += usize::from(report.plan.as_ref().is_some_and(|plan| {
                        plan.candidate_path == "ordered walk over retained notes"
                    }));
                    record_stages(report.plan.as_ref());
                    let paths = report
                        .notes
                        .iter()
                        .map(|note| note.document_path.clone())
                        .collect::<Vec<_>>();
                    answers.push(serde_json::json!({
                        "query": [filters, sort_by, descending, offset, limit],
                        "notes": paths,
                    }));
                }
            }
        }
    }
    let guard =
        ProfilePermissionGuard::new(paths, resolve_permission_profile(paths, None).unwrap());
    walks += dql_answers(
        store,
        paths,
        &guard,
        retained,
        &mut answers,
        &mut record_stages,
    );
    walks += bases_answers(store, paths, &guard, &mut answers, &mut record_stages);
    (answers, walks, stages)
}

type RecordStages<'a> = dyn FnMut(Option<&crate::plan::QueryPlanExplain>) + 'a;

/// DQL answers for [`ranked_answers`]; the number of ordered walks.
fn dql_answers(
    store: &dyn NoteStore,
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    retained: bool,
    answers: &mut Vec<serde_json::Value>,
    record_stages: &mut RecordStages<'_>,
) -> usize {
    let mut walks = 0;
    for (dql, walk) in [
        (
            "TABLE title, status FROM \"x\" WHERE status = \"open\" SORT name ASC LIMIT 5",
            true,
        ),
        (
            "LIST FROM #g1 WHERE type = \"task\" SORT priority DESC LIMIT 3",
            true,
        ),
        ("TABLE name SORT name DESC LIMIT 4", true),
        ("TABLE title WHERE type = \"task\" LIMIT 6", true),
        ("TABLE title FROM \"y\" SORT rank LIMIT 3", true),
        ("TABLE title LIMIT 0", true),
        // Mixed kinds, dates, an undecided WHERE, two keys, more commands.
        ("TABLE title SORT title LIMIT 3", false),
        ("TABLE due SORT due DESC LIMIT 3", false),
        ("TABLE title WHERE priority > 2 SORT name LIMIT 3", false),
        ("TABLE title SORT status, name LIMIT 3", false),
        (
            "TABLE title WHERE type = \"task\" SORT name LIMIT 5 SORT title",
            false,
        ),
        ("TASK SORT name LIMIT 3", false),
    ] {
        let result = crate::dql::evaluate_dql_in(store, paths, dql, None, guard, true)
            .unwrap_or_else(|error| panic!("{dql}: {error}"));
        let walked = result
            .plan
            .as_ref()
            .is_some_and(|plan| plan.candidate_path == "ordered walk over retained notes");
        // Only the retained store can walk.
        assert_eq!(walked, retained && walk, "{dql}");
        walks += usize::from(walked);
        record_stages(result.plan.as_ref());
        let mut result = serde_json::to_value(&result).unwrap();
        result.as_object_mut().unwrap().remove("plan");
        answers.push(serde_json::json!({ "dql": dql, "walk": walk, "result": result }));
    }
    walks
}

/// Bases answers for [`ranked_answers`]; the number of ordered walks.
fn bases_answers(
    store: &dyn NoteStore,
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    answers: &mut Vec<serde_json::Value>,
    record_stages: &mut RecordStages<'_>,
) -> usize {
    let mut walks = 0;
    for base in ["open.base", "all.base", "ranked.base"] {
        let report = crate::bases::evaluate_base_file_in(store, paths, base, guard, true)
            .unwrap_or_else(|error| panic!("{base}: {error}"));
        for view in &report.views {
            walks += usize::from(
                view.plan
                    .as_ref()
                    .is_some_and(|plan| plan.candidate_path == "ordered walk over retained notes"),
            );
            record_stages(view.plan.as_ref());
            let mut view = serde_json::to_value(view).unwrap();
            view.as_object_mut().unwrap().remove("plan");
            answers.push(serde_json::json!({ "base": base, "view": view }));
        }
        assert!(
            report.diagnostics.is_empty(),
            "{base}: {:?}",
            report.diagnostics
        );
    }
    walks
}

#[test]
fn ordered_walks_answer_like_full_sorts() {
    let (_temp_dir, paths) = ranked_vault();
    let session = NoteStoreSession::new(paths.clone());
    let check = |round: &str| {
        let snapshot = session.snapshot().expect("no writer is active");
        let (retained, walks, stages) = ranked_answers(&snapshot, &paths, true);
        drop(snapshot);
        let (direct, direct_walks, _) =
            ranked_answers(&DirectNoteStore::new(&paths), &paths, false);
        assert_eq!(
            direct_walks, 0,
            "the direct store holds no records up front"
        );
        // Every total filter with a page walks: 6 filters x 4 orders x 2
        // directions x 6 pages, the views of the two total bases except the
        // one sorted by a `file.name` column (a sort key outside the columns
        // is a plain row value), and six DQL queries.
        assert_eq!(walks, 6 * 4 * 2 * 6 + 2 * 5 + 6, "{round}");
        for (retained, direct) in retained.iter().zip(&direct) {
            assert_eq!(retained, direct, "{round}");
        }
        stages
    };
    let built = check("first");
    assert!(built.contains("order index built") && built.contains("match index built"));
    assert!(
        !built.iter().any(|stage| stage.ends_with("updated")),
        "{built:?}"
    );
    let reused = check("cached indexes");
    assert!(
        reused.iter().all(|stage| stage.ends_with("reused")),
        "{reused:?}"
    );

    // Edits that keep every identity carry the indexes: a record moves in
    // the title order, changes tag, and stops matching a type.
    for (path, contents) in [
        (
            "x/n00.md",
            "---\ntype: task\nstatus: open\ntitle: \"0\"\npriority: 9\nname: n99\n---\nbody\n",
        ),
        (
            "y/z/n10.md",
            "---\ntype: contact\nstatus: open\ntitle: \"zz\"\npriority: 1\nname: n00\ntags: [g1]\n---\n",
        ),
        (
            "x/n04.md",
            "---\ntype: project\nstatus: done\ntitle: true\npriority: 0\nname: n50\ntags: [g2]\n---\n",
        ),
    ] {
        fs::write(paths.vault_root().join(path), contents).unwrap();
        scan_vault(&paths, ScanMode::Incremental).unwrap();
        let carried = check(&format!("after editing {path}"));
        assert!(
            carried.contains("order index updated") && carried.contains("match index updated"),
            "{path}: {carried:?}"
        );
        assert!(!carried.contains("order index built"), "{path}: {carried:?}");
    }

    // Indexes the snapshot between two edits never used still carry: they
    // update across both edits instead of building.
    for (round, title) in ["\"m1\"", "\"m2\""].into_iter().enumerate() {
        fs::write(
            paths.vault_root().join("w/n01.md"),
            format!("---\ntype: task\nstatus: open\ntitle: {title}\npriority: 3\nname: n02\n---\n"),
        )
        .unwrap();
        scan_vault(&paths, ScanMode::Incremental).unwrap();
        if round == 0 {
            let snapshot = session.snapshot().expect("no writer is active");
            crate::properties::query_notes_page_in(
                &snapshot,
                &paths,
                &NoteQuery {
                    filters: vec!["type = task".to_string()],
                    sort_by: Some("title".to_string()),
                    sort_descending: false,
                },
                None,
                Some(crate::properties::NotePage {
                    offset: 0,
                    limit: Some(3),
                }),
            )
            .unwrap();
        }
    }
    let carried = check("after an edit the previous snapshot hardly used");
    assert!(
        !carried.iter().any(|stage| stage.ends_with("built")),
        "{carried:?}"
    );

    // A new note changes identities: indexes build again.
    fs::write(
        paths.vault_root().join("w/new.md"),
        "---\ntype: task\nstatus: open\ntitle: \"a\"\npriority: 2\nname: n01\ntags: [g1]\n---\n",
    )
    .unwrap();
    scan_vault(&paths, ScanMode::Incremental).unwrap();
    let rebuilt = check("after creation");
    assert!(rebuilt.contains("order index built"), "{rebuilt:?}");
}
