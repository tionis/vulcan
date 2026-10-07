//! Native contract gates for the Vulcan-owned features
//! `vulcan.record_write.v1` and `vulcan.lifecycle.v1`
//! (`docs/specs/mdb/IMPLEMENTATION_CONTRACTS.md` §1–5). Each gate drives the
//! shared App write pipeline in a fresh temporary collection; a feature is
//! claimed only when every one of its gates passes. These are feature claims
//! in Vulcan's namespace, not upstream `core_write` or `lifecycle` profiles.

use super::{
    apply_mdbase_write, build_mdbase_read_report, plan_mdbase_write, MdbaseWriteApplyReport,
    MdbaseWriteChangeRequest, MdbaseWriteExecutionOptions, MdbaseWriteOperation,
    MdbaseWritePlanReport, MdbaseWritePlanRequest,
};
use crate::mdbase_conformance::{MdbaseConformanceCaseResult, MdbaseConformanceCaseStatus};
use crate::AppError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use vulcan_core::VaultPaths;

pub const RECORD_WRITE_FEATURE: &str = "vulcan.record_write.v1";
pub const LIFECYCLE_FEATURE: &str = "vulcan.lifecycle.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseFeatureResult {
    pub feature: String,
    /// Always `vulcan`: these names never denote upstream profiles.
    pub namespace: String,
    pub passed: bool,
    pub cases: Vec<MdbaseConformanceCaseResult>,
}

type Gate = fn() -> Result<(), String>;

/// Run every gate of both features.
#[must_use]
pub fn run_mdbase_feature_gates() -> Vec<MdbaseFeatureResult> {
    let record_write: [(&str, &str, Gate); 6] = [
        (
            "vulcan.record_write.crud",
            "create, revision-checked update, and delete",
            crud_gate,
        ),
        (
            "vulcan.record_write.batch",
            "an invalid batch member rejects the whole batch",
            batch_gate,
        ),
        (
            "vulcan.record_write.rename",
            "a move rewrites references in the same transaction",
            rename_gate,
        ),
        (
            "vulcan.record_write.idempotency",
            "applies replay by key and reject reused keys",
            idempotency_gate,
        ),
        (
            "vulcan.record_write.limits",
            "oversized batches fail before any change",
            limits_gate,
        ),
        (
            "vulcan.record_write.recovery",
            "an unreconciled commit recovers to a consistent state",
            recovery_gate,
        ),
    ];
    let lifecycle: [(&str, &str, Gate); 3] = [
        (
            "vulcan.lifecycle.providers",
            "every provider is planned once and persisted exactly",
            providers_gate,
        ),
        (
            "vulcan.lifecycle.guards",
            "update guards see old values and skip when false",
            guards_gate,
        ),
        (
            "vulcan.lifecycle.events",
            "renames run no create/update policies; unsupported hooks fail",
            events_gate,
        ),
    ];
    [
        (RECORD_WRITE_FEATURE, &record_write[..]),
        (LIFECYCLE_FEATURE, &lifecycle[..]),
    ]
    .into_iter()
    .map(|(feature, gates)| {
        let cases = gates
            .iter()
            .map(|(id, name, gate)| {
                let result = gate();
                MdbaseConformanceCaseResult {
                    id: (*id).to_string(),
                    name: (*name).to_string(),
                    fixture_set: "vulcan-write-gates".to_string(),
                    operation: "write".to_string(),
                    covers: vec![feature.to_string()],
                    status: if result.is_ok() {
                        MdbaseConformanceCaseStatus::Pass
                    } else {
                        MdbaseConformanceCaseStatus::Fail
                    },
                    message: result.err(),
                }
            })
            .collect::<Vec<_>>();
        MdbaseFeatureResult {
            feature: feature.to_string(),
            namespace: "vulcan".to_string(),
            passed: cases
                .iter()
                .all(|case| case.status == MdbaseConformanceCaseStatus::Pass),
            cases,
        }
    })
    .collect()
}

struct Collection {
    directory: tempfile::TempDir,
    paths: VaultPaths,
}

impl Collection {
    fn new(lifecycle: &str) -> Result<Self, String> {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let root = directory.path();
        let write = |path: &str, contents: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().expect("fixture path has a parent"))
                .and_then(|()| fs::write(path, contents))
                .map_err(|error| error.to_string())
        };
        write(
            "mdbase.yaml",
            "spec_version: '0.3.0'\nsettings:\n  timezone: UTC\n",
        )?;
        write(
            "_types/task.md",
            &format!(
                "---\nkind: mdbase.type\nname: task\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [type, title]\n    properties:\n      type: {{const: task}}\n      title: {{type: string}}\n      status: {{type: string}}\n{lifecycle}---\n"
            ),
        )?;
        let paths = VaultPaths::new(root);
        vulcan_core::initialize_vulcan_dir(&paths).map_err(|error| error.to_string())?;
        Ok(Self { directory, paths })
    }

    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.directory.path().join(path)).ok()
    }

    fn plan(
        &self,
        operation: MdbaseWriteOperation,
        changes: Vec<(&str, Option<&str>, Option<String>)>,
    ) -> Result<MdbaseWritePlanReport, AppError> {
        plan_mdbase_write(
            &self.paths,
            &MdbaseWritePlanRequest {
                caller_id: "vulcan-conformance".to_string(),
                instance_id: "vulcan-conformance".to_string(),
                operation,
                changes: changes
                    .into_iter()
                    .map(|(path, after, if_revision)| MdbaseWriteChangeRequest {
                        path: path.to_string(),
                        after: after.map(str::to_string),
                        if_revision,
                    })
                    .collect(),
                matched_types: Vec::new(),
                generated_values: BTreeMap::new(),
                permission_profile: None,
                ttl_seconds: None,
            },
            now(),
        )
    }

    fn apply(
        &self,
        plan: &MdbaseWritePlanReport,
        key: &str,
    ) -> Result<MdbaseWriteApplyReport, AppError> {
        apply_mdbase_write(
            &self.paths,
            plan,
            &MdbaseWriteExecutionOptions {
                idempotency_key: key.to_string(),
                no_commit: true,
                quiet: true,
            },
            now(),
        )
    }

    fn write(
        &self,
        operation: MdbaseWriteOperation,
        path: &str,
        after: Option<&str>,
    ) -> Result<(), String> {
        let plan = self
            .plan(operation, vec![(path, after, None)])
            .map_err(|error| error.to_string())?;
        self.apply(&plan, &ulid::Ulid::new().to_string())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_string())
}

const RECORD: &str = "---\ntype: task\ntitle: Alpha\n---\nBody\n";

fn crud_gate() -> Result<(), String> {
    let collection = Collection::new("")?;
    collection.write(MdbaseWriteOperation::Create, "a.md", Some(RECORD))?;
    ensure(
        collection.read("a.md").as_deref() == Some(RECORD),
        "create did not persist",
    )?;
    let revision = vulcan_core::mdbase::mdbase_content_revision(RECORD);
    let updated = "---\ntype: task\ntitle: Beta\n---\nBody\n";
    let plan = collection
        .plan(
            MdbaseWriteOperation::Update,
            vec![("a.md", Some(updated), Some(revision.clone()))],
        )
        .map_err(|error| error.to_string())?;
    collection
        .apply(&plan, "update")
        .map_err(|error| error.to_string())?;
    let stale = collection.plan(
        MdbaseWriteOperation::Update,
        vec![("a.md", Some(RECORD), Some(revision))],
    );
    ensure(
        stale.is_err_and(|error| error.code() == Some("concurrent_modification")),
        "a stale revision was not rejected as concurrent_modification",
    )?;
    ensure(
        collection.read("a.md").as_deref() == Some(updated),
        "stale update changed the file",
    )?;
    let read = build_mdbase_read_report(&collection.paths, "a.md", false, None)
        .map_err(|error| error.to_string())?;
    ensure(
        read.record.frontmatter["title"] == "Beta",
        "read-after-write saw old state",
    )?;
    collection.write(MdbaseWriteOperation::Delete, "a.md", None)?;
    ensure(collection.read("a.md").is_none(), "delete left the record")
}

fn batch_gate() -> Result<(), String> {
    let collection = Collection::new("")?;
    let invalid = "---\ntype: task\n---\nNo title\n";
    let plan = collection.plan(
        MdbaseWriteOperation::Batch,
        vec![
            ("good.md", Some(RECORD), None),
            ("bad.md", Some(invalid), None),
        ],
    );
    ensure(plan.is_err(), "an invalid batch member was accepted")?;
    ensure(
        collection.read("good.md").is_none() && collection.read("bad.md").is_none(),
        "a rejected batch changed files",
    )
}

fn rename_gate() -> Result<(), String> {
    let collection = Collection::new("")?;
    collection.write(MdbaseWriteOperation::Create, "tasks/a.md", Some(RECORD))?;
    fs::write(
        collection.directory.path().join("Home.md"),
        "See [the task](tasks/a.md).\n",
    )
    .map_err(|error| error.to_string())?;
    vulcan_core::scan_vault(&collection.paths, vulcan_core::ScanMode::Full)
        .map_err(|error| error.to_string())?;
    crate::browse::move_note_with_profile(
        &collection.paths,
        "tasks/a.md",
        "archive/a.md",
        false,
        None,
    )
    .map_err(|error| error.to_string())?;
    ensure(
        collection.read("archive/a.md").as_deref() == Some(RECORD)
            && collection.read("tasks/a.md").is_none(),
        "the record did not move byte for byte",
    )?;
    ensure(
        collection
            .read("Home.md")
            .is_some_and(|home| !home.contains("tasks/a.md")),
        "the reference was not rewritten",
    )
}

fn idempotency_gate() -> Result<(), String> {
    let collection = Collection::new("")?;
    let plan = collection
        .plan(
            MdbaseWriteOperation::Create,
            vec![("a.md", Some(RECORD), None)],
        )
        .map_err(|error| error.to_string())?;
    let first = collection
        .apply(&plan, "same-key")
        .map_err(|error| error.to_string())?;
    let replay = collection
        .apply(&plan, "same-key")
        .map_err(|error| error.to_string())?;
    ensure(
        !first.outcome.replayed && replay.outcome.replayed,
        "the apply was not replayed",
    )?;
    let other = collection
        .plan(
            MdbaseWriteOperation::Create,
            vec![("b.md", Some(RECORD), None)],
        )
        .map_err(|error| error.to_string())?;
    ensure(
        collection.apply(&other, "same-key").is_err(),
        "a reused key with different input was accepted",
    )?;
    ensure(
        collection.read("b.md").is_none(),
        "a rejected reuse wrote a file",
    )
}

fn limits_gate() -> Result<(), String> {
    let collection = Collection::new("")?;
    let paths = (0..1_001)
        .map(|index| format!("r{index:04}.md"))
        .collect::<Vec<_>>();
    let plan = collection
        .plan(
            MdbaseWriteOperation::Batch,
            paths
                .iter()
                .map(|path| (path.as_str(), Some(RECORD), None))
                .collect(),
        )
        .map_err(|error| error.to_string())?;
    ensure(
        collection
            .apply(&plan, "too-large")
            .is_err_and(|error| error.code() == Some("limit_exceeded")),
        "an oversized batch was not rejected with limit_exceeded",
    )?;
    ensure(
        collection.read("r0000.md").is_none(),
        "an oversized batch changed files",
    )
}

fn recovery_gate() -> Result<(), String> {
    let collection = Collection::new("")?;
    collection.write(MdbaseWriteOperation::Create, "a.md", Some(RECORD))?;
    super::write_repair::interrupt_update_for_gate(
        &collection.paths,
        "a.md",
        "---\ntype: task\ntitle: Recovered\n---\nBody\n",
    )?;
    ensure(
        vulcan_core::mdbase::acquire_mdbase_consistent_read(&collection.paths).is_err(),
        "an unreconciled commit did not require recovery",
    )?;
    super::recover_mdbase_write(&collection.paths, None, false)
        .map_err(|error| error.to_string())?;
    let read = build_mdbase_read_report(&collection.paths, "a.md", false, None)
        .map_err(|error| error.to_string())?;
    ensure(
        read.record.frontmatter["title"] == "Recovered",
        "recovery lost the commit",
    )
}

const PROVIDERS: &str = "lifecycle:\n  on_create:\n    set:\n      id: {ulid: true}\n      uuid: {uuid: true}\n      created: {now: true}\n      day: {today: true}\n      slug: {slugify: title}\n      copied: {copy: title}\n      literal: {literal: {nested: null}}\n";

fn providers_gate() -> Result<(), String> {
    let collection = Collection::new(PROVIDERS)?;
    let plan = collection
        .plan(
            MdbaseWriteOperation::Create,
            vec![(
                "a.md",
                Some("---\ntype: task\ntitle: Hello World\n---\n"),
                None,
            )],
        )
        .map_err(|error| error.to_string())?;
    let generated = &plan.preview.generated_values["a.md"];
    ensure(
        generated["id"]
            .as_str()
            .is_some_and(|id| ulid::Ulid::from_string(id).is_ok())
            && generated["uuid"]
                .as_str()
                .is_some_and(|uuid| uuid.len() == 36)
            && generated["slug"] == "hello-world"
            && generated["copied"] == "Hello World"
            && generated["literal"] == serde_json::json!({"nested": null})
            && generated["created"].is_string()
            && generated["day"].is_string(),
        "a provider produced an unexpected value",
    )?;
    let reviewed = plan.preview.changes[0].after.clone().unwrap_or_default();
    collection
        .apply(&plan, "create")
        .map_err(|error| error.to_string())?;
    ensure(
        collection.read("a.md") == Some(reviewed),
        "apply did not persist the reviewed bytes",
    )
}

fn guards_gate() -> Result<(), String> {
    let collection = Collection::new(
        "lifecycle:\n  on_update:\n    - if: 'old.status != status && status == \"done\"'\n      set:\n        completed: {literal: yes}\n",
    )?;
    collection.write(
        MdbaseWriteOperation::Create,
        "a.md",
        Some("---\ntype: task\ntitle: A\nstatus: open\n---\n"),
    )?;
    collection.write(
        MdbaseWriteOperation::Update,
        "a.md",
        Some("---\ntype: task\ntitle: A2\nstatus: open\n---\n"),
    )?;
    ensure(
        collection
            .read("a.md")
            .is_some_and(|source| !source.contains("completed")),
        "a false guard applied its assignment",
    )?;
    collection.write(
        MdbaseWriteOperation::Update,
        "a.md",
        Some("---\ntype: task\ntitle: A2\nstatus: done\n---\n"),
    )?;
    ensure(
        collection
            .read("a.md")
            .is_some_and(|source| source.contains("completed: yes")),
        "a true guard did not apply its assignment",
    )
}

fn events_gate() -> Result<(), String> {
    let collection =
        Collection::new("lifecycle:\n  on_update:\n    set:\n      stamp: {literal: touched}\n")?;
    collection.write(MdbaseWriteOperation::Create, "a.md", Some(RECORD))?;
    vulcan_core::scan_vault(&collection.paths, vulcan_core::ScanMode::Full)
        .map_err(|error| error.to_string())?;
    crate::browse::move_note_with_profile(&collection.paths, "a.md", "b.md", false, None)
        .map_err(|error| error.to_string())?;
    ensure(
        collection.read("b.md").as_deref() == Some(RECORD),
        "a rename ran an update policy",
    )?;
    let unsupported =
        Collection::new("lifecycle:\n  on_delete:\n    set:\n      stamp: {literal: gone}\n")?;
    unsupported.write(MdbaseWriteOperation::Create, "a.md", Some(RECORD))?;
    ensure(
        unsupported
            .plan(MdbaseWriteOperation::Delete, vec![("a.md", None, None)])
            .is_err_and(|error| error.code() == Some("lifecycle_event_unsupported")),
        "an unsupported delete hook did not fail explicitly",
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_feature_gate_passes() {
        for feature in super::run_mdbase_feature_gates() {
            for case in &feature.cases {
                assert_eq!(
                    case.status,
                    crate::mdbase_conformance::MdbaseConformanceCaseStatus::Pass,
                    "{}: {:?}",
                    case.id,
                    case.message
                );
            }
            assert!(feature.passed, "{}", feature.feature);
        }
    }
}
