use super::*;
use crate::mdbase::{
    compose_mdbase_type_behavior, load_mdbase_collection, load_mdbase_type_registry,
};
use serde_json::json;

fn input<'a>(draft: &'a Value, old: Option<&'a Value>) -> MdbaseLifecycleInput<'a> {
    MdbaseLifecycleInput {
        event: if old.is_some() {
            MdbaseLifecycleEvent::Update
        } else {
            MdbaseLifecycleEvent::Create
        },
        draft,
        old,
        file: json!({"path": "tasks/example.md"}),
        operation: json!({"kind": if old.is_some() { "update" } else { "create" }}),
        known_fields: vec!["status".to_string(), "missing".to_string()],
        clock: MdbaseCelClock::new("2026-09-08T23:30:45Z".parse().unwrap(), "Europe/Berlin")
            .unwrap(),
        link_index: None,
    }
}

fn policy(event: &str, fields: Value) -> MdbaseComposedTypeBehavior {
    let Value::Object(fields) = fields else {
        panic!("test fields must be an object")
    };
    MdbaseComposedTypeBehavior {
        types: vec!["task".to_string()],
        lifecycle: BTreeMap::from([(event.to_string(), fields.into_iter().collect())]),
        ..Default::default()
    }
}

fn pinned_behavior(group: usize, names: &[&str]) -> MdbaseComposedTypeBehavior {
    let suite: Value = serde_yaml::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/resources/mdbase/v0.3/upstream/tests/lifecycle/lifecycle.yaml"
    )))
    .unwrap();
    let setup = &suite["groups"][group]["setup"];
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("mdbase.yaml"),
        setup["config"].as_str().unwrap(),
    )
    .unwrap();
    std::fs::create_dir(directory.path().join("_types")).unwrap();
    for (name, source) in setup["types"].as_object().unwrap() {
        std::fs::write(
            directory.path().join("_types").join(name),
            source.as_str().unwrap(),
        )
        .unwrap();
    }
    if names.contains(&"task_copy") {
        let source = setup["types"]["task.md"].as_str().unwrap();
        std::fs::write(
            directory.path().join("_types/task-copy.md"),
            source.replace("name: task\n", "name: task_copy\n"),
        )
        .unwrap();
    }
    let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
    let registry = load_mdbase_type_registry(&collection).unwrap();
    compose_mdbase_type_behavior(
        &registry,
        &names.iter().map(ToString::to_string).collect::<Vec<_>>(),
    )
}

#[test]
fn pinned_create_policy_generates_required_fields_without_materializing_defaults() {
    let behavior = pinned_behavior(0, &["task"]);
    let draft = json!({"type": "task", "title": "Created task"});
    let result = evaluate_mdbase_lifecycle(
        &behavior,
        input(&draft, None),
        &MdbaseCelEngine::default(),
        || Ok([1; 16]),
    )
    .unwrap();
    assert_eq!(result.assignments["slug"], "created-task");
    assert_eq!(
        result.assignments["dateCreated"],
        "2026-09-08T23:30:45.000Z"
    );
    assert_eq!(
        result.assignments["dateModified"],
        result.assignments["dateCreated"]
    );
    assert!(result.assignments["id"]
        .as_str()
        .unwrap()
        .parse::<ulid::Ulid>()
        .is_ok());
    assert!(!result.assignments.contains_key("status"));
    assert_eq!(draft, json!({"type": "task", "title": "Created task"}));
}

#[test]
fn pinned_update_policy_refreshes_only_modified_timestamp() {
    let behavior = pinned_behavior(0, &["task"]);
    let old =
        json!({"type": "task", "title": "Existing task", "dateCreated": "2026-06-14T08:00:00Z"});
    let mut draft = old.clone();
    draft["title"] = json!("Renamed");
    let result = evaluate_mdbase_lifecycle(
        &behavior,
        input(&draft, Some(&old)),
        &MdbaseCelEngine::default(),
        || panic!("update does not generate an ID"),
    )
    .unwrap();
    assert_eq!(
        result.assignments,
        BTreeMap::from([(
            "dateModified".to_string(),
            json!("2026-09-08T23:30:45.000Z")
        )])
    );
    assert_eq!(draft["dateCreated"], old["dateCreated"]);
}

#[test]
fn pinned_transition_guard_runs_only_when_status_becomes_done() {
    let behavior = pinned_behavior(1, &["guarded"]);
    let old = json!({"status": "open"});
    for (status, expected) in [("open", false), ("done", true)] {
        let draft = json!({"status": status});
        let result = evaluate_mdbase_lifecycle(
            &behavior,
            input(&draft, Some(&old)),
            &MdbaseCelEngine::default(),
            || panic!("today needs no entropy"),
        )
        .unwrap();
        assert_eq!(result.assignments.contains_key("completedDate"), expected);
        if expected {
            assert_eq!(result.assignments["completedDate"], "2026-09-09");
        }
        assert_eq!(result.evaluated_guards, 1);
    }
}

#[test]
fn pinned_conflicts_keep_type_and_location_evidence_before_generating() {
    let behavior = pinned_behavior(1, &["conflict_a", "conflict_b"]);
    let reordered = pinned_behavior(1, &["conflict_b", "conflict_a"]);
    let draft = json!({"title": "Conflicting lifecycle"});
    let errors = evaluate_mdbase_lifecycle(
        &behavior,
        input(&draft, Some(&draft)),
        &MdbaseCelEngine::default(),
        || panic!("conflicts must fail before generation"),
    )
    .unwrap_err();
    assert_eq!(errors, reordered.diagnostics);
    assert_eq!(errors[0].code, "type_conflict");
    assert_eq!(errors[0].field, "stamp");
    assert_eq!(errors[0].type_names, ["conflict_a", "conflict_b"]);
    assert_eq!(
        errors[0].locations,
        [
            "conflict_a:lifecycle.on_update.stamp",
            "conflict_b:lifecycle.on_update.stamp"
        ]
    );
}

#[test]
fn identical_matched_type_assignments_generate_once() {
    let mut behavior = pinned_behavior(0, &["task", "task_copy"]);
    // Composition coalesces duplicate matched type declarations before execution.
    assert_eq!(behavior.types, ["task", "task_copy"]);
    behavior
        .lifecycle
        .get_mut("on_create")
        .unwrap()
        .retain(|field, _| field == "id");
    let mut calls = 0;
    let draft = json!({"title": "Task"});
    let result = evaluate_mdbase_lifecycle(
        &behavior,
        input(&draft, None),
        &MdbaseCelEngine::default(),
        || {
            calls += 1;
            Ok([0; 16])
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(result.assignments.len(), 1);
}

#[test]
fn guards_share_raw_snapshot_presence_and_reserved_bindings() {
    let guard = "old.status == 'open' && status == 'done' && missing == null && !present.raw.missing && present.raw.empty && empty == null && file.path == 'tasks/example.md' && operation.kind == 'update' && raw.file == 'spoof'";
    let behavior = policy(
        "on_update",
        json!({
            "a": [{"if": guard, "value": {"literal": 1}}],
            "b": [{"if": guard, "value": {"copy": "status"}}],
            "status": [{"if": null, "value": {"literal": "archived"}}]
        }),
    );
    let old = json!({"status": "open"});
    let draft =
        json!({"status": "done", "empty": null, "file": "spoof", "old": {"status": "spoof"}});
    let result = evaluate_mdbase_lifecycle(
        &behavior,
        input(&draft, Some(&old)),
        &MdbaseCelEngine::default(),
        || panic!("no IDs"),
    )
    .unwrap();
    assert_eq!(result.evaluated_guards, 1);
    assert_eq!(result.assignments["a"], 1);
    assert_eq!(result.assignments["b"], "done");
    assert_eq!(result.assignments["status"], "archived");
    assert_eq!(draft["status"], "done");
}

#[test]
fn false_null_and_create_old_null_guards_have_distinct_semantics() {
    let behavior = policy(
        "on_create",
        json!({
            "skip_false": [{"if": "false", "value": {"uuid": true}}],
            "skip_null": [{"if": "missing", "value": {"uuid": true}}],
            "created": [{"if": "old == null", "value": {"literal": true}}]
        }),
    );
    let result = evaluate_mdbase_lifecycle(
        &behavior,
        input(&json!({}), None),
        &MdbaseCelEngine::default(),
        || panic!("skipped actions must not generate"),
    )
    .unwrap();
    assert_eq!(
        result.assignments,
        BTreeMap::from([("created".to_string(), json!(true))])
    );
}

#[test]
fn guard_compile_evaluation_type_and_binding_errors_fail_the_operation() {
    for guard in ["(", "1 / 0 == 0", "42", "event.kind == 'change'"] {
        let behavior = policy(
            "on_create",
            json!({"field": [{"if": guard, "value": {"ulid": true}}]}),
        );
        let errors = evaluate_mdbase_lifecycle(
            &behavior,
            input(&json!({}), None),
            &MdbaseCelEngine::default(),
            || panic!("failed guard must not generate"),
        )
        .unwrap_err();
        assert_eq!(errors[0].code, "lifecycle_expression_error", "{guard}");
        assert_eq!(errors[0].field, "field");
    }
}

#[test]
fn ordered_field_actions_keep_the_last_active_value() {
    let behavior = policy(
        "on_create",
        json!({"field": [
            {"if": null, "value": {"literal": 1}},
            {"if": "true", "value": {"literal": 2}},
            {"if": "false", "value": {"literal": 3}}
        ]}),
    );
    let result = evaluate_mdbase_lifecycle(
        &behavior,
        input(&json!({}), None),
        &MdbaseCelEngine::default(),
        || panic!("no IDs"),
    )
    .unwrap();
    assert_eq!(result.assignments["field"], 2);
}

#[test]
fn malformed_inputs_and_provider_failures_return_no_partial_result() {
    let behavior = policy(
        "on_create",
        json!({
            "a": [{"if": null, "value": {"literal": "prepared"}}],
            "b": [{"if": null, "value": {"copy": "absent"}}]
        }),
    );
    let errors = evaluate_mdbase_lifecycle(
        &behavior,
        input(&json!({}), None),
        &MdbaseCelEngine::default(),
        || panic!("no IDs"),
    )
    .unwrap_err();
    assert_eq!(errors[0].code, "lifecycle_provider_error");
    assert_eq!(errors[0].field, "b");
    let draft = json!({});
    let mut invalid = input(&draft, None);
    invalid.event = MdbaseLifecycleEvent::Update;
    let errors = evaluate_mdbase_lifecycle(&behavior, invalid, &MdbaseCelEngine::default(), || {
        panic!("invalid input")
    })
    .unwrap_err();
    assert_eq!(errors[0].code, "lifecycle_input_invalid");
}
