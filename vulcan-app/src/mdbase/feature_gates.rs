//! Native contract gates for the Vulcan-owned features
//! `vulcan.record_write.v1`, `vulcan.lifecycle.v1`, and
//! `vulcan.saved_views.v1` (`docs/specs/mdb/IMPLEMENTATION_CONTRACTS.md`).
//! Each gate drives the shared App pipeline in a fresh temporary collection; a
//! feature is claimed only when every one of its gates passes. These are
//! feature claims in Vulcan's namespace, not upstream profiles.

use super::{
    apply_mdbase_write, build_mdbase_read_report, build_mdbase_view_list_report,
    build_mdbase_view_report, create_mdbase_view_source, delete_mdbase_view_source,
    plan_mdbase_write, read_mdbase_view_source, update_mdbase_view_source, MdbaseViewSourceOptions,
    MdbaseWriteApplyReport, MdbaseWriteChangeRequest, MdbaseWriteExecutionOptions,
    MdbaseWriteOperation, MdbaseWritePlanReport, MdbaseWritePlanRequest,
};
use crate::mdbase_conformance::{MdbaseConformanceCaseResult, MdbaseConformanceCaseStatus};
use crate::AppError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use vulcan_core::mdbase::{MdbaseViewContextArg, MdbaseViewInvocation};
use vulcan_core::permissions::{PathPermission, ResourceSpecifier};
use vulcan_core::Verbosity;
use vulcan_core::{PermissionFilter, VaultPaths};

pub const RECORD_WRITE_FEATURE: &str = "vulcan.record_write.v1";
pub const LIFECYCLE_FEATURE: &str = "vulcan.lifecycle.v1";
pub const SAVED_VIEWS_FEATURE: &str = "vulcan.saved_views.v1";
/// The native gate that is the evidence for the upstream optional feature
/// `writable_view_sources`, which has no pinned upstream suite.
pub const WRITABLE_VIEW_SOURCES_GATE: &str = "vulcan.saved_views.sources";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseFeatureResult {
    pub feature: String,
    /// Always `vulcan`: these names never denote upstream profiles.
    pub namespace: String,
    pub passed: bool,
    pub cases: Vec<MdbaseConformanceCaseResult>,
}

type Gate = fn() -> Result<(), String>;

/// Run every native feature gate. `upstream` is the pinned fixture evidence:
/// saved views also require the upstream `view_records` suite to pass.
#[must_use]
pub fn run_mdbase_feature_gates(
    upstream: &[MdbaseConformanceCaseResult],
) -> Vec<MdbaseFeatureResult> {
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
    let saved_views: [(&str, &str, Gate); 4] = [
        (
            "vulcan.saved_views.scoping",
            "listing and execution see only the caller's sources, contexts, and candidates",
            saved_view_scoping_gate,
        ),
        (
            "vulcan.saved_views.freshness",
            "source and record edits reach the next listing and execution",
            saved_view_freshness_gate,
        ),
        (
            WRITABLE_VIEW_SOURCES_GATE,
            "complete-document sources are created without replacement, updated and deleted under if_revision, and visible to the next listing",
            saved_view_sources_gate,
        ),
        (
            "vulcan.saved_views.headless",
            "unknown presentations run headlessly; rendering is reported unsupported",
            saved_view_headless_gate,
        ),
    ];
    let mut results = [
        (RECORD_WRITE_FEATURE, &record_write[..]),
        (LIFECYCLE_FEATURE, &lifecycle[..]),
        (SAVED_VIEWS_FEATURE, &saved_views[..]),
    ]
    .into_iter()
    .map(|(feature, gates)| run_feature(feature, gates))
    .collect::<Vec<_>>();
    attach_upstream_view_evidence(&mut results, upstream);
    results
}

/// Saved views also require the pinned upstream saved-view suite.
fn attach_upstream_view_evidence(
    results: &mut [MdbaseFeatureResult],
    upstream: &[MdbaseConformanceCaseResult],
) {
    let upstream_views = crate::mdbase_conformance::optional_feature_passed(
        upstream,
        crate::mdbase_conformance::MDBASE_VIEW_RECORDS_FEATURE,
    );
    if let Some(views) = results
        .iter_mut()
        .find(|result| result.feature == SAVED_VIEWS_FEATURE)
    {
        views.cases.push(MdbaseConformanceCaseResult {
            id: "vulcan.saved_views.upstream".to_string(),
            name: "the pinned upstream view_records suite passes".to_string(),
            fixture_set: "views".to_string(),
            operation: "execute_view".to_string(),
            covers: vec![SAVED_VIEWS_FEATURE.to_string()],
            status: if upstream_views {
                MdbaseConformanceCaseStatus::Pass
            } else {
                MdbaseConformanceCaseStatus::Fail
            },
            message: (!upstream_views)
                .then(|| "upstream view_records evidence is missing or failing".to_string()),
        });
        views.passed &= upstream_views;
    }
}

fn run_feature(feature: &str, gates: &[(&str, &str, Gate)]) -> MdbaseFeatureResult {
    let (fixture_set, operation) = if feature == SAVED_VIEWS_FEATURE {
        ("vulcan-view-gates", "view")
    } else {
        ("vulcan-write-gates", "write")
    };
    let cases = gates
        .iter()
        .map(|(id, name, gate)| {
            let result = gate();
            MdbaseConformanceCaseResult {
                id: (*id).to_string(),
                name: (*name).to_string(),
                fixture_set: fixture_set.to_string(),
                operation: operation.to_string(),
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
                verbosity: Verbosity::Quiet,
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

const VIEW_TYPE: &str = "---\nkind: mdbase.type\nname: view\nmatch:\n  where:\n    type: view\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n---\n";

/// A view record whose `context` view needs a task context; the `all` view
/// lists task titles; presentation names a renderer Vulcan does not ship.
const VIEW_RECORD: &str = "---\ntype: view\nid: tasks\nversion: 1\nname: Tasks\nquery:\n  types: [task]\nviews:\n  - id: all\n    name: All\n    select: [title]\n    order_by:\n      - field: title\n    presentation:\n      type: example.unknown-renderer\n  - id: context\n    name: Context\n    context:\n      this:\n        on_missing: error\n    where: 'title == this.title'\n    select: [title]\n---\n";

impl Collection {
    fn with_views() -> Result<Self, String> {
        let collection = Self::new("")?;
        let root = collection.directory.path();
        for (path, contents) in [
            ("_types/view.md", VIEW_TYPE),
            ("views/tasks.md", VIEW_RECORD),
            ("open/a.md", RECORD),
            ("hidden/b.md", "---\ntype: task\ntitle: Beta\n---\n"),
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().expect("fixture path has a parent"))
                .and_then(|()| fs::write(path, contents))
                .map_err(|error| error.to_string())?;
        }
        Ok(collection)
    }

    fn view(
        &self,
        view: &str,
        context: MdbaseViewContextArg,
        render: bool,
        filter: Option<&PermissionFilter>,
    ) -> Result<Vec<String>, AppError> {
        build_mdbase_view_report(
            &self.paths,
            &MdbaseViewInvocation {
                source: "tasks".to_string(),
                view: view.to_string(),
                context,
                render,
                ..MdbaseViewInvocation::default()
            },
            filter,
        )
        .map(|report| {
            report
                .results
                .iter()
                .filter_map(|row| row.values.as_ref()?["title"].as_str().map(str::to_string))
                .collect()
        })
    }
}

fn hide_folder(folder: &str) -> PermissionFilter {
    PermissionFilter::new(PathPermission {
        allow: vec![ResourceSpecifier::Folder("**".to_string())],
        deny: vec![ResourceSpecifier::Folder(format!("{folder}/**"))],
    })
}

fn saved_view_scoping_gate() -> Result<(), String> {
    let collection = Collection::with_views()?;
    let all = |filter| {
        collection
            .view("all", MdbaseViewContextArg::Absent, false, filter)
            .map_err(|error| error.to_string())
    };
    ensure(
        all(None)? == ["Alpha", "Beta"],
        "unrestricted view lost records",
    )?;
    let hidden = hide_folder("hidden");
    ensure(
        all(Some(&hidden))? == ["Alpha"],
        "a view returned a hidden candidate",
    )?;
    ensure(
        collection
            .view(
                "context",
                MdbaseViewContextArg::Path("hidden/b.md".to_string()),
                false,
                Some(&hidden),
            )
            .is_err_and(|error| error.code() == Some("context_not_found")),
        "a hidden context record was bound",
    )?;
    let no_views = hide_folder("views");
    ensure(
        build_mdbase_view_list_report(&collection.paths, Some(&no_views))
            .map_err(|error| error.to_string())?
            .views
            .is_empty(),
        "a hidden view source was listed",
    )?;
    ensure(
        all(Some(&no_views)).is_err_and(|error| error.contains("no view record")),
        "a hidden view source executed",
    )
}

fn saved_view_freshness_gate() -> Result<(), String> {
    let collection = Collection::with_views()?;
    let titles = || {
        collection
            .view("all", MdbaseViewContextArg::Absent, false, None)
            .map_err(|error| error.to_string())
    };
    titles()?;
    collection.write(
        MdbaseWriteOperation::Update,
        "open/a.md",
        Some("---\ntype: task\ntitle: Gamma\n---\nBody\n"),
    )?;
    ensure(
        titles()? == ["Beta", "Gamma"],
        "a record edit was not visible to the next execution",
    )?;
    fs::write(
        collection.directory.path().join("views/tasks.md"),
        VIEW_RECORD.replace("name: All", "name: Everything"),
    )
    .map_err(|error| error.to_string())?;
    let list = build_mdbase_view_list_report(&collection.paths, None)
        .map_err(|error| error.to_string())?;
    ensure(
        list.views
            .first()
            .and_then(|source| source.views.first())
            .is_some_and(|view| view.name == "Everything"),
        "a view source edit was not visible to the next listing",
    )
}

fn saved_view_sources_gate() -> Result<(), String> {
    let collection = Collection::with_views()?;
    let paths = &collection.paths;
    let options = MdbaseViewSourceOptions {
        no_commit: true,
        verbosity: Verbosity::Quiet,
        ..MdbaseViewSourceOptions::default()
    };
    let error_code = |result: Result<(), AppError>| {
        result
            .err()
            .and_then(|error| error.code().map(str::to_string))
    };
    let extension = VIEW_RECORD.replace("views:\n", "x-example:\n  kept: true\nviews:\n");
    let created = create_mdbase_view_source(paths, Some("views/more.md"), &extension, &options)
        .map_err(|error| error.to_string())?;
    ensure(
        collection.read("views/more.md").as_deref() == Some(extension.as_str()),
        "a created source was not persisted byte for byte",
    )?;
    ensure(
        error_code(
            create_mdbase_view_source(paths, Some("views/more.md"), VIEW_RECORD, &options)
                .map(|_| ()),
        )
        .as_deref()
            == Some("path_conflict"),
        "creation replaced an existing source",
    )?;
    ensure(
        error_code(
            create_mdbase_view_source(
                paths,
                Some("views/bad.md"),
                &VIEW_RECORD.replace("id: context", "id: all"),
                &options,
            )
            .map(|_| ()),
        )
        .as_deref()
            == Some("invalid_view")
            && collection.read("views/bad.md").is_none(),
        "an invalid document was written",
    )?;
    let renamed = extension.replace("name: All", "name: Renamed");
    ensure(
        error_code(
            update_mdbase_view_source(
                paths,
                "views/more.md",
                &renamed,
                Some("sha256:stale"),
                &options,
            )
            .map(|_| ()),
        )
        .as_deref()
            == Some("concurrent_modification"),
        "an update ignored if_revision",
    )?;
    update_mdbase_view_source(
        paths,
        "views/more.md",
        &renamed,
        Some(&created.revision),
        &options,
    )
    .map_err(|error| error.to_string())?;
    let listed = build_mdbase_view_list_report(paths, None)
        .map_err(|error| error.to_string())?
        .views
        .into_iter()
        .any(|source| source.source.path == "views/more.md" && source.views[0].name == "Renamed");
    ensure(listed, "an update was not visible to the next listing")?;
    let current =
        read_mdbase_view_source(paths, "views/more.md", None).map_err(|error| error.to_string())?;
    ensure(
        error_code(
            delete_mdbase_view_source(paths, "views/more.md", Some(&created.revision), &options)
                .map(|_| ()),
        )
        .as_deref()
            == Some("concurrent_modification"),
        "a delete ignored if_revision",
    )?;
    delete_mdbase_view_source(paths, "views/more.md", Some(&current.revision), &options)
        .map_err(|error| error.to_string())?;
    ensure(
        collection.read("views/more.md").is_none(),
        "a deleted source remained",
    )
}

fn saved_view_headless_gate() -> Result<(), String> {
    let collection = Collection::with_views()?;
    ensure(
        collection
            .view("all", MdbaseViewContextArg::Absent, false, None)
            .is_ok_and(|titles| titles.len() == 2),
        "an unknown presentation blocked headless execution",
    )?;
    ensure(
        collection
            .view("all", MdbaseViewContextArg::Absent, true, None)
            .is_err_and(|error| error.code() == Some("unsupported_presentation")),
        "a rendered request was not reported unsupported",
    )?;
    ensure(
        collection
            .view("context", MdbaseViewContextArg::Absent, false, None)
            .is_err_and(|error| error.code() == Some("context_required")),
        "a required context was not enforced",
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_feature_gate_passes() {
        let upstream = crate::mdbase_conformance::run_mdbase_core_read_conformance()
            .expect("conformance runs")
            .cases;
        for feature in super::run_mdbase_feature_gates(&upstream) {
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

    #[test]
    fn saved_views_require_upstream_view_evidence() {
        let features = super::run_mdbase_feature_gates(&[]);
        let views = features
            .iter()
            .find(|feature| feature.feature == super::SAVED_VIEWS_FEATURE)
            .expect("saved views are gated");
        assert!(!views.passed);
        assert!(features
            .iter()
            .filter(|feature| feature.feature != super::SAVED_VIEWS_FEATURE)
            .all(|feature| feature.passed));
    }
}
