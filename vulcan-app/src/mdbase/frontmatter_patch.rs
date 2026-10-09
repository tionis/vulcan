//! The first writable App/script entrypoint (MDB.10 write pilot): a
//! revision-checked patch to one record's persisted frontmatter.
//!
//! The patch edits only the named keys in the exact source, then goes through
//! the shared mdbase planner and apply service like every other collection
//! write: authorization, schema and collection rules, lifecycle policies, the
//! fsynced journal, recovery, cache publication, and indexing. Effective
//! defaults are never written back unless the caller sets them explicitly.

use super::{
    apply_mdbase_write, load_collection_authorized, plan_mdbase_write, AppError,
    MdbaseWriteChangeRequest, MdbaseWriteExecutionOptions, MdbaseWriteOperation,
    MdbaseWritePlanRequest, VaultPaths,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;
use vulcan_core::mdbase::{is_mdbase_record_path, mdbase_content_revision};
use vulcan_core::paths::secure_read_to_string;
use vulcan_core::Verbosity;
use vulcan_core::{resolve_permission_profile, PermissionGuard, ProfilePermissionGuard};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MdbaseFrontmatterPatchRequest {
    /// Collection-relative record path.
    pub path: String,
    /// Required: the revision the caller read. A different current revision
    /// fails with `concurrent_modification` before anything is planned.
    pub if_revision: String,
    /// Fields to set, as JSON values.
    pub set: BTreeMap<String, serde_json::Value>,
    /// Fields to remove from persisted frontmatter.
    pub unset: Vec<String>,
    pub permission_profile: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MdbaseFrontmatterPatchOptions {
    pub dry_run: bool,
    pub no_commit: bool,
    pub verbosity: Verbosity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseFrontmatterPatchReport {
    pub path: String,
    pub dry_run: bool,
    /// False when the patch matched the persisted values already.
    pub changed: bool,
    pub revision_before: String,
    /// The persisted revision afterwards, including lifecycle-generated
    /// values; the planned revision for a dry run.
    pub revision: String,
}

pub fn patch_mdbase_frontmatter(
    paths: &VaultPaths,
    request: &MdbaseFrontmatterPatchRequest,
    options: &MdbaseFrontmatterPatchOptions,
) -> Result<MdbaseFrontmatterPatchReport, AppError> {
    validate_request(request)?;
    let (root, before) = read_authorized_source(paths, request)?;
    let revision_before = mdbase_content_revision(&before);
    if revision_before != request.if_revision {
        return Err(AppError::operation_with_code(
            "concurrent_modification",
            "the mdbase record no longer matches if_revision; read the current record and retry",
        ));
    }
    let changes = request
        .set
        .iter()
        .map(|(key, value)| {
            serde_yaml::to_value(value)
                .map(|value| (key.clone(), Some(value)))
                .map_err(AppError::operation)
        })
        .chain(request.unset.iter().map(|key| Ok((key.clone(), None))))
        .collect::<Result<Vec<_>, _>>()?;
    let after = vulcan_core::refactor::patch_frontmatter_source(&before, &request.path, &changes)
        .map_err(AppError::operation)?;
    if after == before {
        return Ok(MdbaseFrontmatterPatchReport {
            path: request.path.clone(),
            dry_run: options.dry_run,
            changed: false,
            revision: revision_before.clone(),
            revision_before,
        });
    }
    let now = chrono::DateTime::<chrono::Utc>::from(SystemTime::now());
    let plan = plan_mdbase_write(
        paths,
        &MdbaseWritePlanRequest {
            caller_id: "vulcan-frontmatter-patch".to_string(),
            instance_id: "vulcan-frontmatter-patch".to_string(),
            operation: MdbaseWriteOperation::Update,
            changes: vec![MdbaseWriteChangeRequest {
                path: request.path.clone(),
                after: Some(after.clone()),
                if_revision: Some(request.if_revision.clone()),
            }],
            matched_types: Vec::new(),
            generated_values: BTreeMap::new(),
            permission_profile: request.permission_profile.clone(),
            ttl_seconds: None,
        },
        now,
    )?;
    if options.dry_run {
        let planned = plan
            .preview
            .changes
            .first()
            .and_then(|change| change.after.as_deref())
            .map_or_else(|| mdbase_content_revision(&after), mdbase_content_revision);
        return Ok(MdbaseFrontmatterPatchReport {
            path: request.path.clone(),
            dry_run: true,
            changed: true,
            revision_before,
            revision: planned,
        });
    }
    apply_mdbase_write(
        paths,
        &plan,
        &MdbaseWriteExecutionOptions {
            idempotency_key: ulid::Ulid::new().to_string(),
            no_commit: options.no_commit,
            verbosity: options.verbosity,
        },
        now,
    )?;
    let persisted =
        secure_read_to_string(&root, Path::new(&request.path)).map_err(AppError::operation)?;
    Ok(MdbaseFrontmatterPatchReport {
        path: request.path.clone(),
        dry_run: false,
        changed: true,
        revision_before,
        revision: mdbase_content_revision(&persisted),
    })
}

fn validate_request(request: &MdbaseFrontmatterPatchRequest) -> Result<(), AppError> {
    if request.set.is_empty() && request.unset.is_empty() {
        return Err(AppError::operation_with_code(
            "invalid_request",
            "a frontmatter patch needs at least one field to set or unset",
        ));
    }
    if let Some(key) = request.set.keys().find(|key| request.unset.contains(key)) {
        return Err(AppError::operation_with_code(
            "invalid_request",
            format!("`{key}` is both set and unset"),
        ));
    }
    Ok(())
}

/// The exact source of a governed record the caller may read; anything else
/// is indistinguishable from a missing record. The planner checks the write.
fn read_authorized_source(
    paths: &VaultPaths,
    request: &MdbaseFrontmatterPatchRequest,
) -> Result<(std::path::PathBuf, String), AppError> {
    let guard = ProfilePermissionGuard::new(
        paths,
        resolve_permission_profile(paths, request.permission_profile.as_deref())
            .map_err(AppError::operation)?,
    );
    let root = {
        let loaded = load_collection_authorized(paths, Some(&guard.read_filter()))?;
        let governed = is_mdbase_record_path(&loaded.collection, &request.path)
            .map_err(AppError::operation)?;
        if !governed || guard.check_read_path(&request.path).is_err() {
            return Err(not_found(&request.path));
        }
        loaded.collection.root.clone()
    };
    let before = secure_read_to_string(&root, Path::new(&request.path))
        .map_err(|_| not_found(&request.path))?;
    Ok((root, before))
}

fn not_found(path: &str) -> AppError {
    AppError::operation_with_code(
        "record_not_found",
        format!("no readable mdbase record has path `{path}`"),
    )
}

#[cfg(test)]
mod tests;
