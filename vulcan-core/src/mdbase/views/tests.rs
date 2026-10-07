use super::*;
use crate::mdbase::{
    compile_mdbase_prepared_query, load_mdbase_collection, load_mdbase_contract_registry,
    load_mdbase_records_with_contracts, load_mdbase_type_registry,
};
use serde_json::json;
use std::fs;
use std::path::Path;

const VIEW_TYPE: &str = "---\nkind: mdbase.type\nname: view\nversion: 1\nmatch:\n  where:\n    type: view\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n---\n";
const TASK_TYPE: &str = "---\nkind: mdbase.type\nname: task\nversion: 1\nmatch:\n  where:\n    type: task\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    properties:\n      title: { type: string }\n      status: { type: string }\n      priority: { type: integer }\n---\n";
const PROJECT_TYPE: &str = "---\nkind: mdbase.type\nname: project\nversion: 1\nmatch:\n  where:\n    type: project\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n---\n";

const TASK_VIEWS: &str = r#"---
type: view
id: task.views
version: 1
name: Task views
properties:
  title:
    label: Task
    format: text
  projection.urgency:
    label: Urgency
    hidden: true
summary_functions:
  joined:
    expr: 'values.join(",")'
query:
  types: [task]
  where: 'status != "archived"'
  context:
    this:
      types: [project]
  projections:
    urgency:
      expr: 'priority * 2'
views:
  - id: open
    name: Open
    where: 'status == "open"'
    projections:
      urgency:
        expr: 'priority * 2'
    select:
      - title
      - projection.urgency
      - name: shout
        expr: 'title + "!"'
        label: Loud
    order_by:
      - field: priority
        direction: desc
    limit: 5
    presentation:
      type: example.list
  - id: everything
    name: Everything
    types: [task, project]
    context:
      this:
        on_missing: "null"
  - id: required
    name: Required
    context:
      this:
        on_missing: error
---
"#;

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn collection(extra: &[(&str, &str)]) -> (tempfile::TempDir, MdbaseRecordSet) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mdbase.yaml", "spec_version: \"0.3.0\"\n");
    write(root, "_types/view.md", VIEW_TYPE);
    write(root, "_types/task.md", TASK_TYPE);
    write(root, "_types/project.md", PROJECT_TYPE);
    write(root, "views/tasks.md", TASK_VIEWS);
    write(
        root,
        "projects/alpha.md",
        "---\ntype: project\nid: alpha\ntitle: Alpha\n---\n",
    );
    for (path, title, status, priority) in [
        ("tasks/a.md", "A", "open", 1),
        ("tasks/b.md", "B", "open", 3),
        ("tasks/c.md", "C", "done", 2),
        ("tasks/d.md", "D", "archived", 4),
    ] {
        write(
            root,
            path,
            &format!(
                "---\ntype: task\ntitle: {title}\nstatus: {status}\npriority: {priority}\n---\n"
            ),
        );
    }
    for (path, contents) in extra {
        write(root, path, contents);
    }
    let records = load_records(root);
    (directory, records)
}

fn load_records(root: &Path) -> MdbaseRecordSet {
    let collection = load_mdbase_collection(root).unwrap().unwrap();
    let types = load_mdbase_type_registry(&collection).unwrap();
    let contracts = load_mdbase_contract_registry(&collection, &types).unwrap();
    load_mdbase_records_with_contracts(&collection, &types, &contracts, false).unwrap()
}

fn invoke(view: &str) -> MdbaseViewInvocation {
    MdbaseViewInvocation {
        source: "views/tasks.md".to_string(),
        view: view.to_string(),
        ..MdbaseViewInvocation::default()
    }
}

fn codes(error: &MdbaseQueryError) -> Vec<&str> {
    error
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

fn execute(root: &Path, resolved: &MdbaseResolvedView) -> crate::mdbase::MdbaseQueryResult {
    let collection = load_mdbase_collection(root).unwrap().unwrap();
    let types = load_mdbase_type_registry(&collection).unwrap();
    compile_mdbase_prepared_query(&resolved.query)
        .unwrap()
        .execute(&load_records(root), &types, "id", None, chrono::Utc::now())
        .unwrap()
}

#[test]
fn discovery_describes_sources_in_path_order_with_selected_property_metadata() {
    let (_directory, records) = collection(&[(
        "views/another.md",
        "---\ntype: view\nid: another\nversion: 1\nname: Another\nviews:\n  - id: all\n    name: All\n---\n",
    )]);
    let list = list_mdbase_views(&records);
    assert!(list.diagnostics.is_empty(), "{:?}", list.diagnostics);
    assert_eq!(list.meta.total_count, 2);
    assert_eq!(
        list.views
            .iter()
            .map(|view| view.source.path.as_str())
            .collect::<Vec<_>>(),
        ["views/another.md", "views/tasks.md"]
    );
    let tasks = &list.views[1];
    assert_eq!(tasks.id, "task.views");
    assert_eq!(tasks.source.format, MDBASE_VIEW_SOURCE_FORMAT);
    assert!(tasks.source.writable);
    assert_eq!(
        tasks.source.revision,
        records.get("views/tasks.md").unwrap().revision
    );
    assert_eq!(
        tasks
            .views
            .iter()
            .map(|named| named.id.as_str())
            .collect::<Vec<_>>(),
        ["open", "everything", "required"]
    );
    assert_eq!(
        serde_json::to_value(&tasks.views[0].properties).unwrap(),
        json!([
            {"key": "title", "label": "Task", "format": "text"},
            {"key": "urgency", "label": "Urgency", "hidden": true},
            {"key": "shout", "label": "Loud"},
        ])
    );
    assert_eq!(
        tasks.views[0].presentation,
        Some(json!({"type": "example.list"}))
    );
    assert!(tasks.views[1].properties.is_empty());
}

#[test]
fn malformed_sources_are_omitted_with_warnings_and_fail_explicit_resolution() {
    let (_directory, records) = collection(&[
        (
            "views/duplicate.md",
            "---\ntype: view\nid: dup\nversion: 1\nname: Dup\nviews:\n  - id: same\n    name: One\n  - id: same\n    name: Two\n---\n",
        ),
        (
            "views/conflict.md",
            "---\ntype: view\nid: conflict\nversion: 1\nname: Conflict\nquery:\n  projections:\n    x:\n      expr: '1'\nviews:\n  - id: v\n    name: V\n    projections:\n      x:\n        expr: '2'\n---\n",
        ),
        (
            "views/schema.md",
            "---\ntype: view\nid: schema\nversion: 1\nviews:\n  - id: v\n    name: V\n---\n",
        ),
    ]);
    let list = list_mdbase_views(&records);
    assert_eq!(list.meta.total_count, 1);
    assert_eq!(list.views[0].source.path, "views/tasks.md");
    let reported = list
        .diagnostics
        .iter()
        .map(|diagnostic| {
            assert_eq!(diagnostic.severity, MdbaseDiagnosticLevel::Warning);
            assert_eq!(diagnostic.code, "invalid_view");
            diagnostic.path.as_deref().unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        reported,
        BTreeSet::from(["views/conflict.md", "views/duplicate.md", "views/schema.md"])
    );
    for (path, view) in [
        ("views/duplicate.md", "same"),
        ("views/conflict.md", "v"),
        ("views/schema.md", "v"),
    ] {
        let error = resolve_mdbase_view(
            &records,
            &MdbaseViewInvocation {
                source: path.to_string(),
                view: view.to_string(),
                ..MdbaseViewInvocation::default()
            },
        )
        .unwrap_err();
        assert!(codes(&error).iter().all(|code| *code == "invalid_view"));
        assert_eq!(error.diagnostics[0].severity, MdbaseDiagnosticLevel::Error);
    }
}

#[test]
fn resolution_inherits_shared_scope_and_combines_filters() {
    let (_directory, records) = collection(&[]);
    let open = resolve_mdbase_view(
        &records,
        &MdbaseViewInvocation {
            context: MdbaseViewContextArg::Path("projects/alpha.md".to_string()),
            ..invoke("open")
        },
    )
    .unwrap();
    assert_eq!(open.path, "views/tasks.md");
    assert_eq!(open.id, "open");
    assert_eq!(
        open.query,
        json!({
            "types": ["task"],
            "where": "(status != \"archived\") && (status == \"open\")",
            "projections": {"urgency": {"expr": "priority * 2"}},
            "summary_functions": {"joined": {"expr": "values.join(\",\")"}},
            "select": [
                "title",
                "projection.urgency",
                {"name": "shout", "expr": "title + \"!\"", "label": "Loud"},
            ],
            "order_by": [{"field": "priority", "direction": "desc"}],
            "limit": 5,
            "context": {"this": {"path": "projects/alpha.md"}},
        })
    );
    // A named view's `types` replaces the shared list.
    let everything = resolve_mdbase_view(&records, &invoke("everything")).unwrap();
    assert_eq!(everything.query["types"], json!(["task", "project"]));
    assert!(everything.query.get("context").is_none());
}

#[test]
fn invocation_pagination_and_timezone_override_the_saved_view() {
    let (_directory, records) = collection(&[]);
    let resolved = resolve_mdbase_view(
        &records,
        &MdbaseViewInvocation {
            context: MdbaseViewContextArg::Path("projects/alpha.md".to_string()),
            limit: Some(1),
            offset: Some(2),
            timezone: Some("Europe/Berlin".to_string()),
            ..invoke("open")
        },
    )
    .unwrap();
    assert_eq!(resolved.query["limit"], 1);
    assert_eq!(resolved.query["offset"], 2);
    assert_eq!(resolved.query["timezone"], "Europe/Berlin");
}

#[test]
fn context_policy_binds_view_null_or_requires_a_record() {
    let (_directory, records) = collection(&[]);
    let context = |view: &str, argument: MdbaseViewContextArg| {
        resolve_mdbase_view(
            &records,
            &MdbaseViewInvocation {
                context: argument,
                ..invoke(view)
            },
        )
        .map(|resolved| resolved.query.get("context").cloned())
    };
    // The shared context declares `types: [project]` with the default
    // `on_missing: view`, and the view record is not a project.
    assert_eq!(
        codes(&context("open", MdbaseViewContextArg::Absent).unwrap_err()),
        ["context_type_mismatch"]
    );
    assert_eq!(
        codes(&context("open", MdbaseViewContextArg::Path("tasks/a.md".to_string())).unwrap_err()),
        ["context_type_mismatch"]
    );
    assert_eq!(
        codes(
            &context(
                "open",
                MdbaseViewContextArg::Path("projects/missing.md".to_string())
            )
            .unwrap_err()
        ),
        ["context_not_found"]
    );
    // An explicit null always wins, even over a type constraint.
    assert_eq!(context("open", MdbaseViewContextArg::Null).unwrap(), None);
    // A named context replaces the shared declaration rather than merging:
    // `everything` drops the project type constraint.
    assert_eq!(
        context("everything", MdbaseViewContextArg::Absent).unwrap(),
        None
    );
    assert_eq!(
        context(
            "everything",
            MdbaseViewContextArg::Path("tasks/a.md".to_string())
        )
        .unwrap(),
        Some(json!({"this": {"path": "tasks/a.md"}}))
    );
    assert_eq!(
        codes(&context("required", MdbaseViewContextArg::Absent).unwrap_err()),
        ["context_required"]
    );
}

#[test]
fn view_records_bind_themselves_by_default() {
    let (_directory, records) = collection(&[(
        "views/plain.md",
        "---\ntype: view\nid: plain\nversion: 1\nname: Plain\nviews:\n  - id: all\n    name: All\n---\n",
    )]);
    let resolved = resolve_mdbase_view(
        &records,
        &MdbaseViewInvocation {
            source: "plain".to_string(),
            view: "all".to_string(),
            ..MdbaseViewInvocation::default()
        },
    )
    .unwrap();
    assert_eq!(resolved.path, "views/plain.md");
    assert_eq!(
        resolved.query,
        json!({"context": {"this": {"path": "views/plain.md"}}})
    );
}

#[test]
fn unknown_sources_views_and_ambiguous_ids_fail_before_execution() {
    let (_directory, records) = collection(&[(
        "views/copy.md",
        &TASK_VIEWS.replace("name: Task views", "name: Copy"),
    )]);
    let error = |source: &str, view: &str| {
        let error = resolve_mdbase_view(
            &records,
            &MdbaseViewInvocation {
                source: source.to_string(),
                view: view.to_string(),
                ..MdbaseViewInvocation::default()
            },
        )
        .unwrap_err();
        codes(&error).join(",")
    };
    assert_eq!(error("views/missing.md", "open"), "view_not_found");
    // An ordinary record is not a view source, even when addressed by path.
    assert_eq!(error("tasks/a.md", "open"), "view_not_found");
    assert_eq!(error("views/tasks.md", "missing"), "view_not_found");
    assert_eq!(error("task.views", "open"), "invalid_view");
    assert_eq!(
        resolve_mdbase_view(
            &records,
            &MdbaseViewInvocation {
                source: "views/copy.md".to_string(),
                context: MdbaseViewContextArg::Null,
                ..invoke("open")
            }
        )
        .unwrap()
        .path,
        "views/copy.md"
    );
}

#[test]
fn rendered_output_is_unsupported_while_headless_execution_succeeds() {
    let (directory, records) = collection(&[]);
    let error = resolve_mdbase_view(
        &records,
        &MdbaseViewInvocation {
            render: true,
            context: MdbaseViewContextArg::Path("projects/alpha.md".to_string()),
            ..invoke("open")
        },
    )
    .unwrap_err();
    assert_eq!(codes(&error), ["unsupported_presentation"]);
    let resolved = resolve_mdbase_view(
        &records,
        &MdbaseViewInvocation {
            context: MdbaseViewContextArg::Path("projects/alpha.md".to_string()),
            ..invoke("open")
        },
    )
    .unwrap();
    let result = execute(directory.path(), &resolved);
    let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
    let shared = execute_mdbase_view(
        &records,
        &load_mdbase_type_registry(&collection).unwrap(),
        &MdbaseViewInvocation {
            context: MdbaseViewContextArg::Path("projects/alpha.md".to_string()),
            ..invoke("open")
        },
        "id",
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(shared.results, result.results);
    assert_eq!(
        shared.meta.view,
        Some(crate::mdbase::MdbaseQueryViewMeta {
            path: "views/tasks.md".to_string(),
            id: "open".to_string(),
        })
    );
    assert_eq!(result.meta.view, None);
    assert_eq!(
        result
            .results
            .iter()
            .map(|row| row.values.clone().unwrap())
            .collect::<Vec<_>>(),
        [
            json!({"title": "B", "urgency": 6, "shout": "B!"}),
            json!({"title": "A", "urgency": 2, "shout": "A!"}),
        ]
    );
    assert_eq!(result.meta.context.unwrap().path, "projects/alpha.md");
}
