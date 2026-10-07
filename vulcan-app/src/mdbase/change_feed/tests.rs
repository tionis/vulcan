use super::*;
use crate::mdbase::tests::{fixture, read_control_grant};
use std::fs;
use vulcan_core::permissions::{PathPermission, ResourceSpecifier};

#[test]
fn only_collection_changes_publish_after_the_cache_is_current() {
    let (directory, paths) = fixture();
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    // A watching host has scanned, so the cache database exists.
    drop(vulcan_core::CacheDatabase::open(&paths).unwrap());
    let feed = MdbaseChangeFeed::new();
    assert_eq!(
        feed.observe(&paths, &["notes/unrelated.txt".to_string()])
            .unwrap(),
        None
    );
    fs::write(
        directory.path().join("tasks/public.md"),
        "---\ntype: task\ntitle: Edited\n---\nBody\n",
    )
    .unwrap();
    assert_eq!(
        feed.observe(
            &paths,
            &["tasks/public.md".to_string(), "x.txt".to_string()]
        )
        .unwrap(),
        Some(1)
    );
    // The published state is already in the derived cache.
    let database = vulcan_core::CacheDatabase::open(&paths).unwrap();
    let revision: String = database
        .connection()
        .query_row(
            "SELECT revision FROM mdbase_record_cache WHERE path = 'tasks/public.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        revision,
        vulcan_core::mdbase::mdbase_content_revision("---\ntype: task\ntitle: Edited\n---\nBody\n")
    );
    assert_eq!(
        feed.observe(&paths, &["_types/task.md".to_string()])
            .unwrap(),
        Some(2)
    );
    let report = feed.changes_since(&paths, 0, None).unwrap();
    assert_eq!(report.generation, 2);
    assert!(!report.reconcile);
    assert_eq!(
        report.notifications,
        [
            MdbaseChangeNotification {
                generation: 1,
                controls_changed: false,
                paths: vec!["tasks/public.md".to_string()],
            },
            MdbaseChangeNotification {
                generation: 2,
                controls_changed: true,
                paths: vec!["_types/task.md".to_string()],
            },
        ]
    );
    assert!(feed
        .changes_since(&paths, 2, None)
        .unwrap()
        .notifications
        .is_empty());
}

#[test]
fn readers_see_only_visible_paths_and_reconcile_after_overflow() {
    let (_directory, paths) = fixture();
    let feed = MdbaseChangeFeed::new();
    feed.observe(&paths, &["tasks/private/secret.md".to_string()])
        .unwrap();
    feed.observe(
        &paths,
        &[
            "tasks/private/secret.md".to_string(),
            "tasks/public.md".to_string(),
        ],
    )
    .unwrap();
    let filter = PermissionFilter::new(PathPermission {
        allow: read_control_grant(),
        deny: vec![ResourceSpecifier::Folder("tasks/private/**".to_string())],
    });
    let report = feed.changes_since(&paths, 0, Some(&filter)).unwrap();
    assert_eq!(
        report.notifications,
        [MdbaseChangeNotification {
            generation: 2,
            controls_changed: false,
            paths: vec!["tasks/public.md".to_string()],
        }]
    );
    // Without control authority the feed is unavailable, like any read.
    let no_controls = PermissionFilter::new(PathPermission {
        allow: vec![ResourceSpecifier::Folder("tasks/**".to_string())],
        deny: Vec::new(),
    });
    assert_eq!(
        feed.changes_since(&paths, 0, Some(&no_controls))
            .unwrap_err()
            .code(),
        Some("permission_denied")
    );

    for _ in 0..MDBASE_CHANGE_FEED_CAPACITY {
        feed.observe(&paths, &["tasks/public.md".to_string()])
            .unwrap();
    }
    let behind = feed.changes_since(&paths, 1, None).unwrap();
    assert!(behind.reconcile);
    assert_eq!(behind.notifications.len(), MDBASE_CHANGE_FEED_CAPACITY);
    let current = feed.changes_since(&paths, behind.generation, None).unwrap();
    assert!(!current.reconcile && current.notifications.is_empty());
}

#[test]
fn invalid_controls_are_announced_and_vaults_without_collections_are_ignored() {
    let (directory, paths) = fixture();
    let feed = MdbaseChangeFeed::new();
    fs::write(
        directory.path().join("_types/task.md"),
        "---\nbroken: [\n---\n",
    )
    .unwrap();
    assert_eq!(
        feed.observe(&paths, &["_types/task.md".to_string()])
            .unwrap(),
        Some(1)
    );
    fs::remove_file(directory.path().join("mdbase.yaml")).unwrap();
    assert_eq!(
        feed.observe(&paths, &["tasks/public.md".to_string()])
            .unwrap(),
        None
    );
    assert_eq!(
        feed.observe(&paths, &["mdbase.yaml".to_string()]).unwrap(),
        Some(2)
    );
}

#[test]
fn changes_whose_refresh_failed_are_announced_with_the_next_observation() {
    let (directory, paths) = fixture();
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    fs::write(paths.cache_db(), "not a database").unwrap();
    let feed = MdbaseChangeFeed::new();
    assert!(feed
        .observe(&paths, &["tasks/public.md".to_string()])
        .is_err());
    assert_eq!(feed.changes_since(&paths, 0, None).unwrap().generation, 0);
    fs::remove_file(paths.cache_db()).unwrap();
    drop(vulcan_core::CacheDatabase::open(&paths).unwrap());
    let _ = directory;
    assert_eq!(
        feed.observe(&paths, &["unrelated.txt".to_string()])
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        feed.changes_since(&paths, 0, None).unwrap().notifications[0].paths,
        ["tasks/public.md"]
    );
}
