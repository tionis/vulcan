//! Diagnostic instrumentation, not a new canonical query envelope or freshness policy.

use super::{load_collection_authorized, load_query_records, registry_diagnostics};
use crate::AppError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::{Instant, SystemTime};
use vulcan_core::mdbase::{
    compile_mdbase_prepared_query, MdbaseCachedLoadMetrics, MdbaseQueryResult,
};
use vulcan_core::{PermissionFilter, VaultPaths};

/// Per-operation diagnostics for the shared query workflow. No paths, field
/// values, expressions, or denied-record counts are retained. These are not a
/// complete I/O audit: completed-manifest counts exclude partial failed scans,
/// and source fallback/refresh currently expose aggregate time and attempt counts.
/// Cached-load timings are nested inside record preparation, not additive to it.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct MdbaseQueryMetrics {
    pub total_seconds: f64,
    pub collection_seconds: f64,
    pub query_preparation_seconds: f64,
    pub record_preparation_seconds: f64,
    pub manifest_before_seconds: f64,
    pub manifest_after_seconds: f64,
    pub cache_open_seconds: f64,
    pub cache_refresh_seconds: f64,
    pub source_fallback_seconds: f64,
    pub execution_seconds: f64,
    pub diagnostic_assembly_seconds: f64,
    pub completed_manifests: usize,
    pub completed_manifest_records: usize,
    pub completed_manifest_bytes: u64,
    pub cache_attempts: usize,
    pub cache_hits: usize,
    pub cache_refresh_attempts: usize,
    pub cache_rebuild_attempts: usize,
    pub source_loads: usize,
    pub prepared_visible_records: usize,
    pub cached_load: MdbaseCachedLoadMetrics,
}

/// Execute exactly the ordinary shared query workflow, retaining diagnostic
/// metrics even on error. Resets the supplied metrics before authorization so a
/// reused collector cannot expose a previous operation's scope. Does not log,
/// serialize results, or include caller-side permission-profile construction.
pub fn build_mdbase_query_report_profiled(
    paths: &VaultPaths,
    query: &serde_json::Value,
    filter: Option<&PermissionFilter>,
    metrics: &mut MdbaseQueryMetrics,
) -> Result<MdbaseQueryResult, AppError> {
    *metrics = MdbaseQueryMetrics::default();
    let start = Instant::now();
    let result = (|| {
        let loaded = time(&mut metrics.collection_seconds, || {
            load_collection_authorized(paths, filter)
        })?;
        let prepared = time(&mut metrics.query_preparation_seconds, || {
            compile_mdbase_prepared_query(query).map_err(AppError::operation)
        })?;
        let record_start = Instant::now();
        let records = load_query_records(paths, &loaded, filter, metrics);
        metrics.record_preparation_seconds += record_start.elapsed().as_secs_f64();
        let records = records?;
        metrics.prepared_visible_records = records.records.len();
        let mut report = time(&mut metrics.execution_seconds, || {
            prepared.execute(
                &records,
                &loaded.types,
                &loaded.collection.config.settings.id_field,
                loaded.collection.config.settings.timezone.as_deref(),
                DateTime::<Utc>::from(SystemTime::now()),
            )
        })
        .map_err(AppError::operation)?;
        time(&mut metrics.diagnostic_assembly_seconds, || {
            report
                .diagnostics
                .splice(0..0, registry_diagnostics(&loaded, filter));
        });
        Ok(report)
    })();
    metrics.total_seconds = start.elapsed().as_secs_f64();
    result
}

pub(super) fn time<T>(seconds: &mut f64, operation: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = operation();
    *seconds += start.elapsed().as_secs_f64();
    result
}

#[cfg(test)]
mod tests;
