use super::*;
use crate::mdbase::tests::{fixture, read_control_grant};
use crate::mdbase::{
    create_mdbase_view_source, delete_mdbase_view_source, read_mdbase_view_source,
    update_mdbase_view_source, MdbaseViewSourceOptions,
};
use std::fs;
use vulcan_core::mdbase::MdbaseViewContextArg;
use vulcan_core::permissions::{PathPermission, ResourceSpecifier};

const VIEW_TYPE: &str = "---\nkind: mdbase.type\nname: view\nversion: 1\nmatch:\n  where:\n    type: view\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n---\n";

fn view_source(id: &str) -> String {
    format!(
        "---\ntype: view\nid: {id}\nversion: 1\nname: {id}\nquery:\n  types: [task]\nviews:\n  - id: titles\n    name: Titles\n    select: [title]\n    order_by:\n      - field: title\n  - id: context\n    name: Context\n    context:\n      this:\n        on_missing: error\n        types: [task]\n    select:\n      - name: context_title\n        expr: 'this.title'\n---\n"
    )
}

fn views_fixture() -> (tempfile::TempDir, VaultPaths) {
    let (directory, paths) = fixture();
    fs::write(directory.path().join("_types/view.md"), VIEW_TYPE).unwrap();
    fs::create_dir_all(directory.path().join("tasks/views")).unwrap();
    fs::write(
        directory.path().join("tasks/views/public.md"),
        view_source("public.views"),
    )
    .unwrap();
    fs::write(
        directory.path().join("tasks/private/views.md"),
        view_source("private.views"),
    )
    .unwrap();
    (directory, paths)
}

fn restricted() -> PermissionFilter {
    PermissionFilter::new(PathPermission {
        allow: read_control_grant(),
        deny: vec![ResourceSpecifier::Folder("tasks/private/**".to_string())],
    })
}

fn invocation(source: &str, view: &str, context: MdbaseViewContextArg) -> MdbaseViewInvocation {
    MdbaseViewInvocation {
        source: source.to_string(),
        view: view.to_string(),
        context,
        ..MdbaseViewInvocation::default()
    }
}

fn titles(report: &MdbaseQueryResult) -> Vec<String> {
    report
        .results
        .iter()
        .map(|row| {
            row.values.as_ref().unwrap()["title"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn listing_shows_only_visible_view_sources() {
    let (_directory, paths) = views_fixture();
    let sources = |filter: Option<&PermissionFilter>| {
        build_mdbase_view_list_report(&paths, filter)
            .unwrap()
            .views
            .into_iter()
            .map(|view| view.source.path)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        sources(None),
        ["tasks/private/views.md", "tasks/views/public.md"]
    );
    let filter = restricted();
    assert_eq!(sources(Some(&filter)), ["tasks/views/public.md"]);
    let list = build_mdbase_view_list_report(&paths, Some(&filter)).unwrap();
    assert_eq!(list.meta.total_count, 1);
    assert!(list.diagnostics.is_empty(), "{:?}", list.diagnostics);
}

#[test]
fn execution_scopes_sources_contexts_and_candidates_to_the_caller() {
    let (_directory, paths) = views_fixture();
    let filter = restricted();
    let run = |invocation: &MdbaseViewInvocation, filter: Option<&PermissionFilter>| {
        build_mdbase_view_report(&paths, invocation, filter)
    };
    let titles_view = invocation(
        "tasks/views/public.md",
        "titles",
        MdbaseViewContextArg::Absent,
    );
    assert_eq!(
        titles(&run(&titles_view, None).unwrap()),
        ["Public", "Secret"]
    );
    let scoped = run(&titles_view, Some(&filter)).unwrap();
    assert_eq!(titles(&scoped), ["Public"]);
    assert_eq!(
        scoped.meta.view.as_ref().map(|view| view.id.as_str()),
        Some("titles")
    );

    // A hidden source or context is indistinguishable from a missing one.
    let hidden_source = invocation("private.views", "titles", MdbaseViewContextArg::Absent);
    assert!(run(&hidden_source, None).is_ok());
    assert_eq!(
        run(&hidden_source, Some(&filter)).unwrap_err().code(),
        Some("view_not_found")
    );
    let hidden_context = invocation(
        "tasks/views/public.md",
        "context",
        MdbaseViewContextArg::Path("tasks/private/secret.md".to_string()),
    );
    let unrestricted = run(&hidden_context, None).unwrap();
    assert_eq!(
        unrestricted.results[0].values.as_ref().unwrap()["context_title"],
        "Secret"
    );
    assert_eq!(
        run(&hidden_context, Some(&filter)).unwrap_err().code(),
        Some("context_not_found")
    );
    let missing_context = invocation(
        "tasks/views/public.md",
        "context",
        MdbaseViewContextArg::Path("tasks/private/absent.md".to_string()),
    );
    assert_eq!(
        run(&missing_context, Some(&filter)).unwrap_err().code(),
        Some("context_not_found")
    );
    assert_eq!(
        run(
            &invocation(
                "tasks/views/public.md",
                "context",
                MdbaseViewContextArg::Absent
            ),
            Some(&filter)
        )
        .unwrap_err()
        .code(),
        Some("context_required")
    );
}

#[test]
fn cached_snapshots_serve_views_and_see_later_edits() {
    let (directory, paths) = views_fixture();
    let titles_view = invocation("public.views", "titles", MdbaseViewContextArg::Absent);
    let expected = build_mdbase_view_report(&paths, &titles_view, None).unwrap();
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    assert_eq!(
        build_mdbase_view_report(&paths, &titles_view, None).unwrap(),
        expected
    );
    fs::write(
        directory.path().join("tasks/public.md"),
        "---\ntype: task\ntitle: Edited\n---\nBody\n",
    )
    .unwrap();
    assert_eq!(
        titles(&build_mdbase_view_report(&paths, &titles_view, None).unwrap()),
        ["Edited", "Secret"]
    );
    // A view source edit is visible to the next listing and execution.
    fs::write(
        directory.path().join("tasks/views/public.md"),
        view_source("public.views").replace("name: Titles", "name: Renamed"),
    )
    .unwrap();
    let list = build_mdbase_view_list_report(&paths, None).unwrap();
    let public = list
        .views
        .iter()
        .find(|view| view.id == "public.views")
        .unwrap();
    assert_eq!(public.views[0].name, "Renamed");
}

fn source_options() -> MdbaseViewSourceOptions {
    MdbaseViewSourceOptions {
        no_commit: true,
        verbosity: Verbosity::Quiet,
        ..MdbaseViewSourceOptions::default()
    }
}

fn source_fixture() -> (tempfile::TempDir, VaultPaths) {
    let (directory, paths) = views_fixture();
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    (directory, paths)
}

#[test]
fn view_sources_read_exact_documents_and_reject_ordinary_records() {
    let (directory, paths) = source_fixture();
    let source = read_mdbase_view_source(&paths, "tasks/views/public.md", None).unwrap();
    let bytes = fs::read_to_string(directory.path().join("tasks/views/public.md")).unwrap();
    assert_eq!(source.document, bytes);
    assert_eq!(source.format, "mdbase.view");
    assert_eq!(
        source.revision,
        vulcan_core::mdbase::mdbase_content_revision(&bytes)
    );
    for path in ["tasks/public.md", "tasks/missing.md"] {
        assert_eq!(
            read_mdbase_view_source(&paths, path, None)
                .unwrap_err()
                .code(),
            Some("view_not_found")
        );
    }
    assert_eq!(
        read_mdbase_view_source(&paths, "tasks/private/views.md", Some(&restricted()))
            .unwrap_err()
            .code(),
        Some("view_not_found")
    );
}

#[test]
fn creating_view_sources_validates_the_whole_document_and_never_replaces() {
    let (directory, paths) = source_fixture();
    let options = source_options();
    let created =
        create_mdbase_view_source(&paths, None, &view_source("team:board"), &options).unwrap();
    assert_eq!(created.path, "views/team-board.md");
    assert_eq!(
        fs::read_to_string(directory.path().join("views/team-board.md")).unwrap(),
        view_source("team:board")
    );
    assert!(build_mdbase_view_list_report(&paths, None)
        .unwrap()
        .views
        .iter()
        .any(|view| view.source.path == "views/team-board.md"));
    let conflict =
        create_mdbase_view_source(&paths, None, &view_source("team:board"), &options).unwrap_err();
    assert_eq!(conflict.code(), Some("path_conflict"));
    let duplicate = view_source("dup").replace("id: context", "id: titles");
    let not_a_view = "---\ntype: task\ntitle: Nope\n---\n";
    for document in [duplicate.as_str(), not_a_view] {
        assert_eq!(
            create_mdbase_view_source(&paths, Some("views/bad.md"), document, &options)
                .unwrap_err()
                .code(),
            Some("invalid_view")
        );
        assert!(!directory.path().join("views/bad.md").exists());
    }
    let dry = MdbaseViewSourceOptions {
        dry_run: true,
        ..source_options()
    };
    create_mdbase_view_source(&paths, Some("views/dry.md"), &view_source("dry"), &dry).unwrap();
    assert!(!directory.path().join("views/dry.md").exists());
}

#[test]
fn updating_and_deleting_view_sources_honor_if_revision() {
    let (directory, paths) = source_fixture();
    let options = source_options();
    let path = "tasks/views/public.md";
    let current = read_mdbase_view_source(&paths, path, None).unwrap();
    let renamed = current.document.replace("name: Titles", "name: Renamed");
    let stale = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    assert_eq!(
        update_mdbase_view_source(&paths, path, &renamed, Some(stale), &options)
            .unwrap_err()
            .code(),
        Some("concurrent_modification")
    );
    assert_eq!(
        update_mdbase_view_source(
            &paths,
            path,
            &renamed.replace("id: context", "id: titles"),
            None,
            &options
        )
        .unwrap_err()
        .code(),
        Some("invalid_view")
    );
    let updated =
        update_mdbase_view_source(&paths, path, &renamed, Some(&current.revision), &options)
            .unwrap();
    assert_eq!(
        updated.revision,
        vulcan_core::mdbase::mdbase_content_revision(&renamed)
    );
    assert_eq!(
        fs::read_to_string(directory.path().join(path)).unwrap(),
        renamed
    );
    assert_eq!(
        delete_mdbase_view_source(&paths, path, Some(&current.revision), &options)
            .unwrap_err()
            .code(),
        Some("concurrent_modification")
    );
    let deleted =
        delete_mdbase_view_source(&paths, path, Some(&updated.revision), &options).unwrap();
    assert!(deleted.deleted);
    assert!(!directory.path().join(path).exists());
    assert!(build_mdbase_view_list_report(&paths, None)
        .unwrap()
        .views
        .iter()
        .all(|view| view.source.path != path));
}

#[test]
fn creating_without_write_authority_never_reveals_existing_paths() {
    let (directory, paths) = source_fixture();
    let config = directory.path().join(".vulcan/config.toml");
    let existing = fs::read_to_string(&config).unwrap_or_default();
    fs::write(
        &config,
        format!(
            "{existing}\n[permissions.profiles.views]\nread = \"all\"\nwrite = {{ allow = [\"folder:views/**\"] }}\nrefactor = \"none\"\ngit = \"deny\"\nnetwork = \"deny\"\nindex = \"deny\"\nconfig = \"read\"\nexecute = \"allow\"\nshell = \"deny\"\n"
        ),
    )
    .unwrap();
    let options = MdbaseViewSourceOptions {
        permission_profile: Some("views".to_string()),
        ..source_options()
    };
    // Existing and absent paths outside the grant fail the same way.
    for path in ["tasks/views/public.md", "tasks/views/absent.md"] {
        let error = create_mdbase_view_source(&paths, Some(path), &view_source("probe"), &options)
            .unwrap_err();
        assert_ne!(error.code(), Some("path_conflict"), "{path}: {error}");
        assert!(error.to_string().contains("permission"), "{path}: {error}");
    }
    create_mdbase_view_source(&paths, Some("views/ok.md"), &view_source("ok"), &options).unwrap();
}
