//! Cross-frontend parity (QRY.7): the same question asked in DQL, Bases,
//! `QueryAst`, and mdbase over one collection returns the same notes in the
//! same order wherever the dialects define the question identically, through
//! the direct note store and a retained session snapshot alike.

use std::fmt::Write as _;
use std::fs;
use tempfile::TempDir;
use vulcan_core::note_session::NoteStoreSession;
use vulcan_core::note_store::{DirectNoteStore, NoteStore};
use vulcan_core::{
    resolve_permission_profile, scan_vault, PermissionGuard, ProfilePermissionGuard, QueryAst,
    ScanMode, VaultPaths,
};

const STATUSES: [&str; 3] = ["active", "done", "waiting"];

/// An mdbase collection of tasks in two folders, plus untyped notes and
/// notes of another type that no task question may return.
fn collection() -> (TempDir, VaultPaths) {
    let temp_dir = TempDir::new().expect("temp dir");
    let root = temp_dir.path();
    fs::create_dir_all(root.join(".vulcan")).unwrap();
    fs::create_dir_all(root.join("_types")).unwrap();
    fs::write(root.join("mdbase.yaml"), "spec_version: \"0.3.0\"\n").unwrap();
    for kind in ["task", "contact"] {
        fs::write(
            root.join(format!("_types/{kind}.md")),
            format!(
                "---\nkind: mdbase.type\nname: {kind}\nschema:\n  dialect: json-schema-2020-12\n  value: {{type: object}}\n---\n"
            ),
        )
        .unwrap();
    }
    for index in 0..24 {
        let folder = if index % 2 == 0 { "work" } else { "home" };
        let status = STATUSES[index % 3];
        let priority = index % 5 + 1;
        // Titles sort differently from paths.
        let title = format!("Task {:02}", (index * 7) % 24);
        fs::create_dir_all(root.join(format!("tasks/{folder}"))).unwrap();
        fs::write(
            root.join(format!("tasks/{folder}/t{index:02}.md")),
            format!(
                "---\ntype: task\ntitle: {title}\nstatus: {status}\npriority: {priority}\ntags: [{folder}]\n---\n# {title}\n"
            ),
        )
        .unwrap();
    }
    for index in 0..4 {
        fs::write(
            root.join(format!("tasks/work/c{index}.md")),
            format!(
                "---\ntype: contact\ntitle: Contact {index}\nstatus: active\npriority: 5\n---\n"
            ),
        )
        .unwrap();
    }
    fs::write(
        root.join("tasks/work/loose.md"),
        "---\nstatus: active\npriority: 9\n---\n",
    )
    .unwrap();
    let paths = VaultPaths::new(root);
    scan_vault(&paths, ScanMode::Full).expect("scan");
    (temp_dir, paths)
}

/// One question in each frontend's language.
struct Question {
    name: &'static str,
    dql: String,
    query: String,
    bases_filters: Vec<String>,
    bases_sort: (&'static str, &'static str),
    mdbase: serde_json::Value,
}

fn questions() -> Vec<Question> {
    vec![
        Question {
            name: "type and status, by title",
            dql: "TABLE WITHOUT ID file.path AS path FROM \"tasks\" WHERE type = \"task\" AND status = \"active\" SORT title ASC".to_string(),
            query: "from notes where type = task and status = active order by title".to_string(),
            bases_filters: vec!["type == \"task\"".to_string(), "status == \"active\"".to_string()],
            bases_sort: ("title", "ASC"),
            mdbase: serde_json::json!({
                "types": ["task"],
                "where": "status == \"active\"",
                "order_by": [{"field": "title", "direction": "asc"}],
            }),
        },
        Question {
            name: "numeric comparison, by title descending",
            dql: "TABLE WITHOUT ID file.path AS path FROM \"tasks\" WHERE type = \"task\" AND priority > 2 SORT title DESC".to_string(),
            query: "from notes where type = task and priority > 2 order by title desc".to_string(),
            bases_filters: vec!["type == \"task\"".to_string(), "priority > 2".to_string()],
            bases_sort: ("title", "DESC"),
            mdbase: serde_json::json!({
                "types": ["task"],
                "where": "priority > 2",
                "order_by": [{"field": "title", "direction": "desc"}],
            }),
        },
        Question {
            name: "folder and status, by priority then path",
            dql: "TABLE WITHOUT ID file.path AS path FROM \"tasks/work\" WHERE type = \"task\" AND status != \"done\" SORT priority ASC, file.path ASC".to_string(),
            query: "from notes where file.path starts_with \"tasks/work/\" and type = task and status != done order by priority".to_string(),
            bases_filters: vec![
                "file.inFolder(\"tasks/work\")".to_string(),
                "type == \"task\"".to_string(),
                "status != \"done\"".to_string(),
            ],
            bases_sort: ("priority", "ASC"),
            mdbase: serde_json::json!({
                "types": ["task"],
                "where": "file.path.startsWith(\"tasks/work/\") && status != \"done\"",
                "order_by": [
                    {"field": "priority", "direction": "asc"},
                    {"field": "file.path", "direction": "asc"}
                ],
            }),
        },
    ]
}

fn guard(paths: &VaultPaths) -> ProfilePermissionGuard {
    ProfilePermissionGuard::new(paths, resolve_permission_profile(paths, None).unwrap())
}

/// Each native frontend's ordered paths for `question` through `store`.
fn native_answers(
    store: &dyn NoteStore,
    paths: &VaultPaths,
    question: &Question,
) -> Vec<(&'static str, Vec<String>)> {
    let guard = guard(paths);
    let dql = vulcan_core::dql::evaluate_dql_in(store, paths, &question.dql, None, &guard, false)
        .unwrap_or_else(|error| panic!("{}: {error}", question.name));
    let dql = dql
        .rows
        .iter()
        .map(|row| row["path"].as_str().expect("path column").to_string())
        .collect();
    let ast = QueryAst::from_dsl(&question.query).expect("query DSL");
    let query = vulcan_core::execute_query_report_in(store, paths, ast, Some(&guard.read_filter()))
        .expect("query");
    let query = query
        .notes
        .iter()
        .map(|note| note.document_path.clone())
        .collect();
    let mut base = "filters:\n  and:\n".to_string();
    for filter in &question.bases_filters {
        base.push_str("    - '");
        base.push_str(filter);
        base.push_str("'\n");
    }
    write!(
        base,
        "views:\n  - type: table\n    name: rows\n    order:\n      - file.path\n    sort:\n      - property: {}\n        direction: {}\n",
        question.bases_sort.0, question.bases_sort.1,
    )
    .unwrap();
    fs::write(paths.vault_root().join("parity.base"), base).unwrap();
    let bases =
        vulcan_core::bases::evaluate_base_file_in(store, paths, "parity.base", &guard, false)
            .unwrap_or_else(|error| panic!("{}: {error}", question.name));
    assert!(bases.diagnostics.is_empty(), "{:?}", bases.diagnostics);
    let bases = bases.views[0]
        .rows
        .iter()
        .map(|row| row.document_path.clone())
        .collect();
    vec![("dql", dql), ("query", query), ("bases", bases)]
}

#[test]
fn equivalent_questions_return_identical_ordered_answers_in_every_frontend() {
    let (_temp_dir, paths) = collection();
    let session = NoteStoreSession::new(paths.clone());
    for question in questions() {
        let mdbase = vulcan_app::mdbase::build_mdbase_query_report(&paths, &question.mdbase, None)
            .unwrap_or_else(|error| panic!("{}: {error}", question.name));
        assert!(mdbase.diagnostics.is_empty(), "{:?}", mdbase.diagnostics);
        let expected = mdbase
            .results
            .iter()
            .map(|row| row.file["path"].as_str().expect("record path").to_string())
            .collect::<Vec<_>>();
        assert!(expected.len() >= 4, "{}: {expected:?}", question.name);
        let direct = native_answers(&DirectNoteStore::new(&paths), &paths, &question);
        let snapshot = session.snapshot().expect("no writer is active");
        let retained = native_answers(&snapshot, &paths, &question);
        drop(snapshot);
        for (frontend, answer) in direct.into_iter().chain(retained) {
            assert_eq!(answer, expected, "{}: {frontend}", question.name);
        }
    }
}
