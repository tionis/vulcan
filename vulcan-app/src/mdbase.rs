//! Reusable mdbase collection read and journaled write workflows.

use crate::{plugins, AppError};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::SystemTime;
use vulcan_core::mdbase::{
    apply_mdbase_write_transaction_with_preflight, authorize_mdbase_write_validation_scope,
    build_mdbase_write_preview, compile_mdbase_query, discover_mdbase_files, execute_mdbase_query,
    load_mdbase_collection, load_mdbase_contract_registry,
    load_mdbase_records_with_contracts_filtered, load_mdbase_type_registry,
    MdbaseAuthorizedValidationScope, MdbaseCollection, MdbaseConsistentReadGuard,
    MdbaseContractDefinition, MdbaseContractImplementation, MdbaseContractRegistry,
    MdbaseDiagnostic, MdbaseDiagnosticLevel, MdbaseQueryResult, MdbaseRecordDocument,
    MdbaseTypeDefinition, MdbaseTypeRegistry, MdbaseWriteApplyRequest,
    MdbaseWriteAuthorizationRequest, MdbaseWriteOutcome, MdbaseWritePreview,
    MdbaseWritePreviewChangeRequest, MdbaseWritePreviewRequest, MdbaseWritePreviewVerification,
};
use vulcan_core::{
    auto_commit, initialize_vulcan_dir, load_vault_config, resolve_permission_profile,
    AutoCommitReport, ConfigDiagnosticKind, GitTrigger, PermissionFilter, PermissionGuard,
    PluginEvent, ProfilePermissionGuard, ScanMode, ScanSummary, VaultConfig, VaultPaths,
};

const DEFAULT_WRITE_PREVIEW_TTL_SECONDS: i64 = 300;
const MAX_WRITE_PREVIEW_TTL_SECONDS: i64 = 3_600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MdbaseWriteOperation {
    Create,
    Update,
    Delete,
    Rename { from: String, to: String },
    Batch,
}

impl MdbaseWriteOperation {
    fn name(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Rename { .. } => "rename",
            Self::Batch => "batch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteChangeRequest {
    pub path: String,
    /// Exact proposed UTF-8 Markdown, or `None` to delete the path.
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePlanRequest {
    pub caller_id: String,
    pub instance_id: String,
    pub operation: MdbaseWriteOperation,
    pub changes: Vec<MdbaseWriteChangeRequest>,
    pub matched_types: Vec<String>,
    pub generated_values: BTreeMap<String, serde_json::Value>,
    pub permission_profile: Option<String>,
    pub ttl_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWritePlanReport {
    pub dry_run: bool,
    pub permission_profile: String,
    pub authorization: MdbaseAuthorizedValidationScope,
    pub preview: MdbaseWritePreview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseWriteExecutionOptions {
    pub idempotency_key: String,
    pub no_commit: bool,
    pub quiet: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseWriteApplyReport {
    pub dry_run: bool,
    pub outcome: MdbaseWriteOutcome,
    pub scan: Option<ScanSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_commit: Option<AutoCommitReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_up_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseStatusReport {
    pub collection_root: String,
    pub spec_version: String,
    pub records: usize,
    pub types: usize,
    pub contracts: usize,
    pub nested_collections: Vec<String>,
    pub valid: bool,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseTypesReport {
    pub types: Vec<MdbaseTypeDefinition>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseContractEntry {
    pub contract: MdbaseContractDefinition,
    pub implementations: Vec<MdbaseContractImplementation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseContractsReport {
    pub contracts: Vec<MdbaseContractEntry>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseValidationRecord {
    pub path: String,
    pub valid: bool,
    pub types: Vec<String>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseValidateReport {
    pub valid: bool,
    pub records: Vec<MdbaseValidationRecord>,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseReadReport {
    pub valid: bool,
    pub record: MdbaseRecordDocument,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

struct LoadedCollection {
    read_guard: Option<MdbaseConsistentReadGuard>,
    collection: MdbaseCollection,
    types: MdbaseTypeRegistry,
    contracts: MdbaseContractRegistry,
}

pub fn build_mdbase_status_report(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseStatusReport, AppError> {
    let loaded = load_collection(paths)?;
    let discovery = discover_mdbase_files(&loaded.collection).map_err(AppError::operation)?;
    let records = discovery
        .records
        .iter()
        .filter(|path| allowed(filter, path))
        .count();
    let types = loaded
        .types
        .iter()
        .filter(|definition| allowed(filter, &definition.path))
        .count();
    let contracts = loaded
        .contracts
        .iter()
        .filter(|definition| allowed(filter, &definition.path))
        .count();
    let diagnostics = registry_diagnostics(&loaded, filter);
    Ok(MdbaseStatusReport {
        collection_root: loaded.collection.root.to_string_lossy().into_owned(),
        spec_version: loaded.collection.config.spec_version.clone(),
        records,
        types,
        contracts,
        nested_collections: discovery.nested_collections,
        valid: diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error),
        diagnostics,
    })
}

pub fn build_mdbase_types_report(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseTypesReport, AppError> {
    let loaded = load_collection(paths)?;
    Ok(MdbaseTypesReport {
        types: loaded
            .types
            .iter()
            .filter(|definition| allowed(filter, &definition.path))
            .cloned()
            .collect(),
        diagnostics: loaded
            .types
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
            .map(MdbaseDiagnostic::from_type)
            .collect(),
    })
}

pub fn build_mdbase_contracts_report(
    paths: &VaultPaths,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseContractsReport, AppError> {
    let loaded = load_collection(paths)?;
    let contracts = loaded
        .contracts
        .iter()
        .filter(|contract| allowed(filter, &contract.path))
        .map(|contract| MdbaseContractEntry {
            contract: contract.clone(),
            implementations: loaded
                .contracts
                .implementations(&contract.identity.id, &contract.identity.version)
                .iter()
                .filter(|implementation| allowed(filter, &implementation.type_path))
                .cloned()
                .collect(),
        })
        .collect();
    let diagnostics = loaded
        .contracts
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
        .map(MdbaseDiagnostic::from_contract)
        .collect();
    Ok(MdbaseContractsReport {
        contracts,
        diagnostics,
    })
}

pub fn build_mdbase_validate_report(
    paths: &VaultPaths,
    path: Option<&str>,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseValidateReport, AppError> {
    let loaded = load_collection(paths)?;
    if let Some(path) = path {
        ensure_allowed(filter, path)?;
    }
    let records = load_mdbase_records_with_contracts_filtered(
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
        false,
        filter,
    )
    .map_err(AppError::operation)?;
    let records = records
        .records
        .into_iter()
        .filter(|record| match path {
            Some(path) => record.path == path,
            None => true,
        })
        .map(|record| {
            let valid = record.is_valid();
            MdbaseValidationRecord {
                path: record.path,
                valid,
                types: record.types,
                diagnostics: record
                    .diagnostics
                    .iter()
                    .map(MdbaseDiagnostic::from_record)
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    if path.is_some() && records.is_empty() {
        return Err(AppError::operation("mdbase record was not found"));
    }
    let diagnostics = registry_diagnostics(&loaded, filter);
    let valid = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error)
        && records.iter().all(|record| record.valid);
    Ok(MdbaseValidateReport {
        valid,
        records,
        diagnostics,
    })
}

pub fn build_mdbase_read_report(
    paths: &VaultPaths,
    path: &str,
    include_source: bool,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseReadReport, AppError> {
    ensure_allowed(filter, path)?;
    let loaded = load_collection(paths)?;
    // Use the filtered collection read so cross-record validation and contract
    // projections have the same visibility semantics as `validate`.
    let records = load_mdbase_records_with_contracts_filtered(
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
        include_source,
        filter,
    )
    .map_err(AppError::operation)?;
    let record = if let Some(derived) = records.get(path) {
        derived.clone()
    } else {
        return Err(AppError::operation("mdbase record was not found"));
    };
    let mut diagnostics = registry_diagnostics(&loaded, filter);
    diagnostics.extend(record.diagnostics.iter().map(MdbaseDiagnostic::from_record));
    let valid = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != MdbaseDiagnosticLevel::Error);
    Ok(MdbaseReadReport {
        valid,
        record,
        diagnostics,
    })
}

/// Execute a canonical mdbase query over the records visible to the caller.
/// Record source is loaded for `file.body` evaluation, but is returned only
/// when the query explicitly opts into `include_body`.
pub fn build_mdbase_query_report(
    paths: &VaultPaths,
    query: &serde_json::Value,
    filter: Option<&PermissionFilter>,
) -> Result<MdbaseQueryResult, AppError> {
    let loaded = load_collection(paths)?;
    let plan = compile_mdbase_query(query).map_err(AppError::operation)?;
    let records = load_mdbase_records_with_contracts_filtered(
        &loaded.collection,
        &loaded.types,
        &loaded.contracts,
        true,
        filter,
    )
    .map_err(AppError::operation)?;
    let mut report = execute_mdbase_query(
        &records,
        &loaded.types,
        &plan,
        &loaded.collection.config.settings.id_field,
        loaded.collection.config.settings.timezone.as_deref(),
        DateTime::<Utc>::from(SystemTime::now()),
    )
    .map_err(AppError::operation)?;
    report
        .diagnostics
        .splice(0..0, registry_diagnostics(&loaded, filter));
    Ok(report)
}

pub fn parse_mdbase_query(source: &str) -> Result<serde_json::Value, AppError> {
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(source)
        .map_err(|error| AppError::operation(format!("invalid query YAML or JSON: {error}")))?;
    serde_json::to_value(yaml).map_err(AppError::operation)
}

/// Build an immutable, authorization-bound dry-run for an mdbase mutation.
///
/// Planning reads exact before-images but performs no canonical or cache
/// mutation and dispatches no plugin lifecycle events.
pub fn plan_mdbase_write(
    paths: &VaultPaths,
    request: &MdbaseWritePlanRequest,
    now: DateTime<Utc>,
) -> Result<MdbaseWritePlanReport, AppError> {
    validate_plan_request(request)?;
    let loaded = load_collection(paths)?;
    for type_name in &request.matched_types {
        if loaded.types.get(type_name).is_none() {
            return Err(AppError::operation(format!(
                "unknown mdbase type in proposed draft: {type_name}"
            )));
        }
    }
    let selection = resolve_permission_profile(paths, request.permission_profile.as_deref())
        .map_err(AppError::operation)?;
    let config = load_write_config(paths)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    let affected_paths = request
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    // Before-images are part of every reviewed plan, including absence
    // preconditions for creates, so affected paths require both capabilities.
    let authorization = authorize_mdbase_write_validation_scope(
        &loaded.collection,
        &loaded.types,
        "",
        &MdbaseWriteAuthorizationRequest {
            read_paths: affected_paths.clone(),
            write_paths: affected_paths,
            matched_types: request.matched_types.clone(),
        },
        &guard,
    )
    .map_err(AppError::operation)?;
    let ttl = request
        .ttl_seconds
        .unwrap_or(DEFAULT_WRITE_PREVIEW_TTL_SECONDS);
    let preview = build_mdbase_write_preview(
        &loaded.collection,
        MdbaseWritePreviewRequest {
            plan_id: ulid::Ulid::new().to_string().to_lowercase(),
            caller_id: request.caller_id.clone(),
            instance_id: request.instance_id.clone(),
            operation: request.operation.name().to_string(),
            issued_at: now,
            expires_at: now + TimeDelta::seconds(ttl),
            permission_revision: permission_revision(guard.selection())?,
            config_revision: config_revision(&config)?,
            changes: request
                .changes
                .iter()
                .map(|change| MdbaseWritePreviewChangeRequest {
                    path: change.path.clone(),
                    after: change.after.clone(),
                })
                .collect(),
            matched_types: request.matched_types.clone(),
            relevant_record_namespaces: authorization.collection_record_namespaces.clone(),
            generated_values: request.generated_values.clone(),
        },
    )
    .map_err(AppError::operation)?;
    validate_operation_shape(&request.operation, &preview)?;
    Ok(MdbaseWritePlanReport {
        dry_run: true,
        permission_profile: guard.selection().name.clone(),
        authorization,
        preview,
    })
}

/// Apply an exact reviewed plan through the crash-safe mdbase transaction.
/// Cache refresh is part of consistency; plugin delivery and opt-in Git occur
/// only after the canonical transaction commits and are never repeated for an
/// idempotent replay.
pub fn apply_mdbase_write(
    paths: &VaultPaths,
    plan: &MdbaseWritePlanReport,
    options: &MdbaseWriteExecutionOptions,
    now: DateTime<Utc>,
) -> Result<MdbaseWriteApplyReport, AppError> {
    if !plan.dry_run {
        return Err(AppError::operation("invalid mdbase write plan"));
    }
    let mut loaded = load_collection(paths)?;
    let selection = resolve_permission_profile(paths, Some(&plan.permission_profile))
        .map_err(AppError::operation)?;
    let guard = ProfilePermissionGuard::new(paths, selection);
    let affected_paths = plan
        .preview
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    let authorization = authorize_mdbase_write_validation_scope(
        &loaded.collection,
        &loaded.types,
        "",
        &MdbaseWriteAuthorizationRequest {
            read_paths: affected_paths.clone(),
            write_paths: affected_paths,
            matched_types: plan.preview.matched_types.clone(),
        },
        &guard,
    )
    .map_err(AppError::operation)?;
    if authorization != plan.authorization {
        return Err(AppError::operation(
            "mdbase write authorization changed; create and review a new preview",
        ));
    }

    let config = load_write_config(paths)?;
    let should_commit =
        !options.no_commit && config.git.auto_commit && config.git.trigger == GitTrigger::Mutation;
    if should_commit {
        guard.check_git().map_err(AppError::operation)?;
    }
    let permission_revision = permission_revision(guard.selection())?;
    let config_revision = config_revision(&config)?;
    let profile = plan.permission_profile.clone();
    let plugin_payload = write_plugin_payload(&plan.preview);
    let quiet = options.quiet;
    let mut scan = None;
    // The consistent-read guard must be released before the transaction takes
    // the exclusive vault lock. Control/type data remains immutable-plan input
    // and is reverified by the transaction itself.
    drop(loaded.read_guard.take());
    initialize_vulcan_dir(paths).map_err(AppError::operation)?;
    let apply_request = MdbaseWriteApplyRequest {
        preview: &plan.preview,
        verification: MdbaseWritePreviewVerification {
            caller_id: &plan.preview.caller_id,
            instance_id: &plan.preview.instance_id,
            operation: &plan.preview.operation,
            permission_revision: &permission_revision,
            config_revision: &config_revision,
            now,
        },
        idempotency_key: &options.idempotency_key,
    };
    let outcome = apply_mdbase_write_transaction_with_preflight(
        paths,
        &loaded.collection,
        &apply_request,
        || {
            plugins::dispatch_plugin_event(
                paths,
                Some(&profile),
                PluginEvent::OnNoteWrite,
                &plugin_payload,
                quiet,
            )
            .map_err(|error| error.to_string())
        },
        |_| {
            let summary = vulcan_core::scan::scan_vault_unlocked(paths, ScanMode::Incremental)
                .map_err(|error| error.to_string())?;
            scan = Some(summary);
            Ok(())
        },
    )
    .map_err(AppError::operation)?;

    let mut report = MdbaseWriteApplyReport {
        dry_run: false,
        outcome,
        scan,
        auto_commit: None,
        follow_up_errors: Vec::new(),
    };
    if report.outcome.replayed {
        return Ok(report);
    }
    dispatch_committed_path_events(paths, plan, options.quiet);
    if should_commit && report.outcome.follow_up_error.is_none() {
        apply_auto_commit(paths, plan, &config.git, options, &mut report);
    }
    Ok(report)
}

fn validate_plan_request(request: &MdbaseWritePlanRequest) -> Result<(), AppError> {
    let ttl = request
        .ttl_seconds
        .unwrap_or(DEFAULT_WRITE_PREVIEW_TTL_SECONDS);
    if !(1..=MAX_WRITE_PREVIEW_TTL_SECONDS).contains(&ttl) {
        return Err(AppError::operation(
            "mdbase write preview TTL must be between 1 and 3600 seconds",
        ));
    }
    if request.caller_id.trim().is_empty() || request.instance_id.trim().is_empty() {
        return Err(AppError::operation(
            "mdbase writes require non-empty caller and instance identifiers",
        ));
    }
    Ok(())
}

fn validate_operation_shape(
    operation: &MdbaseWriteOperation,
    preview: &MdbaseWritePreview,
) -> Result<(), AppError> {
    let valid = match operation {
        MdbaseWriteOperation::Create => {
            preview.changes.len() == 1
                && preview.changes[0].before.is_none()
                && preview.changes[0].after.is_some()
        }
        MdbaseWriteOperation::Update => {
            preview.changes.len() == 1
                && preview.changes[0].before.is_some()
                && preview.changes[0].after.is_some()
        }
        MdbaseWriteOperation::Delete => {
            preview.changes.len() == 1
                && preview.changes[0].before.is_some()
                && preview.changes[0].after.is_none()
        }
        MdbaseWriteOperation::Rename { from, to } => {
            from != to
                && preview.changes.iter().any(|change| {
                    change.path == *from && change.before.is_some() && change.after.is_none()
                })
                && preview.changes.iter().any(|change| {
                    change.path == *to && change.before.is_none() && change.after.is_some()
                })
        }
        MdbaseWriteOperation::Batch => !preview.changes.is_empty(),
    };
    if valid {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "mdbase {} request does not match the observed before/after state",
            operation.name()
        )))
    }
}

fn permission_revision(
    selection: &vulcan_core::ResolvedPermissionProfile,
) -> Result<String, AppError> {
    revision("permission", selection)
}

fn config_revision(config: &VaultConfig) -> Result<String, AppError> {
    revision("config", config)
}

fn load_write_config(paths: &VaultPaths) -> Result<VaultConfig, AppError> {
    let loaded = load_vault_config(paths);
    if let Some(diagnostic) = loaded
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == ConfigDiagnosticKind::ParseFailure)
    {
        return Err(AppError::operation(format!(
            "cannot plan or apply an mdbase write with invalid configuration at {}: {}",
            diagnostic.path.display(),
            diagnostic.message
        )));
    }
    Ok(loaded.config)
}

fn revision(label: &str, value: &impl Serialize) -> Result<String, AppError> {
    let bytes = serde_json::to_vec(value).map_err(AppError::operation)?;
    Ok(format!("{label}:sha256:{:x}", Sha256::digest(bytes)))
}

fn write_plugin_payload(preview: &MdbaseWritePreview) -> serde_json::Value {
    json!({
        "kind": PluginEvent::OnNoteWrite,
        "operation": preview.operation,
        "plan_id": preview.plan_id,
        "changes": preview.changes,
    })
}

fn dispatch_committed_path_events(paths: &VaultPaths, plan: &MdbaseWritePlanReport, quiet: bool) {
    for change in &plan.preview.changes {
        let event = match (&change.before, &change.after) {
            (None, Some(_)) => Some(PluginEvent::OnNoteCreate),
            (Some(_), None) => Some(PluginEvent::OnNoteDelete),
            _ => None,
        };
        if let Some(event) = event {
            let _ = plugins::dispatch_plugin_event(
                paths,
                Some(&plan.permission_profile),
                event,
                &json!({
                    "kind": event,
                    "operation": plan.preview.operation,
                    "plan_id": plan.preview.plan_id,
                    "path": change.path,
                    "content": change.after,
                }),
                quiet,
            );
        }
    }
}

fn apply_auto_commit(
    paths: &VaultPaths,
    plan: &MdbaseWritePlanReport,
    git: &vulcan_core::GitConfig,
    options: &MdbaseWriteExecutionOptions,
    report: &mut MdbaseWriteApplyReport,
) {
    let changed_paths = plan
        .preview
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    let payload = json!({
        "kind": PluginEvent::OnPreCommit,
        "operation": plan.preview.operation,
        "plan_id": plan.preview.plan_id,
        "files": changed_paths,
    });
    if let Err(error) = plugins::dispatch_plugin_event(
        paths,
        Some(&plan.permission_profile),
        PluginEvent::OnPreCommit,
        &payload,
        options.quiet,
    ) {
        report
            .follow_up_errors
            .push(format!("auto-commit preflight failed: {error}"));
        return;
    }
    match auto_commit(
        paths.vault_root(),
        git,
        &format!("mdbase {}", plan.preview.operation),
        &changed_paths,
    ) {
        Ok(commit) => {
            let post_payload = json!({
                "kind": PluginEvent::OnPostCommit,
                "operation": plan.preview.operation,
                "plan_id": plan.preview.plan_id,
                "commit": commit,
            });
            let _ = plugins::dispatch_plugin_event(
                paths,
                Some(&plan.permission_profile),
                PluginEvent::OnPostCommit,
                &post_payload,
                options.quiet,
            );
            report.auto_commit = Some(commit);
        }
        Err(error) => report
            .follow_up_errors
            .push(format!("auto-commit failed: {error}")),
    }
}

fn load_collection(paths: &VaultPaths) -> Result<LoadedCollection, AppError> {
    let read_guard =
        vulcan_core::mdbase::acquire_mdbase_consistent_read(paths).map_err(AppError::operation)?;
    let collection = load_mdbase_collection(paths.vault_root())
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("not an mdbase collection: missing mdbase.yaml"))?;
    let types = load_mdbase_type_registry(&collection).map_err(AppError::operation)?;
    let contracts =
        load_mdbase_contract_registry(&collection, &types).map_err(AppError::operation)?;
    Ok(LoadedCollection {
        read_guard,
        collection,
        types,
        contracts,
    })
}

fn registry_diagnostics(
    loaded: &LoadedCollection,
    filter: Option<&PermissionFilter>,
) -> Vec<MdbaseDiagnostic> {
    let mut diagnostics = loaded
        .collection
        .diagnostics
        .iter()
        .map(MdbaseDiagnostic::from_config)
        .collect::<Vec<_>>();
    diagnostics.extend(
        loaded
            .types
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
            .map(MdbaseDiagnostic::from_type),
    );
    diagnostics.extend(
        loaded
            .contracts
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.path.is_empty() || allowed(filter, &diagnostic.path))
            .map(MdbaseDiagnostic::from_contract),
    );
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.field.cmp(&right.field))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    diagnostics
}

fn allowed(filter: Option<&PermissionFilter>, path: &str) -> bool {
    match filter {
        Some(filter) => filter.is_allowed(path),
        None => true,
    }
}

fn ensure_allowed(filter: Option<&PermissionFilter>, path: &str) -> Result<(), AppError> {
    if allowed(filter, path) {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "read permission denied for mdbase record: {path}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::mdbase::{
        apply_mdbase_write_transaction, build_mdbase_write_preview, MdbaseWriteApplyRequest,
        MdbaseWritePreviewChangeRequest, MdbaseWritePreviewRequest, MdbaseWritePreviewVerification,
    };
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::permissions::{PathPermission, ResourceSpecifier};

    fn write_plan_request(
        operation: MdbaseWriteOperation,
        changes: Vec<MdbaseWriteChangeRequest>,
    ) -> MdbaseWritePlanRequest {
        MdbaseWritePlanRequest {
            caller_id: "test-caller".to_string(),
            instance_id: "test-instance".to_string(),
            operation,
            changes,
            matched_types: vec!["task".to_string()],
            generated_values: BTreeMap::new(),
            permission_profile: None,
            ttl_seconds: Some(300),
        }
    }

    fn fixture() -> (tempfile::TempDir, VaultPaths) {
        let directory = tempdir().expect("temp directory");
        fs::write(
            directory.path().join("mdbase.yaml"),
            "spec_version: \"0.3.0\"\n",
        )
        .expect("config");
        fs::create_dir_all(directory.path().join("_types")).expect("types directory");
        fs::write(
            directory.path().join("_types/task.md"),
            "---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value:\n    type: object\n    required: [type, title]\n    properties:\n      type: {const: task}\n      title: {type: string}\ncollection:\n  read_defaults: {status: open}\n---\n",
        )
        .expect("type");
        fs::create_dir_all(directory.path().join("tasks/private")).expect("records directory");
        fs::write(
            directory.path().join("tasks/public.md"),
            "---\ntype: task\ntitle: Public\n---\nBody\n",
        )
        .expect("public record");
        fs::write(
            directory.path().join("tasks/private/secret.md"),
            "---\ntype: task\ntitle: Secret\n---\nHidden\n",
        )
        .expect("private record");
        let paths = VaultPaths::new(directory.path());
        (directory, paths)
    }

    #[test]
    fn read_surface_filters_records_before_validation_and_source_is_opt_in() {
        let (_directory, paths) = fixture();
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("tasks/**".to_string())],
            deny: vec![ResourceSpecifier::Folder("tasks/private/**".to_string())],
        });
        let status = build_mdbase_status_report(&paths, Some(&filter)).expect("status");
        assert_eq!(status.records, 1);

        let report = build_mdbase_validate_report(&paths, None, Some(&filter)).expect("validate");
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].path, "tasks/public.md");

        let without_source =
            build_mdbase_read_report(&paths, "tasks/public.md", false, Some(&filter))
                .expect("read");
        assert!(without_source.record.document.is_none());
        assert_eq!(
            without_source.record.effective_frontmatter["status"],
            "open"
        );
        let with_source = build_mdbase_read_report(&paths, "tasks/public.md", true, Some(&filter))
            .expect("read with source");
        assert!(with_source.record.document.is_some());
        assert!(
            build_mdbase_read_report(&paths, "tasks/private/secret.md", true, Some(&filter))
                .is_err()
        );
    }

    #[test]
    fn registry_read_surfaces_are_deterministic_and_non_mutating() {
        let (directory, paths) = fixture();
        let before = fs::read(directory.path().join("tasks/public.md")).expect("record bytes");
        let types = build_mdbase_types_report(&paths, None).expect("types");
        let contracts = build_mdbase_contracts_report(&paths, None).expect("contracts");
        assert_eq!(types.types.len(), 1);
        assert_eq!(types.types[0].name, "task");
        assert!(contracts.contracts.is_empty());
        assert_eq!(
            fs::read(directory.path().join("tasks/public.md")).expect("record bytes"),
            before
        );
        assert!(!directory.path().join(".vulcan").exists());
    }

    #[test]
    fn query_filters_effective_values_and_never_leaks_denied_records() {
        let (_directory, paths) = fixture();
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("tasks/**".to_string())],
            deny: vec![ResourceSpecifier::Folder("tasks/private/**".to_string())],
        });
        let report = build_mdbase_query_report(
            &paths,
            &serde_json::json!({
                "types": ["task"],
                "where": "status == \"open\" && file.body.contains(\"Body\")",
                "select": ["title", {"name": "display", "expr": "title + \"!\""}],
                "order_by": [{"field": "file.path"}],
                "group_by": [{"field": "status"}],
                "summaries": [{"field": "title", "function": "count", "name": "tasks"}],
                "include_body": false,
                "frontmatter_mode": "effective"
            }),
            Some(&filter),
        )
        .expect("query");

        assert_eq!(report.meta.total_count, 1);
        assert_eq!(report.results[0].file["path"], "tasks/public.md");
        assert_eq!(
            report.results[0].values.as_ref().unwrap()["display"],
            "Public!"
        );
        assert!(report.results[0].body.is_none());
        assert_eq!(
            report.meta.groups.as_ref().unwrap()[0].summaries["tasks"],
            1
        );
    }

    #[test]
    fn canonical_query_source_accepts_yaml_and_json() {
        assert_eq!(
            parse_mdbase_query("types: [task]\nlimit: 2\n").expect("YAML"),
            serde_json::json!({"types": ["task"], "limit": 2})
        );
        assert_eq!(
            parse_mdbase_query(r#"{"where":"true"}"#).expect("JSON"),
            serde_json::json!({"where": "true"})
        );
    }

    #[test]
    fn cooperating_read_reports_a_transaction_that_needs_recovery() {
        let (directory, paths) = fixture();
        initialize_vulcan_dir(&paths).expect("initialize transaction state");
        let collection = load_mdbase_collection(directory.path())
            .expect("load collection")
            .expect("collection");
        let issued_at = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let preview = build_mdbase_write_preview(
            &collection,
            MdbaseWritePreviewRequest {
                plan_id: "read-boundary".to_string(),
                caller_id: "caller".to_string(),
                instance_id: "instance".to_string(),
                operation: "update".to_string(),
                issued_at,
                expires_at: Utc.with_ymd_and_hms(2026, 9, 13, 12, 10, 0).unwrap(),
                permission_revision: "grant:v1".to_string(),
                config_revision: "config:v1".to_string(),
                changes: vec![MdbaseWritePreviewChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some("---\ntype: task\ntitle: Changed\n---\nBody\n".to_string()),
                }],
                matched_types: vec!["task".to_string()],
                relevant_record_namespaces: vec!["tasks/**".to_string()],
                generated_values: BTreeMap::new(),
            },
        )
        .expect("preview");
        let request = MdbaseWriteApplyRequest {
            preview: &preview,
            verification: MdbaseWritePreviewVerification {
                caller_id: "caller",
                instance_id: "instance",
                operation: "update",
                permission_revision: "grant:v1",
                config_revision: "config:v1",
                now: Utc.with_ymd_and_hms(2026, 9, 13, 12, 1, 0).unwrap(),
            },
            idempotency_key: "read-boundary",
        };
        apply_mdbase_write_transaction(&paths, &collection, &request, |_| {
            Err("simulated cache failure".to_string())
        })
        .expect("canonical write committed");

        let error = build_mdbase_status_report(&paths, None).unwrap_err();
        assert!(error.to_string().contains("recovery is required"));
    }

    #[test]
    fn dry_run_then_apply_updates_cache_and_replay_has_no_follow_up_work() {
        let (directory, paths) = fixture();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let plan = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Update,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some("---\ntype: task\ntitle: Updated\n---\nBody\n".to_string()),
                }],
            ),
            now,
        )
        .expect("plan update");
        assert!(plan.dry_run);
        assert_eq!(
            fs::read_to_string(directory.path().join("tasks/public.md")).unwrap(),
            "---\ntype: task\ntitle: Public\n---\nBody\n"
        );
        assert!(!directory.path().join(".vulcan").exists());

        let options = MdbaseWriteExecutionOptions {
            idempotency_key: "update-public".to_string(),
            no_commit: true,
            quiet: true,
        };
        let report =
            apply_mdbase_write(&paths, &plan, &options, now + chrono::Duration::seconds(1))
                .expect("apply update");
        assert!(!report.outcome.replayed);
        assert!(report.scan.is_some());
        assert!(report.follow_up_errors.is_empty());
        assert!(fs::read_to_string(directory.path().join("tasks/public.md"))
            .unwrap()
            .contains("title: Updated"));

        let replay =
            apply_mdbase_write(&paths, &plan, &options, now + chrono::Duration::seconds(2))
                .expect("idempotent replay");
        assert!(replay.outcome.replayed);
        assert!(replay.scan.is_none());
        assert!(replay.auto_commit.is_none());
    }

    #[test]
    fn operation_shapes_cover_create_delete_rename_and_batch_without_mutation() {
        let (directory, paths) = fixture();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let create = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Create,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/new.md".to_string(),
                    after: Some("---\ntype: task\ntitle: New\n---\n".to_string()),
                }],
            ),
            now,
        )
        .expect("create plan");
        assert_eq!(create.preview.operation, "create");

        let delete = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Delete,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: None,
                }],
            ),
            now,
        )
        .expect("delete plan");
        assert_eq!(delete.preview.operation, "delete");

        let rename = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Rename {
                    from: "tasks/public.md".to_string(),
                    to: "tasks/renamed.md".to_string(),
                },
                vec![
                    MdbaseWriteChangeRequest {
                        path: "tasks/public.md".to_string(),
                        after: None,
                    },
                    MdbaseWriteChangeRequest {
                        path: "tasks/renamed.md".to_string(),
                        after: Some("---\ntype: task\ntitle: Public\n---\nBody\n".to_string()),
                    },
                ],
            ),
            now,
        )
        .expect("rename plan");
        assert_eq!(rename.preview.changes.len(), 2);

        let batch = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Batch,
                vec![
                    MdbaseWriteChangeRequest {
                        path: "tasks/public.md".to_string(),
                        after: Some("---\ntype: task\ntitle: Batched\n---\nBody\n".to_string()),
                    },
                    MdbaseWriteChangeRequest {
                        path: "tasks/new.md".to_string(),
                        after: Some("---\ntype: task\ntitle: New\n---\n".to_string()),
                    },
                ],
            ),
            now,
        )
        .expect("batch plan");
        assert_eq!(batch.preview.changes.len(), 2);
        assert!(!directory.path().join("tasks/new.md").exists());
        assert!(directory.path().join("tasks/public.md").exists());
    }

    #[test]
    fn operation_shape_mismatch_is_rejected() {
        let (_directory, paths) = fixture();
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap();
        let error = plan_mdbase_write(
            &paths,
            &write_plan_request(
                MdbaseWriteOperation::Create,
                vec![MdbaseWriteChangeRequest {
                    path: "tasks/public.md".to_string(),
                    after: Some("replacement".to_string()),
                }],
            ),
            now,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not match"));
    }
}
