//! Saved-view discovery and execution over the caller's authorized records.
//!
//! Both operations load one disk-verified, permission-filtered record
//! snapshot through the shared query path, so a view, its invocation context,
//! and its candidates come from the same visible collection state. Hidden view
//! sources are not listed and hidden context records are reported as not
//! found, exactly like hidden ordinary records.

use super::{load_collection_authorized, load_query_records, registry_diagnostics, AppError};
use super::{MdbaseQueryMetrics, VaultPaths};
use std::time::SystemTime;
use vulcan_core::mdbase::{
    compile_mdbase_prepared_query, execute_mdbase_view, list_mdbase_views, MdbaseQueryError,
    MdbaseQueryResult, MdbaseQuerySnapshot, MdbaseViewInvocation, MdbaseViewList,
};
use vulcan_core::PermissionFilter;

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
