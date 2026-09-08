use super::{
    discover_mdbase_files, mdbase_control_revisions, mdbase_glob, MdbaseCollection,
    MdbaseControlRevisions,
};
use crate::paths::secure_read_to_string;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::io::ErrorKind;
use std::path::Path;

pub const MDBASE_WRITE_PREVIEW_VERSION: u32 = 1;
pub const MDBASE_VALIDATION_POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseWritePreviewRequest {
    pub plan_id: String,
    pub caller_id: String,
    pub instance_id: String,
    pub operation: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub permission_revision: String,
    pub config_revision: String,
    pub changes: Vec<MdbaseWritePreviewChangeRequest>,
    /// Collection-relative record namespaces authorized for global validation.
    pub relevant_record_namespaces: Vec<String>,
    /// Values produced once by lifecycle providers during planning.
    pub generated_values: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseWritePreviewChangeRequest {
    pub path: String,
    /// Exact proposed bytes, or `None` for deletion.
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdbaseWritePreviewVerification<'a> {
    pub caller_id: &'a str,
    pub instance_id: &'a str,
    pub operation: &'a str,
    pub permission_revision: &'a str,
    pub config_revision: &'a str,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePreviewChange {
    pub path: String,
    /// Exact bytes observed while planning, or `None` for an absent path.
    pub before: Option<String>,
    /// Exact reviewed bytes to persist, or `None` for deletion.
    pub after: Option<String>,
    pub before_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseDirectoryMembership {
    pub namespace: String,
    pub paths: Vec<String>,
    pub digest: String,
}

/// Immutable preview state consumed by the future journaled apply workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePreview {
    pub version: u32,
    pub validation_policy_version: u32,
    pub plan_id: String,
    pub caller_id: String,
    pub instance_id: String,
    pub operation: String,
    pub issued_at: String,
    pub expires_at: String,
    pub collection_root: String,
    pub control_revisions: MdbaseControlRevisions,
    pub permission_revision: String,
    pub config_revision: String,
    pub changes: Vec<MdbaseWritePreviewChange>,
    pub absence_preconditions: Vec<String>,
    pub accepted_revisions: BTreeMap<String, String>,
    pub directory_memberships: Vec<MdbaseDirectoryMembership>,
    pub generated_values: BTreeMap<String, serde_json::Value>,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePreviewError {
    pub code: String,
    pub message: String,
}

impl MdbaseWritePreviewError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    fn stale() -> Self {
        Self::new(
            "stale_state",
            "mdbase write preview is stale; create and review a new preview",
        )
    }
}

impl Display for MdbaseWritePreviewError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MdbaseWritePreviewError {}

/// Capture exact write bytes and every current dependency needed to recheck a
/// preview before persistence. This function does not mutate the collection or
/// write derived cache state.
pub fn build_mdbase_write_preview(
    collection: &MdbaseCollection,
    request: MdbaseWritePreviewRequest,
) -> Result<MdbaseWritePreview, MdbaseWritePreviewError> {
    validate_request(&request)?;
    let collection_root = canonical_collection_root(collection)?;
    let control_revisions = mdbase_control_revisions(collection).map_err(|error| {
        MdbaseWritePreviewError::new("preview_dependency_error", error.to_string())
    })?;
    let mut seen = BTreeSet::new();
    let mut changes = Vec::with_capacity(request.changes.len());
    let mut absence_preconditions = Vec::new();
    for requested in request.changes {
        let path = normalize_relative_path(&requested.path)?;
        if !seen.insert(path.clone()) {
            return Err(MdbaseWritePreviewError::new(
                "preview_invalid",
                "mdbase write preview contains a duplicate changed path",
            ));
        }
        let before = read_optional_source(collection, &path)?;
        let before_revision = before.as_deref().map(content_revision);
        if before.is_none() {
            absence_preconditions.push(path.clone());
        }
        changes.push(MdbaseWritePreviewChange {
            path,
            before,
            after: requested.after,
            before_revision,
        });
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    absence_preconditions.sort();

    let namespaces = normalize_namespaces(&request.relevant_record_namespaces)?;
    let (directory_memberships, mut accepted_revisions) =
        snapshot_record_scope(collection, &namespaces)?;
    for change in &changes {
        if let Some(revision) = &change.before_revision {
            accepted_revisions.insert(change.path.clone(), revision.clone());
        }
    }

    let mut preview = MdbaseWritePreview {
        version: MDBASE_WRITE_PREVIEW_VERSION,
        validation_policy_version: MDBASE_VALIDATION_POLICY_VERSION,
        plan_id: request.plan_id,
        caller_id: request.caller_id,
        instance_id: request.instance_id,
        operation: request.operation,
        issued_at: timestamp(request.issued_at),
        expires_at: timestamp(request.expires_at),
        collection_root,
        control_revisions,
        permission_revision: request.permission_revision,
        config_revision: request.config_revision,
        changes,
        absence_preconditions,
        accepted_revisions,
        directory_memberships,
        generated_values: request.generated_values,
        digest: String::new(),
    };
    preview.digest = preview_digest(&preview)?;
    Ok(preview)
}

/// Recheck a preview immediately before a journaled apply begins.
///
/// Any control, permission, configuration, source, absence, membership, or
/// relevant-record revision drift returns the same non-oracular `stale_state`.
pub fn verify_mdbase_write_preview(
    collection: &MdbaseCollection,
    preview: &MdbaseWritePreview,
    verification: &MdbaseWritePreviewVerification<'_>,
) -> Result<(), MdbaseWritePreviewError> {
    if preview.version != MDBASE_WRITE_PREVIEW_VERSION
        || preview.validation_policy_version != MDBASE_VALIDATION_POLICY_VERSION
        || preview.digest != preview_digest(preview)?
    {
        return Err(MdbaseWritePreviewError::new(
            "preview_invalid",
            "mdbase write preview failed its integrity check",
        ));
    }
    if preview.caller_id != verification.caller_id
        || preview.instance_id != verification.instance_id
        || preview.operation != verification.operation
    {
        return Err(MdbaseWritePreviewError::new(
            "permission_denied",
            "mdbase write preview is not bound to this caller, instance, and operation",
        ));
    }
    let expires_at = DateTime::parse_from_rfc3339(&preview.expires_at)
        .map_err(|_| MdbaseWritePreviewError::new("preview_invalid", "invalid preview expiry"))?
        .with_timezone(&Utc);
    if verification.now >= expires_at {
        return Err(MdbaseWritePreviewError::new(
            "preview_expired",
            "mdbase write preview has expired; create and review a new preview",
        ));
    }
    if preview.permission_revision != verification.permission_revision
        || preview.config_revision != verification.config_revision
        || preview.collection_root
            != canonical_collection_root(collection)
                .map_err(|_| MdbaseWritePreviewError::stale())?
        || preview.control_revisions
            != mdbase_control_revisions(collection).map_err(|_| MdbaseWritePreviewError::stale())?
    {
        return Err(MdbaseWritePreviewError::stale());
    }
    for change in &preview.changes {
        if read_optional_source(collection, &change.path)
            .map_err(|_| MdbaseWritePreviewError::stale())?
            != change.before
        {
            return Err(MdbaseWritePreviewError::stale());
        }
    }
    let namespaces = preview
        .directory_memberships
        .iter()
        .map(|membership| membership.namespace.clone())
        .collect::<Vec<_>>();
    let (memberships, mut revisions) = snapshot_record_scope(collection, &namespaces)
        .map_err(|_| MdbaseWritePreviewError::stale())?;
    for change in &preview.changes {
        if let Some(revision) = &change.before_revision {
            revisions.insert(change.path.clone(), revision.clone());
        }
    }
    if memberships != preview.directory_memberships || revisions != preview.accepted_revisions {
        return Err(MdbaseWritePreviewError::stale());
    }
    Ok(())
}

fn validate_request(request: &MdbaseWritePreviewRequest) -> Result<(), MdbaseWritePreviewError> {
    for (field, value) in [
        ("plan_id", request.plan_id.as_str()),
        ("caller_id", request.caller_id.as_str()),
        ("instance_id", request.instance_id.as_str()),
        ("operation", request.operation.as_str()),
        ("permission_revision", request.permission_revision.as_str()),
        ("config_revision", request.config_revision.as_str()),
    ] {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(MdbaseWritePreviewError::new(
                "preview_invalid",
                format!("mdbase write preview has an invalid {field}"),
            ));
        }
    }
    if request.expires_at <= request.issued_at {
        return Err(MdbaseWritePreviewError::new(
            "preview_invalid",
            "mdbase write preview expiry must be after its issue time",
        ));
    }
    if request.changes.is_empty() {
        return Err(MdbaseWritePreviewError::new(
            "preview_invalid",
            "mdbase write preview must contain at least one changed path",
        ));
    }
    Ok(())
}

fn snapshot_record_scope(
    collection: &MdbaseCollection,
    namespaces: &[String],
) -> Result<(Vec<MdbaseDirectoryMembership>, BTreeMap<String, String>), MdbaseWritePreviewError> {
    let discovery = discover_mdbase_files(collection).map_err(|error| {
        MdbaseWritePreviewError::new("preview_dependency_error", error.to_string())
    })?;
    let mut memberships = Vec::new();
    let mut accepted_revisions = BTreeMap::new();
    for namespace in namespaces {
        let matcher = mdbase_glob(namespace)
            .map_err(|_| {
                MdbaseWritePreviewError::new("preview_invalid", "invalid record namespace")
            })?
            .compile_matcher();
        let paths = discovery
            .records
            .iter()
            .filter(|path| matcher.is_match(path))
            .cloned()
            .collect::<Vec<_>>();
        for path in &paths {
            let source = read_optional_source(collection, path)?.ok_or_else(|| {
                MdbaseWritePreviewError::new(
                    "preview_dependency_error",
                    "a discovered mdbase record disappeared while planning",
                )
            })?;
            accepted_revisions.insert(path.clone(), content_revision(&source));
        }
        memberships.push(MdbaseDirectoryMembership {
            namespace: namespace.clone(),
            digest: membership_digest(namespace, &paths),
            paths,
        });
    }
    Ok((memberships, accepted_revisions))
}

fn normalize_namespaces(namespaces: &[String]) -> Result<Vec<String>, MdbaseWritePreviewError> {
    let mut normalized = BTreeSet::new();
    for namespace in namespaces {
        let namespace = normalize_relative_pattern(namespace)?;
        mdbase_glob(&namespace).map_err(|_| {
            MdbaseWritePreviewError::new("preview_invalid", "invalid record namespace")
        })?;
        normalized.insert(namespace);
    }
    Ok(normalized.into_iter().collect())
}

fn normalize_relative_path(path: &str) -> Result<String, MdbaseWritePreviewError> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(MdbaseWritePreviewError::new(
            "preview_invalid",
            "mdbase write preview contains an unsafe changed path",
        ));
    }
    Ok(normalized)
}

fn normalize_relative_pattern(pattern: &str) -> Result<String, MdbaseWritePreviewError> {
    let normalized = pattern.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(MdbaseWritePreviewError::new(
            "preview_invalid",
            "mdbase write preview contains an unsafe record namespace",
        ));
    }
    Ok(normalized)
}

fn read_optional_source(
    collection: &MdbaseCollection,
    path: &str,
) -> Result<Option<String>, MdbaseWritePreviewError> {
    match secure_read_to_string(&collection.root, Path::new(path)) {
        Ok(source) => Ok(Some(source)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(MdbaseWritePreviewError::new(
            "preview_dependency_error",
            format!("failed to read mdbase preview dependency: {error}"),
        )),
    }
}

fn canonical_collection_root(
    collection: &MdbaseCollection,
) -> Result<String, MdbaseWritePreviewError> {
    std::fs::canonicalize(&collection.root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|error| {
            MdbaseWritePreviewError::new(
                "preview_dependency_error",
                format!("failed to resolve mdbase collection root: {error}"),
            )
        })
}

fn content_revision(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}

fn membership_digest(namespace: &str, paths: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(namespace.as_bytes());
    for path in paths {
        digest.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(path.as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

fn preview_digest(preview: &MdbaseWritePreview) -> Result<String, MdbaseWritePreviewError> {
    let mut payload = preview.clone();
    payload.digest.clear();
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        MdbaseWritePreviewError::new(
            "preview_invalid",
            format!("failed to serialize mdbase write preview: {error}"),
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::load_mdbase_collection;
    use chrono::Duration;
    use std::fs;
    use tempfile::tempdir;

    fn write(root: &Path, path: &str, source: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture directory");
        fs::write(path, source).expect("fixture write");
    }

    fn fixture() -> (tempfile::TempDir, MdbaseCollection) {
        let directory = tempdir().expect("temp directory");
        write(directory.path(), "mdbase.yaml", "spec_version: 0.3.0\n");
        write(directory.path(), "_types/task.md", "type v1\n");
        write(directory.path(), "_contracts/task.md", "contract v1\n");
        write(directory.path(), "schemas/task.json", "{}\n");
        write(directory.path(), "tasks/a.md", "---\ntype: task\n---\na\n");
        write(directory.path(), "other/b.md", "---\ntype: other\n---\nb\n");
        let collection = load_mdbase_collection(directory.path())
            .expect("collection loads")
            .expect("collection exists");
        (directory, collection)
    }

    fn request(now: DateTime<Utc>) -> MdbaseWritePreviewRequest {
        MdbaseWritePreviewRequest {
            plan_id: "01k4mk7j28dawhqbf8jvx4q2p8".to_string(),
            caller_id: "caller".to_string(),
            instance_id: "instance".to_string(),
            operation: "update".to_string(),
            issued_at: now,
            expires_at: now + Duration::minutes(5),
            permission_revision: "grant:v1".to_string(),
            config_revision: "config:v1".to_string(),
            changes: vec![MdbaseWritePreviewChangeRequest {
                path: "tasks/a.md".to_string(),
                after: Some("---\ntype: task\n---\nupdated\n".to_string()),
            }],
            relevant_record_namespaces: vec!["tasks/**".to_string()],
            generated_values: BTreeMap::from([
                ("now".to_string(), serde_json::json!("2026-09-08T12:00:00Z")),
                ("uuid".to_string(), serde_json::json!("fixed-uuid")),
            ]),
        }
    }

    fn verify(
        collection: &MdbaseCollection,
        preview: &MdbaseWritePreview,
        now: DateTime<Utc>,
    ) -> Result<(), MdbaseWritePreviewError> {
        verify_mdbase_write_preview(
            collection,
            preview,
            &MdbaseWritePreviewVerification {
                caller_id: "caller",
                instance_id: "instance",
                operation: "update",
                permission_revision: "grant:v1",
                config_revision: "config:v1",
                now,
            },
        )
    }

    #[test]
    fn preview_binds_exact_bytes_revisions_membership_and_generated_values() {
        let (_directory, collection) = fixture();
        let now = "2026-09-08T12:00:00Z".parse().expect("time");
        let preview = build_mdbase_write_preview(&collection, request(now)).expect("preview");
        assert_eq!(
            preview.changes[0].before.as_deref(),
            Some("---\ntype: task\n---\na\n")
        );
        assert_eq!(preview.directory_memberships[0].paths, ["tasks/a.md"]);
        assert!(preview.accepted_revisions.contains_key("tasks/a.md"));
        assert_eq!(preview.generated_values["uuid"], "fixed-uuid");
        assert!(preview.digest.starts_with("sha256:"));
        verify(&collection, &preview, now).expect("unchanged preview verifies");
    }

    #[test]
    fn control_classes_permission_and_config_drift_are_stale() {
        let cases = [
            ("mdbase.yaml", "spec_version: 0.3.0\n# changed\n"),
            ("_types/task.md", "type v2\n"),
            ("_contracts/task.md", "contract v2\n"),
            ("schemas/task.json", "{\"type\":\"object\"}\n"),
        ];
        let now = "2026-09-08T12:00:00Z".parse().expect("time");
        for (path, source) in cases {
            let (directory, collection) = fixture();
            let preview = build_mdbase_write_preview(&collection, request(now)).expect("preview");
            write(directory.path(), path, source);
            assert_eq!(
                verify(&collection, &preview, now).unwrap_err().code,
                "stale_state"
            );
        }

        let (_directory, collection) = fixture();
        let preview = build_mdbase_write_preview(&collection, request(now)).expect("preview");
        assert_eq!(
            verify_mdbase_write_preview(
                &collection,
                &preview,
                &MdbaseWritePreviewVerification {
                    caller_id: "caller",
                    instance_id: "instance",
                    operation: "update",
                    permission_revision: "grant:v2",
                    config_revision: "config:v1",
                    now,
                },
            )
            .unwrap_err()
            .code,
            "stale_state"
        );
        assert_eq!(
            verify_mdbase_write_preview(
                &collection,
                &preview,
                &MdbaseWritePreviewVerification {
                    caller_id: "caller",
                    instance_id: "instance",
                    operation: "update",
                    permission_revision: "grant:v1",
                    config_revision: "config:v2",
                    now,
                },
            )
            .unwrap_err()
            .code,
            "stale_state"
        );
    }

    #[test]
    fn source_absence_and_phantom_membership_preconditions_fail_closed() {
        let (directory, collection) = fixture();
        let now = "2026-09-08T12:00:00Z".parse().expect("time");
        let preview = build_mdbase_write_preview(&collection, request(now)).expect("preview");
        write(directory.path(), "tasks/phantom.md", "phantom\n");
        assert_eq!(
            verify(&collection, &preview, now).unwrap_err().code,
            "stale_state"
        );

        let (directory, collection) = fixture();
        let mut create = request(now);
        create.changes[0] = MdbaseWritePreviewChangeRequest {
            path: "tasks/new.md".to_string(),
            after: Some("new\n".to_string()),
        };
        let preview = build_mdbase_write_preview(&collection, create).expect("create preview");
        assert_eq!(preview.absence_preconditions, ["tasks/new.md"]);
        write(directory.path(), "tasks/new.md", "external\n");
        assert_eq!(
            verify(&collection, &preview, now).unwrap_err().code,
            "stale_state"
        );
    }

    #[test]
    fn relevant_content_drift_invalidates_but_unrelated_content_drift_does_not() {
        let (directory, collection) = fixture();
        let now = "2026-09-08T12:00:00Z".parse().expect("time");
        let preview = build_mdbase_write_preview(&collection, request(now)).expect("preview");
        write(directory.path(), "other/b.md", "unrelated change\n");
        verify(&collection, &preview, now).expect("unrelated namespace is not bound");
        write(directory.path(), "tasks/a.md", "relevant change\n");
        assert_eq!(
            verify(&collection, &preview, now).unwrap_err().code,
            "stale_state"
        );
    }

    #[test]
    fn expiry_binding_and_integrity_are_checked_before_apply() {
        let (_directory, collection) = fixture();
        let now = "2026-09-08T12:00:00Z".parse().expect("time");
        let preview = build_mdbase_write_preview(&collection, request(now)).expect("preview");
        assert_eq!(
            verify_mdbase_write_preview(
                &collection,
                &preview,
                &MdbaseWritePreviewVerification {
                    caller_id: "other",
                    instance_id: "instance",
                    operation: "update",
                    permission_revision: "grant:v1",
                    config_revision: "config:v1",
                    now,
                },
            )
            .unwrap_err()
            .code,
            "permission_denied"
        );
        assert_eq!(
            verify(&collection, &preview, now + Duration::minutes(5))
                .unwrap_err()
                .code,
            "preview_expired"
        );

        let mut tampered = preview;
        tampered.generated_values.insert(
            "uuid".to_string(),
            serde_json::json!("silently-regenerated"),
        );
        assert_eq!(
            verify(&collection, &tampered, now).unwrap_err().code,
            "preview_invalid"
        );
    }
}
