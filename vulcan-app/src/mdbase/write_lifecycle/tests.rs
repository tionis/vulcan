use super::*;
use crate::mdbase::*;
use chrono::TimeZone;
use std::fs;
use vulcan_core::mdbase::{
    list_mdbase_write_outbox, load_mdbase_collection, load_mdbase_record, load_mdbase_type_registry,
};
use vulcan_core::VaultPaths;

fn fixture(policy: &str, collection: &str) -> (tempfile::TempDir, VaultPaths) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("mdbase.yaml"),
        "spec_version: '0.3.0'\nsettings:\n  timezone: Europe/Berlin\n",
    )
    .unwrap();
    fs::create_dir(dir.path().join("_types")).unwrap();
    fs::write(dir.path().join("_types/task.md"), format!("---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [type, id, title]\n    properties:\n      type: {{const: task}}\n      id: {{type: string}}\n      title: {{type: string}}\n      status: {{type: string}}\ncollection:\n{collection}lifecycle:\n{policy}---\n")).unwrap();
    let paths = VaultPaths::new(dir.path());
    (dir, paths)
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 13, 23, 30, 0).unwrap()
}

fn request(
    operation: MdbaseWriteOperation,
    changes: &[(&str, Option<&str>)],
) -> MdbaseWritePlanRequest {
    MdbaseWritePlanRequest {
        caller_id: "test".to_string(),
        instance_id: "test".to_string(),
        operation,
        changes: changes
            .iter()
            .map(|(path, after)| MdbaseWriteChangeRequest {
                path: (*path).to_string(),
                after: after.map(str::to_string),
                if_revision: None,
            })
            .collect(),
        matched_types: Vec::new(),
        generated_values: BTreeMap::new(),
        permission_profile: None,
        ttl_seconds: None,
    }
}

fn apply(paths: &VaultPaths, plan: &MdbaseWritePlanReport, key: &str) -> MdbaseWriteApplyReport {
    apply_mdbase_write(
        paths,
        plan,
        &MdbaseWriteExecutionOptions {
            idempotency_key: key.to_string(),
            no_commit: true,
            quiet: true,
        },
        now() + chrono::Duration::seconds(2),
    )
    .unwrap()
}

const DEFAULTS: &str = "  read_defaults: {status: open}\n";
const SOURCE: &str = "---\ntype: task\nid: original\ntitle: Before\nstatus: open\n---\nBody\n";

#[test]
fn note_create_append_and_patch_report_policy_output_including_dry_run() {
    use crate::notes::*;
    use vulcan_core::config::VaultConfig;
    let (dir, paths) = fixture("  on_create:\n    set:\n      id: {literal: created}\n  on_update:\n    set:\n      id: {ulid: true}\n", DEFAULTS);
    let created = apply_note_create(
        &paths,
        &NoteCreateRequest {
            path: "a.md".into(),
            template: None,
            frontmatter: None,
            body: SOURCE.into(),
        },
        None,
        true,
    )
    .unwrap();
    assert!(created.content.contains("id: created"));
    assert_eq!(
        created.content,
        fs::read_to_string(dir.path().join("a.md")).unwrap()
    );
    let appended = apply_note_append(
        &paths,
        &NoteAppendRequest {
            note: Some("a.md".into()),
            text: "Extra".into(),
            mode: NoteAppendMode::Append,
            heading: None,
            periodic: None,
            date: None,
            vars: std::collections::HashMap::default(),
        },
        None,
        true,
    )
    .unwrap();
    assert!(!appended.content.contains("id: created"));
    assert_eq!(
        appended.content,
        fs::read_to_string(dir.path().join("a.md")).unwrap()
    );
    let mut request = NotePatchRequest {
        target: MarkdownTarget {
            display_path: "a.md".into(),
            absolute_path: dir.path().join("a.md"),
            vault_relative_path: Some("a.md".into()),
            config: VaultConfig::default(),
        },
        section_id: None,
        heading: None,
        block_ref: None,
        lines: None,
        find: "Extra".into(),
        replace: "Patched".into(),
        replace_all: false,
        dry_run: true,
    };
    let preview = apply_note_patch(&paths, &request, None, true).unwrap();
    assert!(preview.content.contains("Patched"));
    assert_ne!(
        preview.content,
        appended.content.replace("Extra", "Patched")
    );
    assert_eq!(
        appended.content,
        fs::read_to_string(dir.path().join("a.md")).unwrap()
    );
    assert_eq!(list_mdbase_write_outbox(&paths).unwrap().len(), 2);
    request.dry_run = false;
    let patched = apply_note_patch(&paths, &request, None, true).unwrap();
    assert_ne!(patched.content, preview.content);
    assert_eq!(
        patched.content,
        fs::read_to_string(dir.path().join("a.md")).unwrap()
    );
}

#[cfg(feature = "js_runtime")]
#[test]
fn script_results_refresh_after_implicit_and_explicit_lifecycle_commits() {
    use vulcan_core::config::JsRuntimeSandbox;
    use vulcan_core::dataview_js::{evaluate_dataview_js_with_options, DataviewJsEvalOptions};
    use vulcan_core::{initialize_vulcan_dir, scan_vault, ScanMode};
    let (dir, paths) = fixture(
        "  on_create:\n    set:\n      id: {literal: generated}\n",
        DEFAULTS,
    );
    initialize_vulcan_dir(&paths).unwrap();
    scan_vault(&paths, ScanMode::Full).unwrap();
    let result = evaluate_dataview_js_with_options(
        &paths,
        r#"
        const standalone = vault.create("a", {frontmatter: {type: "task", title: "A"}});
        const provisional = vault.transaction(tx => tx.create("b", {
            frontmatter: {type: "task", title: "B"}
        }));
        [standalone.id, provisional.id ?? null, dv.page("b").id]
    "#,
        None,
        DataviewJsEvalOptions {
            sandbox: Some(JsRuntimeSandbox::Fs),
            mutation_committer: Some(mdbase_js_mutation_committer(&paths, None, true)),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.value, Some(json!(["generated", null, "generated"])));
    for path in ["a.md", "b.md"] {
        assert!(fs::read_to_string(dir.path().join(path))
            .unwrap()
            .contains("id: generated"));
    }
    assert_eq!(list_mdbase_write_outbox(&paths).unwrap().len(), 2);
}

#[test]
fn failed_guard_leaves_sources_and_journal_untouched() {
    let (dir, paths) = fixture(
        "  on_update:\n    if: '1 + true'\n    set:\n      id: {ulid: true}\n",
        DEFAULTS,
    );
    fs::write(dir.path().join("a.md"), SOURCE).unwrap();
    let error = plan_mdbase_write(
        &paths,
        &request(
            MdbaseWriteOperation::Update,
            &[("a.md", Some(&SOURCE.replace("Before", "After")))],
        ),
        now(),
    )
    .unwrap_err();
    assert_eq!(error.code(), Some("lifecycle_expression_error"));
    assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), SOURCE);
    assert!(!dir.path().join(".vulcan").exists());
}

#[test]
fn all_providers_run_before_validation_and_exact_reviewed_values_survive_apply_and_replay() {
    let (dir, paths) = fixture("  on_create:\n    set:\n      id: {ulid: true}\n      uuid: {uuid: true}\n      created: {now: true}\n      day: {today: true}\n      slug: {slugify: title}\n      copied: {copy: title}\n      literal: {literal: {nested: null}}\n", DEFAULTS);
    let candidate = "\u{feff}---\r\ntype: task\r\ntitle: Hello World\r\n---\r\nBody\r\n";
    let plan = plan_mdbase_write(
        &paths,
        &request(
            MdbaseWriteOperation::Batch,
            &[("a.md", Some(candidate)), ("b.md", Some(candidate))],
        ),
        now(),
    )
    .unwrap();
    let generated = &plan.preview.generated_values["a.md"];
    assert!(ulid::Ulid::from_string(generated["id"].as_str().unwrap()).is_ok());
    assert_ne!(generated["id"], plan.preview.generated_values["b.md"]["id"]);
    assert_eq!(generated["uuid"].as_str().unwrap().len(), 36);
    assert_eq!(generated["created"], "2026-09-13T23:30:00.000Z");
    assert_eq!(generated["day"], "2026-09-14");
    assert_eq!(generated["slug"], "hello-world");
    assert_eq!(generated["copied"], "Hello World");
    assert_eq!(generated["literal"], json!({"nested": null}));
    assert!(!dir.path().join("a.md").exists());
    assert!(!dir.path().join(".vulcan").exists());
    let reviewed = plan.preview.changes[0].after.as_ref().unwrap();
    assert!(reviewed.starts_with("\u{feff}---\r\n"));
    assert!(reviewed.ends_with("---\r\nBody\r\n"));
    assert!(!reviewed.contains("status:"));
    assert!(!apply(&paths, &plan, "create").outcome.replayed);
    assert_eq!(
        fs::read_to_string(dir.path().join("a.md")).unwrap(),
        *reviewed
    );
    assert!(apply(&paths, &plan, "create").outcome.replayed);
    assert_eq!(list_mdbase_write_outbox(&paths).unwrap().len(), 1);
}

#[test]
fn update_guards_see_old_raw_values_and_noop_policy_preserves_exact_source() {
    let (dir, paths) = fixture("  on_update:\n    - if: 'old.status != status && status == \"done\" && operation.kind == \"update\"'\n      set:\n        completed: {today: true}\n", DEFAULTS);
    fs::write(dir.path().join("a.md"), SOURCE).unwrap();
    let noop = plan_mdbase_write(
        &paths,
        &request(MdbaseWriteOperation::Update, &[("a.md", Some(SOURCE))]),
        now(),
    )
    .unwrap();
    assert_eq!(noop.preview.changes[0].after.as_deref(), Some(SOURCE));
    assert!(noop.preview.generated_values.is_empty());
    let done = SOURCE.replace("status: open", "status: done");
    let plan = plan_mdbase_write(
        &paths,
        &request(MdbaseWriteOperation::Update, &[("a.md", Some(&done))]),
        now(),
    )
    .unwrap();
    assert_eq!(
        plan.preview.generated_values["a.md"]["completed"],
        "2026-09-14"
    );
    apply(&paths, &plan, "done");
    assert_eq!(
        fs::read_to_string(dir.path().join("a.md")).unwrap(),
        *plan.preview.changes[0].after.as_ref().unwrap()
    );
}

#[test]
fn generated_values_cannot_change_membership_or_violate_collection_uniqueness() {
    let (dir, paths) = fixture(
        "  on_create:\n    set:\n      id: {literal: same}\n",
        "  unique: [{field: id, scope: collection}]\n",
    );
    fs::write(
        dir.path().join("existing.md"),
        SOURCE.replace("original", "same"),
    )
    .unwrap();
    let draft = "---\ntype: task\ntitle: Candidate\n---\n";
    let error = plan_mdbase_write(
        &paths,
        &request(MdbaseWriteOperation::Create, &[("a.md", Some(draft))]),
        now(),
    )
    .unwrap_err();
    assert!(error.message().contains("duplicate_value"));
    let type_path = dir.path().join("_types/task.md");
    let policy = fs::read_to_string(&type_path).unwrap().replace(
        "id: {literal: same}",
        "type: {literal: [task, other]}\n      id: {literal: different}",
    );
    fs::write(type_path, policy).unwrap();
    fs::write(dir.path().join("_types/other.md"), "---\nkind: mdbase.type\nname: other\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\n---\n").unwrap();
    let error = plan_mdbase_write(
        &paths,
        &request(MdbaseWriteOperation::Create, &[("a.md", Some(draft))]),
        now(),
    )
    .unwrap_err();
    assert_eq!(error.code(), Some("type_membership_changed"));
    assert!(!dir.path().join("a.md").exists());
    assert!(!dir.path().join(".vulcan").exists());
}

#[test]
fn link_guards_require_complete_visibility_and_receive_authorized_metadata() {
    let (dir, paths) = fixture("  on_update:\n    - if: 'link(\"[[target]]\").asFile().basename == \"target\" && file.mtime != null'\n      set:\n        stamp: {literal: linked}\n", DEFAULTS);
    fs::write(dir.path().join("a.md"), SOURCE).unwrap();
    fs::write(dir.path().join("target.md"), "Target\n").unwrap();
    fs::create_dir(dir.path().join(".vulcan")).unwrap();
    fs::write(paths.config_file(), "[permissions.profiles.scoped]\nread = { allow = [\"note:a.md\", \"note:mdbase.yaml\", \"folder:_types/**\", \"note:mdbase.lock.yaml\", \"folder:_contracts/**\"] }\nwrite = { allow = [\"note:a.md\"] }\n").unwrap();
    let mut draft = request(MdbaseWriteOperation::Update, &[("a.md", Some(SOURCE))]);
    draft.permission_profile = Some("scoped".to_string());
    assert_eq!(
        plan_mdbase_write(&paths, &draft, now()).unwrap_err().code(),
        Some("permission_denied")
    );
    draft.permission_profile = None;
    let plan = plan_mdbase_write(&paths, &draft, now()).unwrap();
    assert_eq!(plan.preview.generated_values["a.md"]["stamp"], "linked");
    assert!(plan.preview.accepted_revisions.contains_key("target.md"));
    let type_path = dir.path().join("_types/task.md");
    let policy = fs::read_to_string(&type_path).unwrap().replace(
        "link(\"[[target]]\").asFile().basename == \"target\" && file.mtime != null",
        "file.basename == \"a\"",
    );
    fs::write(type_path, policy).unwrap();
    draft.permission_profile = Some("scoped".to_string());
    let plan = plan_mdbase_write(&paths, &draft, now()).unwrap();
    assert!(plan.authorization.record_namespaces.is_empty());
    assert_eq!(plan.preview.generated_values["a.md"]["stamp"], "linked");
}

#[test]
fn rename_preserves_identity_and_optional_hooks_fail_explicitly() {
    let (dir, paths) = fixture(
        "  on_create:\n    set: {id: {uuid: true}}\n  on_update:\n    set: {stamp: {now: true}}\n",
        DEFAULTS,
    );
    fs::write(dir.path().join("a.md"), SOURCE).unwrap();
    let draft = request(
        MdbaseWriteOperation::Rename {
            from: "a.md".to_string(),
            to: "b.md".to_string(),
        },
        &[("a.md", None), ("b.md", Some(SOURCE))],
    );
    let plan = plan_mdbase_write(&paths, &draft, now()).unwrap();
    assert!(plan.preview.generated_values.is_empty());
    assert_eq!(plan.preview.changes[1].after.as_deref(), Some(SOURCE));
    let type_path = dir.path().join("_types/task.md");
    let policy = fs::read_to_string(&type_path).unwrap();
    fs::write(
        &type_path,
        format!(
            "{}  on_rename:\n    set: {{stamp: {{now: true}}}}\n---\n",
            policy.trim_end_matches("---\n")
        ),
    )
    .unwrap();
    assert_eq!(
        plan_mdbase_write(&paths, &draft, now()).unwrap_err().code(),
        Some("lifecycle_event_unsupported")
    );
    fs::write(
        &type_path,
        format!(
            "{}  on_delete:\n    set: {{stamp: {{now: true}}}}\n---\n",
            policy.trim_end_matches("---\n")
        ),
    )
    .unwrap();
    let delete = request(MdbaseWriteOperation::Delete, &[("a.md", None)]);
    assert_eq!(
        plan_mdbase_write(&paths, &delete, now())
            .unwrap_err()
            .code(),
        Some("lifecycle_event_unsupported")
    );
    assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), SOURCE);
}

#[test]
fn raw_repair_skips_generation_and_callers_cannot_supply_generated_values() {
    let (dir, paths) = fixture("  on_create:\n    set: {id: {uuid: true}}\n", DEFAULTS);
    let mut spoofed = request(MdbaseWriteOperation::Create, &[("a.md", Some(SOURCE))]);
    spoofed
        .generated_values
        .insert("id".to_string(), json!("injected"));
    assert_eq!(
        plan_mdbase_write(&paths, &spoofed, now())
            .unwrap_err()
            .code(),
        Some("invalid_input")
    );
    let report = apply_managed_mdbase_note_write(
        &paths,
        &MdbaseManagedNoteWriteRequest {
            path: "a.md",
            before: None,
            after: Some(SOURCE),
            operation: MdbaseWriteOperation::Create,
            mode: MdbaseManagedWriteMode::RawRepair,
            dry_run: false,
            permission_profile: None,
            quiet: true,
        },
    )
    .unwrap()
    .unwrap();
    assert!(report.plan.preview.generated_values.is_empty());
    assert_eq!(fs::read_to_string(dir.path().join("a.md")).unwrap(), SOURCE);
}

#[test]
fn note_set_returns_authoritative_post_lifecycle_source() {
    let (dir, paths) = fixture(
        "  on_update:\n    set: {stamp: {literal: updated}}\n",
        DEFAULTS,
    );
    fs::write(dir.path().join("a.md"), SOURCE).unwrap();
    let report = crate::notes::apply_note_set(
        &paths,
        &crate::notes::NoteSetRequest {
            note: "a.md".to_string(),
            replacement: SOURCE.replace("Before", "After"),
            preserve_frontmatter: false,
        },
        None,
        true,
    )
    .unwrap();
    assert_eq!(
        report.content,
        fs::read_to_string(dir.path().join("a.md")).unwrap()
    );
    assert!(report.content.contains("stamp: updated"));
    let collection = load_mdbase_collection(dir.path()).unwrap().unwrap();
    let types = load_mdbase_type_registry(&collection).unwrap();
    let record = load_mdbase_record(&collection, &types, "a.md", true).unwrap();
    assert_eq!(record.frontmatter["stamp"], "updated");
}
