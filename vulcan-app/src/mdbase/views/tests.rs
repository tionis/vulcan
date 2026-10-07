use super::*;
use crate::mdbase::tests::{fixture, read_control_grant};
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
