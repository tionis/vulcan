//! Reusable, non-mutating mdbase collection read workflows.

use crate::AppError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::SystemTime;
use vulcan_core::mdbase::{
    compile_mdbase_query, discover_mdbase_files, execute_mdbase_query, load_mdbase_collection,
    load_mdbase_contract_registry, load_mdbase_records_with_contracts_filtered,
    load_mdbase_type_registry, MdbaseCollection, MdbaseContractDefinition,
    MdbaseContractImplementation, MdbaseContractRegistry, MdbaseDiagnostic, MdbaseDiagnosticLevel,
    MdbaseQueryResult, MdbaseRecordDocument, MdbaseTypeDefinition, MdbaseTypeRegistry,
};
use vulcan_core::{PermissionFilter, VaultPaths};

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

fn load_collection(paths: &VaultPaths) -> Result<LoadedCollection, AppError> {
    let collection = load_mdbase_collection(paths.vault_root())
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("not an mdbase collection: missing mdbase.yaml"))?;
    let types = load_mdbase_type_registry(&collection).map_err(AppError::operation)?;
    let contracts =
        load_mdbase_contract_registry(&collection, &types).map_err(AppError::operation)?;
    Ok(LoadedCollection {
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
    use std::fs;
    use tempfile::tempdir;
    use vulcan_core::permissions::{PathPermission, ResourceSpecifier};

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
}
