//! Saved-view discovery and execution over the caller's authorized records.
//!
//! Both operations load one disk-verified, permission-filtered record
//! snapshot through the shared query path, so a view, its invocation context,
//! and its candidates come from the same visible collection state. Hidden view
//! sources are not listed and hidden context records are reported as not
//! found, exactly like hidden ordinary records.

use super::{
    apply_mdbase_write, plan_mdbase_write, MdbaseQueryMetrics, MdbaseWriteChangeRequest,
    MdbaseWriteExecutionOptions, MdbaseWriteOperation, MdbaseWritePlanRequest, VaultPaths,
};
use super::{load_collection_authorized, load_query_records, registry_diagnostics, AppError};
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::SystemTime;
use vulcan_core::mdbase::{
    compile_mdbase_prepared_query, execute_mdbase_view, is_mdbase_view_record, list_mdbase_views,
    mdbase_content_revision, validate_mdbase_view_source, MdbaseQueryError, MdbaseQueryResult,
    MdbaseQuerySnapshot, MdbaseViewInvocation, MdbaseViewList, MDBASE_VIEW_SOURCE_FORMAT,
};
use vulcan_core::paths::secure_read_to_string;
use vulcan_core::{
    resolve_permission_profile, PermissionFilter, PermissionGuard, ProfilePermissionGuard,
};

/// `list_views`: every visible saved-view source, in source-path order.
/// Malformed sources are omitted and reported as warnings.
pub fn build_mdbase_view_list_report(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseViewList, AppError> {
    let (loaded, records) = visible_records(paths, filter)?;
    let mut list = list_mdbase_views(records.records());
    list.diagnostics
        .splice(0..0, registry_diagnostics(&loaded, filter));
    Ok(list)
}

/// `execute_view`: resolve a named view, bind its context, and return the
/// canonical query envelope with `meta.view`.
pub fn build_mdbase_view_report(
    paths: &VaultPaths,
    invocation: &MdbaseViewInvocation,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseQueryResult, AppError> {
    let (loaded, records) = visible_records(paths, filter)?;
    let mut report = execute_mdbase_view(
        records.records(),
        &loaded.types,
        invocation,
        &loaded.collection.config.settings.id_field,
        loaded.collection.config.settings.timezone.as_deref(),
        chrono::DateTime::<chrono::Utc>::from(SystemTime::now()),
    )
    .map_err(view_error)?;
    report
        .diagnostics
        .splice(0..0, registry_diagnostics(&loaded, filter));
    Ok(report)
}

/// A complete saved-view source document (`read_view_source`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseViewSourceDocument {
    pub path: String,
    pub format: String,
    pub revision: String,
    pub document: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseViewSourceDeletion {
    pub path: String,
    pub deleted: bool,
    /// True when `dry_run` validated the deletion without applying it.
    pub dry_run: bool,
}

/// How a source operation runs. Writes go through the managed mdbase write
/// pipeline under this profile, so record validation, authorization,
/// recovery, and auto-commit behave as for any other record write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MdbaseViewSourceOptions {
    pub permission_profile: Option<String>,
    pub dry_run: bool,
    pub no_commit: bool,
    pub quiet: bool,
}

/// `read_view_source`: the exact source of one visible view record.
pub fn read_mdbase_view_source(
    paths: &VaultPaths,
    path: &str,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseViewSourceDocument, AppError> {
    let (loaded, records) = visible_records(paths, filter)?;
    let record = records
        .records()
        .get(path)
        .filter(|record| is_mdbase_view_record(record))
        .ok_or_else(|| view_source_not_found(path))?;
    let document = secure_read_to_string(&loaded.collection.root, std::path::Path::new(path))
        .map_err(AppError::operation)?;
    let revision = mdbase_content_revision(&document);
    if revision != record.revision {
        return Err(AppError::operation_with_code(
            "stale_state",
            "the view source changed while it was read; retry",
        ));
    }
    Ok(source_document(path, document))
}

/// `create_view_source`: validate a complete document and create it without
/// replacing anything. Without `path`, the source goes to `views/<id>.md`.
pub fn create_mdbase_view_source(
    paths: &VaultPaths,
    path: Option<&str>,
    document: &str,
    options: &MdbaseViewSourceOptions,
) -> Result<MdbaseViewSourceDocument, AppError> {
    let filter = read_filter(paths, options)?;
    // Validate under the collection read guard, then release it: the managed
    // write takes the exclusive lock.
    let (path, root) = {
        let loaded = load_collection_authorized(paths, Some(&filter))?;
        let path = if let Some(path) = path {
            path.to_string()
        } else {
            // The ID decides the default path; validate once to learn it.
            let id = validate_mdbase_view_source(
                &loaded.collection,
                &loaded.types,
                "views/new.md",
                document,
            )
            .map_err(view_error)?;
            default_view_path(&id)
        };
        validate_mdbase_view_source(&loaded.collection, &loaded.types, &path, document)
            .map_err(view_error)?;
        (path, loaded.collection.root.clone())
    };
    // Authorize the write first, so existence is never probed without it.
    let selection = resolve_permission_profile(paths, options.permission_profile.as_deref())
        .map_err(AppError::operation)?;
    super::authorize_affected_paths(
        &ProfilePermissionGuard::new(paths, selection),
        std::slice::from_ref(&path),
    )?;
    if root.join(&path).exists() {
        return Err(AppError::operation_with_code(
            "path_conflict",
            format!("a file already exists at `{path}`"),
        ));
    }
    let plan = plan_source_write(
        paths,
        MdbaseWriteOperation::Create,
        &path,
        Some(document),
        None,
        options,
    )?;
    apply_source_write(paths, &plan, options)?;
    Ok(source_document(&path, document.to_string()))
}

/// `update_view_source`: atomically replace a visible view source with a
/// complete, valid document, optionally only at `if_revision`.
pub fn update_mdbase_view_source(
    paths: &VaultPaths,
    path: &str,
    document: &str,
    if_revision: Option<&str>,
    options: &MdbaseViewSourceOptions,
) -> Result<MdbaseViewSourceDocument, AppError> {
    let filter = read_filter(paths, options)?;
    let current = read_mdbase_view_source(paths, path, Some(&filter))?;
    {
        let loaded = load_collection_authorized(paths, Some(&filter))?;
        validate_mdbase_view_source(&loaded.collection, &loaded.types, path, document)
            .map_err(view_error)?;
    }
    let plan = plan_source_write(
        paths,
        MdbaseWriteOperation::Update,
        path,
        Some(document),
        Some(if_revision.unwrap_or(&current.revision)),
        options,
    )?;
    apply_source_write(paths, &plan, options)?;
    Ok(source_document(path, document.to_string()))
}

/// `delete_view_source`: delete a visible view source, optionally only at
/// `if_revision`.
pub fn delete_mdbase_view_source(
    paths: &VaultPaths,
    path: &str,
    if_revision: Option<&str>,
    options: &MdbaseViewSourceOptions,
) -> Result<MdbaseViewSourceDeletion, AppError> {
    let filter = read_filter(paths, options)?;
    let current = read_mdbase_view_source(paths, path, Some(&filter))?;
    let plan = plan_source_write(
        paths,
        MdbaseWriteOperation::Delete,
        path,
        None,
        Some(if_revision.unwrap_or(&current.revision)),
        options,
    )?;
    apply_source_write(paths, &plan, options)?;
    Ok(MdbaseViewSourceDeletion {
        path: path.to_string(),
        deleted: !options.dry_run,
        dry_run: options.dry_run,
    })
}

fn read_filter(
    paths: &VaultPaths,
    options: &MdbaseViewSourceOptions,
) -> Result<PermissionFilter, AppError> {
    let selection = resolve_permission_profile(paths, options.permission_profile.as_deref())
        .map_err(AppError::operation)?;
    Ok(ProfilePermissionGuard::new(paths, selection).read_filter())
}

fn plan_source_write(
    paths: &VaultPaths,
    operation: MdbaseWriteOperation,
    path: &str,
    after: Option<&str>,
    if_revision: Option<&str>,
    options: &MdbaseViewSourceOptions,
) -> Result<super::MdbaseWritePlanReport, AppError> {
    plan_mdbase_write(
        paths,
        &MdbaseWritePlanRequest {
            caller_id: "vulcan-view-source".to_string(),
            instance_id: "vulcan-view-source".to_string(),
            operation,
            changes: vec![MdbaseWriteChangeRequest {
                path: path.to_string(),
                after: after.map(ToString::to_string),
                if_revision: if_revision.map(ToString::to_string),
            }],
            matched_types: Vec::new(),
            generated_values: BTreeMap::new(),
            permission_profile: options.permission_profile.clone(),
            ttl_seconds: None,
        },
        chrono::DateTime::<chrono::Utc>::from(SystemTime::now()),
    )
}

fn apply_source_write(
    paths: &VaultPaths,
    plan: &super::MdbaseWritePlanReport,
    options: &MdbaseViewSourceOptions,
) -> Result<(), AppError> {
    if options.dry_run {
        return Ok(());
    }
    apply_mdbase_write(
        paths,
        plan,
        &MdbaseWriteExecutionOptions {
            idempotency_key: ulid::Ulid::new().to_string(),
            no_commit: options.no_commit,
            quiet: options.quiet,
        },
        chrono::DateTime::<chrono::Utc>::from(SystemTime::now()),
    )
    .map(|_| ())
}

fn source_document(path: &str, document: String) -> MdbaseViewSourceDocument {
    MdbaseViewSourceDocument {
        path: path.to_string(),
        format: MDBASE_VIEW_SOURCE_FORMAT.to_string(),
        revision: mdbase_content_revision(&document),
        document,
    }
}

fn view_source_not_found(path: &str) -> AppError {
    AppError::operation_with_code(
        "view_not_found",
        format!("no view record has path `{path}`"),
    )
}

/// `views/<id>.md`, keeping only characters that are safe in one path segment.
fn default_view_path(id: &str) -> String {
    let name = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("views/{}.md", name.trim_start_matches('.'))
}

fn visible_records(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<(super::LoadedCollection, MdbaseQuerySnapshot), AppError> {
    let loaded = load_collection_authorized(paths, filter)?;
    // An unconstrained plan: the snapshot is every visible record, verified
    // against disk exactly as an ordinary query's records are.
    let everything =
        compile_mdbase_prepared_query(&serde_json::json!({})).map_err(AppError::operation)?;
    let records = load_query_records(
        paths,
        &loaded,
        filter,
        &mut MdbaseQueryMetrics::default(),
        &everything,
    )?;
    Ok((loaded, records))
}

/// Keep the first canonical code (`view_not_found`, `context_required`, ...)
/// machine-readable for callers.
fn view_error(error: MdbaseQueryError) -> AppError {
    match error.diagnostics.first() {
        Some(diagnostic) => AppError::operation_with_code(diagnostic.code.clone(), error),
        None => AppError::operation(error),
    }
}

#[cfg(test)]
mod tests;
