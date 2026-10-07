use crate::mdbase::{
    build_mdbase_view_list_report, build_mdbase_view_report, create_mdbase_view_source,
    delete_mdbase_view_source, read_mdbase_view_source, update_mdbase_view_source,
    MdbaseViewSourceOptions,
};
use std::fs;
use vulcan_core::mdbase::{MdbaseViewContextArg, MdbaseViewInvocation};
use vulcan_core::permissions::{PathPermission, ResourceSpecifier};
use vulcan_core::{scan_vault, PermissionFilter, ScanMode, VaultPaths};

const BASE: &str = "filters:\n  and:\n    - 'file.ext == \"md\"'\nproperties:\n  status:\n    displayName: Status\nviews:\n  - name: Open Work\n    type: table\n    filters:\n      - 'status == \"open\"'\n    order:\n      - file.name\n      - status\n  - name: Open Work\n    type: board\n    order:\n      - file.name\n    groupBy:\n      property: status\n      direction: ASC\n";

fn fixture() -> (tempfile::TempDir, VaultPaths) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (path, contents) in [
        (
            "mdbase.yaml",
            "spec_version: \"0.3.0\"\nx-obsidian:\n  bases:\n    include: ['Views/**/*.base']\n",
        ),
        ("a.md", "---\nstatus: open\n---\n"),
        ("b.md", "---\nstatus: done\n---\n"),
        ("Private/c.md", "---\nstatus: open\n---\n"),
        ("Views/work.base", BASE),
        ("Views/broken.base", "views: [\n"),
        ("Private/Views/hidden.base", BASE),
        ("Other/ignored.base", BASE),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    let paths = VaultPaths::new(root);
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    scan_vault(&paths, ScanMode::Full).unwrap();
    (directory, paths)
}

fn invoke(view: &str) -> MdbaseViewInvocation {
    MdbaseViewInvocation {
        source: "Views/work.base".to_string(),
        view: view.to_string(),
        ..MdbaseViewInvocation::default()
    }
}

fn options() -> MdbaseViewSourceOptions {
    MdbaseViewSourceOptions {
        no_commit: true,
        quiet: true,
        ..MdbaseViewSourceOptions::default()
    }
}

#[test]
fn included_bases_are_listed_with_derived_ids_properties_and_revisions() {
    let (_directory, paths) = fixture();
    let list = build_mdbase_view_list_report(&paths, None).unwrap();
    let sources = list
        .views
        .iter()
        .map(|source| source.source.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(sources, ["Views/work.base"]);
    let base = &list.views[0];
    assert_eq!(base.source.format, "obsidian.base");
    assert_eq!(
        base.source.revision,
        vulcan_core::mdbase::mdbase_content_revision(BASE)
    );
    assert_eq!(
        serde_json::to_value(&base.views).unwrap(),
        serde_json::json!([
            {"id": "open-work", "name": "Open Work",
             "properties": [{"key": "file.name"}, {"key": "status", "label": "Status"}],
             "presentation": {"type": "table"}},
            {"id": "open-work-2", "name": "Open Work",
             "properties": [{"key": "file.name"}],
             "presentation": {"type": "board"}},
        ])
    );
    assert!(list
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "invalid_view"
            && diagnostic.path.as_deref() == Some("Views/broken.base")));
}

#[test]
fn base_views_execute_headlessly_with_bases_semantics() {
    let (_directory, paths) = fixture();
    let open = build_mdbase_view_report(&paths, &invoke("open-work"), None).unwrap();
    assert_eq!(
        open.results
            .iter()
            .map(|row| row.file["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        // Bases order: by the first ordered property, `file.name`.
        ["a.md", "Private/c.md"]
    );
    assert_eq!(
        open.results[0].values,
        Some(serde_json::json!({"file.name": "a", "status": "open"}))
    );
    assert_eq!(open.meta.view.as_ref().unwrap().id, "open-work");

    let board = build_mdbase_view_report(
        &paths,
        &MdbaseViewInvocation {
            limit: Some(1),
            ..invoke("open-work-2")
        },
        None,
    )
    .unwrap();
    assert_eq!(board.results.len(), 1);
    assert_eq!(board.meta.total_count, 3);
    assert!(board.meta.has_more);
    let groups = board.meta.groups.unwrap();
    assert_eq!(groups.iter().map(|group| group.count).sum::<usize>(), 3);

    // Restricted readers see neither hidden rows nor hidden sources.
    let filter = PermissionFilter::new(PathPermission {
        allow: vec![ResourceSpecifier::Folder("**".to_string())],
        deny: vec![ResourceSpecifier::Folder("Private/**".to_string())],
    });
    let scoped = build_mdbase_view_report(&paths, &invoke("open-work"), Some(&filter)).unwrap();
    assert_eq!(scoped.results.len(), 1);

    for (invocation, code) in [
        (invoke("missing"), "view_not_found"),
        (
            MdbaseViewInvocation {
                source: "Other/ignored.base".to_string(),
                ..invoke("open-work")
            },
            "view_not_found",
        ),
        (
            MdbaseViewInvocation {
                context: MdbaseViewContextArg::Path("a.md".to_string()),
                ..invoke("open-work")
            },
            "unsupported_context",
        ),
        (
            MdbaseViewInvocation {
                render: true,
                ..invoke("open-work")
            },
            "unsupported_presentation",
        ),
    ] {
        assert_eq!(
            build_mdbase_view_report(&paths, &invocation, None)
                .unwrap_err()
                .code(),
            Some(code)
        );
    }
}

#[test]
fn base_sources_round_trip_complete_documents_under_if_revision() {
    let (directory, paths) = fixture();
    let read = read_mdbase_view_source(&paths, "Views/work.base", None).unwrap();
    assert_eq!(read.document, BASE);
    assert_eq!(read.format, "obsidian.base");
    assert_eq!(
        read_mdbase_view_source(&paths, "Other/ignored.base", None)
            .unwrap_err()
            .code(),
        Some("view_not_found")
    );

    // Unknown keys survive because the document is stored as given.
    let extended = format!("{BASE}x-plugin:\n  kept: true\n");
    let stale = "sha256:stale";
    assert_eq!(
        update_mdbase_view_source(
            &paths,
            "Views/work.base",
            &extended,
            Some(stale),
            &options()
        )
        .unwrap_err()
        .code(),
        Some("concurrent_modification")
    );
    assert_eq!(
        update_mdbase_view_source(&paths, "Views/work.base", "views: [\n", None, &options())
            .unwrap_err()
            .code(),
        Some("invalid_view")
    );
    let updated = update_mdbase_view_source(
        &paths,
        "Views/work.base",
        &extended,
        Some(&read.revision),
        &options(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(directory.path().join("Views/work.base")).unwrap(),
        extended
    );

    assert_eq!(
        create_mdbase_view_source(&paths, Some("Views/work.base"), BASE, &options())
            .unwrap_err()
            .code(),
        Some("path_conflict")
    );
    assert_eq!(
        create_mdbase_view_source(&paths, Some("Other/new.base"), BASE, &options())
            .unwrap_err()
            .code(),
        Some("invalid_view")
    );
    create_mdbase_view_source(&paths, Some("Views/new/new.base"), BASE, &options()).unwrap();
    assert!(build_mdbase_view_list_report(&paths, None)
        .unwrap()
        .views
        .iter()
        .any(|source| source.source.path == "Views/new/new.base"));

    assert_eq!(
        delete_mdbase_view_source(&paths, "Views/work.base", Some(&read.revision), &options())
            .unwrap_err()
            .code(),
        Some("concurrent_modification")
    );
    delete_mdbase_view_source(
        &paths,
        "Views/work.base",
        Some(&updated.revision),
        &options(),
    )
    .unwrap();
    assert!(!directory.path().join("Views/work.base").exists());
}
