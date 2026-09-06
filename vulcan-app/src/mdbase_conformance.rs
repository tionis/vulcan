//! Adapter for the pinned upstream mdbase v0.3 semantic fixture DSL.

use crate::AppError;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::SystemTime;
use vulcan_core::mdbase::{
    compose_mdbase_type_behavior, load_mdbase_collection, load_mdbase_contract_registry,
    load_mdbase_records_with_contracts, load_mdbase_type_registry, project_mdbase_contract_view,
    render_mdbase_path_pattern, validate_mdbase_schema_value, MdbaseRecordDiagnostic,
    MDBASE_BUNDLED_ASSET_DIGEST, MDBASE_CANONICAL_SCHEMA_BASE, MDBASE_SCHEMA_MAX_BYTES,
    MDBASE_SCHEMA_MAX_DEPTH, MDBASE_SCHEMA_MAX_FILES, MDBASE_SPEC_UPSTREAM_COMMIT,
    MDBASE_SPEC_VERSION, MDBASE_V03_CONFLICTING_TASKNOTES_CONTRACT,
    MDBASE_V03_CORE_COLLECTION_SUITE, MDBASE_V03_DATA_CONTRACTS_SUITE, MDBASE_V03_MANIFEST,
    MDBASE_V03_TASKNOTES_CONTRACT, MDBASE_V03_TASKNOTES_TYPE, MDBASE_V03_VALID_TASK,
};
use vulcan_core::paths::{normalize_relative_input_path, RelativePathOptions};

const TARGET_PROFILES: [&str; 2] = ["core_read", "collection_semantics"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MdbaseConformanceCaseStatus {
    Pass,
    Fail,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConformanceCaseResult {
    pub id: String,
    pub name: String,
    pub fixture_set: String,
    pub operation: String,
    pub covers: Vec<String>,
    pub status: MdbaseConformanceCaseStatus,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConformanceProfileResult {
    pub profile: String,
    pub evaluated: bool,
    pub supported: bool,
    pub passed: usize,
    pub failed: usize,
    pub unsupported: usize,
    pub missing_requirements: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConformanceEvidenceReport {
    pub valid: bool,
    pub spec_version: String,
    pub upstream_commit: String,
    pub artifact_digest: String,
    pub profiles: Vec<MdbaseConformanceProfileResult>,
    pub cases: Vec<MdbaseConformanceCaseResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConformanceClaim {
    pub kind: String,
    pub status: String,
    pub implementation: MdbaseConformanceImplementation,
    pub spec_version: String,
    pub profiles: Vec<String>,
    pub json_schema: MdbaseConformanceJsonSchema,
    pub limits: BTreeMap<String, serde_json::Value>,
    pub evidence: Vec<MdbaseConformanceEvidence>,
    #[serde(rename = "x-vulcan-upstream-commit")]
    pub upstream_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConformanceImplementation {
    pub id: String,
    pub name: String,
    pub version: String,
    pub language: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConformanceJsonSchema {
    pub dialect: String,
    pub keywords: Vec<String>,
    pub formats: Vec<String>,
    pub remote_refs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseConformanceEvidence {
    pub kind: String,
    pub command: String,
    pub result: String,
    pub verified_at: String,
    pub artifact: String,
}

#[derive(Debug, Deserialize)]
struct FixtureSuite {
    fixture_set: String,
    groups: Vec<FixtureGroup>,
}

#[derive(Debug, Deserialize)]
struct FixtureGroup {
    #[serde(default)]
    setup: Option<FixtureSetup>,
    tests: Vec<FixtureCase>,
}

#[derive(Debug, Default, Deserialize)]
struct FixtureSetup {
    #[serde(default)]
    config: String,
    #[serde(default)]
    types: BTreeMap<String, String>,
    #[serde(default)]
    contracts: BTreeMap<String, String>,
    #[serde(default)]
    files: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct FixtureCase {
    id: Option<String>,
    name: String,
    operation: String,
    #[serde(default)]
    input: serde_yaml::Value,
    #[serde(default)]
    expect: serde_yaml::Value,
    #[serde(default)]
    covers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct FixtureManifest {
    claim_profiles: Vec<ManifestProfile>,
}

#[derive(Debug, Deserialize)]
struct ManifestProfile {
    id: String,
    requirements: Vec<String>,
}

#[derive(Debug)]
enum CaseExecution {
    Actual(serde_json::Value),
    Unsupported(String),
}

/// Run every pinned fixture that declares evidence for `core_read` or
/// `collection_semantics` and verify coverage against the upstream manifest.
pub fn run_mdbase_core_read_conformance() -> Result<MdbaseConformanceEvidenceReport, AppError> {
    let suites = [
        serde_yaml::from_str::<FixtureSuite>(MDBASE_V03_CORE_COLLECTION_SUITE)
            .map_err(AppError::operation)?,
        serde_yaml::from_str::<FixtureSuite>(MDBASE_V03_DATA_CONTRACTS_SUITE)
            .map_err(AppError::operation)?,
    ];
    let manifest = serde_yaml::from_str::<FixtureManifest>(MDBASE_V03_MANIFEST)
        .map_err(AppError::operation)?;
    let mut results = Vec::new();
    for suite in &suites {
        for group in &suite.groups {
            let selected = group
                .tests
                .iter()
                .filter(|case| case.covers.iter().any(|cover| target_cover(cover)))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            let directory = tempfile::tempdir().map_err(AppError::operation)?;
            materialize_setup(directory.path(), group.setup.as_ref())?;
            for case in selected {
                materialize_case_inputs(directory.path(), case)?;
                let execution = execute_case(directory.path(), case);
                let (status, message) = match execution {
                    Ok(CaseExecution::Actual(actual)) => {
                        match assert_expectation(&actual, &case.expect) {
                            Ok(()) => (MdbaseConformanceCaseStatus::Pass, None),
                            Err(message) => (MdbaseConformanceCaseStatus::Fail, Some(message)),
                        }
                    }
                    Ok(CaseExecution::Unsupported(message)) => {
                        (MdbaseConformanceCaseStatus::Unsupported, Some(message))
                    }
                    Err(error) => (MdbaseConformanceCaseStatus::Fail, Some(error.to_string())),
                };
                results.push(MdbaseConformanceCaseResult {
                    id: case.id.clone().unwrap_or_else(|| case.name.clone()),
                    name: case.name.clone(),
                    fixture_set: suite.fixture_set.clone(),
                    operation: case.operation.clone(),
                    covers: case.covers.clone(),
                    status,
                    message,
                });
            }
        }
    }
    results.sort_by(|left, right| left.id.cmp(&right.id));

    let profiles = manifest
        .claim_profiles
        .iter()
        .map(|entry| {
            if TARGET_PROFILES.contains(&entry.id.as_str()) {
                build_profile_result(&entry.id, &manifest, &results)
            } else {
                MdbaseConformanceProfileResult {
                    profile: entry.id.clone(),
                    evaluated: false,
                    supported: false,
                    passed: 0,
                    failed: 0,
                    unsupported: 0,
                    missing_requirements: entry.requirements.clone(),
                }
            }
        })
        .collect::<Vec<_>>();
    let valid = profiles
        .iter()
        .filter(|profile| profile.evaluated)
        .all(|profile| profile.supported)
        && profiles.iter().filter(|profile| profile.evaluated).count() == TARGET_PROFILES.len();
    Ok(MdbaseConformanceEvidenceReport {
        valid,
        spec_version: MDBASE_SPEC_VERSION.to_string(),
        upstream_commit: MDBASE_SPEC_UPSTREAM_COMMIT.to_string(),
        artifact_digest: format!("blake3:{MDBASE_BUNDLED_ASSET_DIGEST}"),
        profiles,
        cases: results,
    })
}

/// Build the canonical claim only from a completely passing pinned evidence
/// report. A future regression therefore removes the claim instead of leaving
/// stale compatibility metadata behind.
pub fn build_mdbase_conformance_claim(
    report: &MdbaseConformanceEvidenceReport,
) -> Result<MdbaseConformanceClaim, AppError> {
    let verified_at =
        DateTime::<Utc>::from(SystemTime::now()).to_rfc3339_opts(SecondsFormat::Secs, true);
    build_mdbase_conformance_claim_at(report, &verified_at)
}

fn build_mdbase_conformance_claim_at(
    report: &MdbaseConformanceEvidenceReport,
    verified_at: &str,
) -> Result<MdbaseConformanceClaim, AppError> {
    if !claimable_report(report) {
        return Err(AppError::operation(
            "mdbase conformance claim withheld because required pinned fixtures did not pass",
        ));
    }
    let claim = MdbaseConformanceClaim {
        kind: "mdbase.conformance".to_string(),
        status: "verified".to_string(),
        implementation: MdbaseConformanceImplementation {
            id: "vulcan".to_string(),
            name: "Vulcan".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            language: "Rust".to_string(),
        },
        spec_version: MDBASE_SPEC_VERSION.to_string(),
        profiles: TARGET_PROFILES.iter().map(ToString::to_string).collect(),
        json_schema: claim_json_schema(),
        limits: claim_limits(),
        evidence: vec![MdbaseConformanceEvidence {
            kind: "conformance_suite".to_string(),
            command: "vulcan mdbase conformance --output json".to_string(),
            result: "pass".to_string(),
            verified_at: verified_at.to_string(),
            artifact: format!(
                "mdbase-spec@{}#blake3:{}",
                report.upstream_commit, MDBASE_BUNDLED_ASSET_DIGEST
            ),
        }],
        upstream_commit: report.upstream_commit.clone(),
    };
    validate_claim(&claim)?;
    Ok(claim)
}

fn claimable_report(report: &MdbaseConformanceEvidenceReport) -> bool {
    report.valid
        && report.spec_version == MDBASE_SPEC_VERSION
        && report.upstream_commit == MDBASE_SPEC_UPSTREAM_COMMIT
        && report.artifact_digest == format!("blake3:{MDBASE_BUNDLED_ASSET_DIGEST}")
        && report
            .cases
            .iter()
            .all(|case| case.status == MdbaseConformanceCaseStatus::Pass)
        && TARGET_PROFILES.iter().all(|profile| {
            report.profiles.iter().any(|result| {
                result.profile == *profile
                    && result.evaluated
                    && result.supported
                    && result.failed == 0
                    && result.unsupported == 0
                    && result.missing_requirements.is_empty()
            })
        })
}

fn claim_json_schema() -> MdbaseConformanceJsonSchema {
    MdbaseConformanceJsonSchema {
        dialect: "https://json-schema.org/draft/2020-12/schema".to_string(),
        keywords: [
            "$defs",
            "$ref",
            "additionalProperties",
            "allOf",
            "anyOf",
            "const",
            "default",
            "description",
            "else",
            "enum",
            "examples",
            "exclusiveMaximum",
            "exclusiveMinimum",
            "if",
            "items",
            "maxItems",
            "maxLength",
            "maximum",
            "minItems",
            "minLength",
            "minimum",
            "multipleOf",
            "not",
            "oneOf",
            "pattern",
            "properties",
            "required",
            "then",
            "title",
            "type",
            "uniqueItems",
        ]
        .into_iter()
        .map(ToString::to_string)
        .collect(),
        formats: ["date", "date-time", "time"]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
        remote_refs: false,
    }
}

fn claim_limits() -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([
        (
            "local_schema_max_bytes".to_string(),
            serde_json::json!(MDBASE_SCHEMA_MAX_BYTES),
        ),
        (
            "local_schema_max_depth".to_string(),
            serde_json::json!(MDBASE_SCHEMA_MAX_DEPTH),
        ),
        (
            "local_schema_max_files".to_string(),
            serde_json::json!(MDBASE_SCHEMA_MAX_FILES),
        ),
    ])
}

fn validate_claim(claim: &MdbaseConformanceClaim) -> Result<(), AppError> {
    let schema = vulcan_core::mdbase::bundled_mdbase_schema(&format!(
        "{MDBASE_CANONICAL_SCHEMA_BASE}conformance-claim.schema.json"
    ))
    .expect("the conformance-claim schema is part of the pinned bundle");
    let schema: serde_json::Value =
        serde_json::from_str(schema.json).map_err(AppError::operation)?;
    let value = serde_json::to_value(claim).map_err(AppError::operation)?;
    let diagnostics = validate_mdbase_schema_value(&schema, &value).map_err(AppError::operation)?;
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(AppError::operation(format!(
            "generated mdbase conformance claim is invalid: {}",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )))
    }
}

fn target_cover(cover: &str) -> bool {
    TARGET_PROFILES
        .iter()
        .any(|profile| cover.starts_with(&format!("{profile}.")))
}

fn build_profile_result(
    profile: &str,
    manifest: &FixtureManifest,
    results: &[MdbaseConformanceCaseResult],
) -> MdbaseConformanceProfileResult {
    let cases = results
        .iter()
        .filter(|case| {
            case.covers
                .iter()
                .any(|cover| cover.starts_with(&format!("{profile}.")))
        })
        .collect::<Vec<_>>();
    let covered = cases
        .iter()
        .filter(|case| case.status == MdbaseConformanceCaseStatus::Pass)
        .flat_map(|case| case.covers.iter())
        .filter_map(|cover| cover.strip_prefix(&format!("{profile}.")))
        .collect::<BTreeSet<_>>();
    let missing_requirements = manifest
        .claim_profiles
        .iter()
        .find(|entry| entry.id == profile)
        .map_or_else(
            || vec!["profile_not_in_manifest".to_string()],
            |entry| {
                entry
                    .requirements
                    .iter()
                    .filter(|requirement| !covered.contains(requirement.as_str()))
                    .cloned()
                    .collect()
            },
        );
    let passed = cases
        .iter()
        .filter(|case| case.status == MdbaseConformanceCaseStatus::Pass)
        .count();
    let failed = cases
        .iter()
        .filter(|case| case.status == MdbaseConformanceCaseStatus::Fail)
        .count();
    let unsupported = cases
        .iter()
        .filter(|case| case.status == MdbaseConformanceCaseStatus::Unsupported)
        .count();
    MdbaseConformanceProfileResult {
        profile: profile.to_string(),
        evaluated: true,
        supported: failed == 0 && unsupported == 0 && missing_requirements.is_empty(),
        passed,
        failed,
        unsupported,
        missing_requirements,
    }
}

fn materialize_setup(root: &Path, setup: Option<&FixtureSetup>) -> Result<(), AppError> {
    let default = FixtureSetup::default();
    let setup = setup.unwrap_or(&default);
    let config = if setup.config.is_empty() {
        "spec_version: \"0.3.0\"\n"
    } else {
        &setup.config
    };
    write_fixture(root, "mdbase.yaml", config)?;
    for (path, contents) in &setup.types {
        write_fixture(root, &format!("_types/{path}"), contents)?;
    }
    for (path, contents) in &setup.contracts {
        write_fixture(root, &format!("_contracts/{path}"), contents)?;
    }
    for (path, contents) in &setup.files {
        write_fixture(root, path, contents)?;
    }
    Ok(())
}

fn materialize_case_inputs(root: &Path, case: &FixtureCase) -> Result<(), AppError> {
    let Some(input) = case.input.as_mapping() else {
        return Ok(());
    };
    if let Some(path) = yaml_string(input, "contract") {
        if let Some(contents) = bundled_reference(path) {
            write_fixture(root, &format!("_contracts/{}", basename(path)?), contents)?;
        }
    }
    if let Some(path) = yaml_string(input, "type") {
        if let Some(contents) = bundled_reference(path) {
            write_fixture(root, &format!("_types/{}", basename(path)?), contents)?;
        }
    }
    if let Some(paths) = input
        .get(serde_yaml::Value::String("paths".to_string()))
        .and_then(serde_yaml::Value::as_sequence)
    {
        for (index, path) in paths
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .enumerate()
        {
            if let Some(contents) = bundled_reference(path) {
                write_fixture(
                    root,
                    &format!("_contracts/{index}-{}", basename(path)?),
                    contents,
                )?;
            }
        }
    }
    Ok(())
}

fn execute_case(root: &Path, case: &FixtureCase) -> Result<CaseExecution, AppError> {
    let collection = load_mdbase_collection(root)
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("fixture did not create mdbase.yaml"))?;
    let types = load_mdbase_type_registry(&collection).map_err(AppError::operation)?;
    let contracts =
        load_mdbase_contract_registry(&collection, &types).map_err(AppError::operation)?;
    let input = case
        .input
        .as_mapping()
        .ok_or_else(|| AppError::operation("fixture input must be a mapping"))?;
    match case.operation.as_str() {
        "validate" | "read" | "get_types" => {
            let path = required_yaml_string(input, "path")?;
            let set = load_mdbase_records_with_contracts(&collection, &types, &contracts, false)
                .map_err(AppError::operation)?;
            let record = set
                .get(path)
                .ok_or_else(|| AppError::operation(format!("record not found: {path}")))?;
            let issues = types
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    serde_json::json!({
                        "code": diagnostic.code,
                        "field": diagnostic.field.trim_start_matches('/'),
                        "message": diagnostic.message,
                    })
                })
                .chain(record.diagnostics.iter().map(diagnostic_value))
                .collect::<Vec<_>>();
            Ok(CaseExecution::Actual(serde_json::json!({
                "valid": case.operation != "validate" || record.is_valid(),
                "types": record.types,
                "issues": issues,
                "frontmatter": record.frontmatter,
                "effective_frontmatter": record.effective_frontmatter,
            })))
        }
        "get_type" => {
            let name = required_yaml_string(input, "name")?;
            let definition = types
                .get(name)
                .ok_or_else(|| AppError::operation(format!("type not found: {name}")))?;
            Ok(CaseExecution::Actual(serde_json::json!({
                "valid": true,
                "type": definition.frontmatter,
            })))
        }
        "create" => execute_create(&types, input),
        "data_contract_implementation_validate" => {
            execute_contract_validation(&collection, &types, &contracts, input)
        }
        "data_contract_digest" => {
            let contract = contracts
                .iter()
                .next()
                .ok_or_else(|| AppError::operation("contract fixture did not load"))?;
            Ok(CaseExecution::Actual(serde_json::json!({
                "digest": contract.digest,
            })))
        }
        "data_contract_implementation_digest" => {
            let contract = contracts
                .iter()
                .next()
                .ok_or_else(|| AppError::operation("contract fixture did not load"))?;
            let implementation = contracts
                .implementations(&contract.identity.id, &contract.identity.version)
                .first()
                .ok_or_else(|| AppError::operation("contract implementation did not load"))?;
            Ok(CaseExecution::Actual(serde_json::json!({
                "digest": implementation.digest,
            })))
        }
        "data_contract_registry_validate" => {
            let error = contracts
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    format!(
                        "{}: {}",
                        diagnostic.code.replace('_', " "),
                        diagnostic.message
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            Ok(CaseExecution::Actual(serde_json::json!({
                "valid": contracts.diagnostics.is_empty(),
                "error": error,
            })))
        }
        operation => Ok(CaseExecution::Unsupported(format!(
            "fixture operation `{operation}` is not implemented"
        ))),
    }
}

fn execute_create(
    types: &vulcan_core::mdbase::MdbaseTypeRegistry,
    input: &serde_yaml::Mapping,
) -> Result<CaseExecution, AppError> {
    if let Some(path) = yaml_string(input, "path") {
        return Ok(
            match normalize_relative_input_path(
                path,
                RelativePathOptions {
                    expected_extension: None,
                    append_extension_if_missing: false,
                },
            ) {
                Ok(path) => CaseExecution::Actual(serde_json::json!({"valid": true, "path": path})),
                Err(_) => CaseExecution::Actual(serde_json::json!({
                    "valid": false,
                    "error": {"code": "path_traversal"},
                })),
            },
        );
    }
    let type_name = required_yaml_string(input, "type")?;
    let frontmatter = yaml_to_json(
        input
            .get(serde_yaml::Value::String("frontmatter".to_string()))
            .ok_or_else(|| AppError::operation("create frontmatter is missing"))?,
    )?;
    let behavior = compose_mdbase_type_behavior(types, &[type_name.to_string()]);
    let pattern = behavior
        .path
        .as_ref()
        .and_then(|path| path.get("pattern"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AppError::operation("type has no portable path pattern"))?;
    match render_mdbase_path_pattern(pattern, &frontmatter) {
        Ok(path) => Ok(CaseExecution::Actual(
            serde_json::json!({"valid": true, "path": path}),
        )),
        Err(error) => Ok(CaseExecution::Actual(serde_json::json!({
            "valid": false,
            "error": {"code": error.code},
        }))),
    }
}

fn execute_contract_validation(
    collection: &vulcan_core::mdbase::MdbaseCollection,
    types: &vulcan_core::mdbase::MdbaseTypeRegistry,
    contracts: &vulcan_core::mdbase::MdbaseContractRegistry,
    input: &serde_yaml::Mapping,
) -> Result<CaseExecution, AppError> {
    let mut errors = types
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .chain(
            contracts
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone()),
        )
        .collect::<Vec<_>>();
    if let Some(record_path) = yaml_string(input, "record") {
        let contents = bundled_reference(record_path)
            .ok_or_else(|| AppError::operation(format!("unbundled fixture: {record_path}")))?;
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(contents).map_err(AppError::operation)?;
        let frontmatter = yaml_to_json(&yaml)?;
        let contract = contracts
            .iter()
            .next()
            .ok_or_else(|| AppError::operation("contract fixture did not load"))?;
        let implementation = contracts
            .implementations(&contract.identity.id, &contract.identity.version)
            .first()
            .ok_or_else(|| AppError::operation("contract implementation did not load"))?;
        let view = project_mdbase_contract_view(
            collection,
            contracts,
            &contract.identity.id,
            &contract.identity.version,
            &implementation.type_name,
            &frontmatter,
        );
        errors.extend(
            view.diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.message),
        );
    }
    Ok(CaseExecution::Actual(serde_json::json!({
        "valid": errors.is_empty(),
        "error": errors.join("; "),
    })))
}

fn assert_expectation(
    actual: &serde_json::Value,
    expected: &serde_yaml::Value,
) -> Result<(), String> {
    let expected = yaml_to_json(expected).map_err(|error| error.to_string())?;
    let expected = expected
        .as_object()
        .ok_or_else(|| "expected result must be a mapping".to_string())?;
    for (key, expected_value) in expected {
        match key.as_str() {
            "issues" => assert_issue_subset(actual.get("issues"), expected_value)?,
            "effective_frontmatter" | "type" | "error" => {
                assert_json_subset(actual.get(key), expected_value, key)?;
            }
            "frontmatter_not_contains" => {
                let frontmatter = actual
                    .get("frontmatter")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| "actual frontmatter is missing".to_string())?;
                for field in expected_value.as_array().into_iter().flatten() {
                    if field
                        .as_str()
                        .is_some_and(|field| frontmatter.contains_key(field))
                    {
                        return Err(format!("frontmatter unexpectedly contains {field}"));
                    }
                }
            }
            "error_contains" => {
                let needle = expected_value
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let haystack = actual
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !haystack.contains(&needle) {
                    return Err(format!(
                        "expected error containing `{needle}`, got `{haystack}`"
                    ));
                }
            }
            _ if actual.get(key) != Some(expected_value) => {
                return Err(format!(
                    "expected {key}={expected_value}, got {}",
                    actual.get(key).unwrap_or(&serde_json::Value::Null)
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn assert_issue_subset(
    actual: Option<&serde_json::Value>,
    expected: &serde_json::Value,
) -> Result<(), String> {
    let actual = actual
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "actual issues are missing".to_string())?;
    let expected = expected
        .as_array()
        .ok_or_else(|| "expected issues must be an array".to_string())?;
    for issue in expected {
        if !actual
            .iter()
            .any(|candidate| json_is_subset(candidate, issue))
        {
            return Err(format!("expected issue {issue} was absent from {actual:?}"));
        }
    }
    if expected.is_empty() && !actual.is_empty() {
        return Err(format!("expected no issues, got {actual:?}"));
    }
    Ok(())
}

fn assert_json_subset(
    actual: Option<&serde_json::Value>,
    expected: &serde_json::Value,
    label: &str,
) -> Result<(), String> {
    if actual.is_some_and(|actual| json_is_subset(actual, expected)) {
        Ok(())
    } else {
        Err(format!(
            "expected {label} subset {expected}, got {}",
            actual.unwrap_or(&serde_json::Value::Null)
        ))
    }
}

fn json_is_subset(actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
    match (actual, expected) {
        (serde_json::Value::Object(actual), serde_json::Value::Object(expected)) => {
            expected.iter().all(|(key, expected)| {
                actual
                    .get(key)
                    .is_some_and(|actual| json_is_subset(actual, expected))
            })
        }
        _ => actual == expected,
    }
}

fn diagnostic_value(diagnostic: &MdbaseRecordDiagnostic) -> serde_json::Value {
    serde_json::json!({
        "code": diagnostic.code,
        "field": diagnostic.field.trim_start_matches('/'),
    })
}

fn write_fixture(root: &Path, relative: &str, contents: &str) -> Result<(), AppError> {
    let relative = normalize_relative_input_path(
        relative,
        RelativePathOptions {
            expected_extension: None,
            append_extension_if_missing: false,
        },
    )
    .map_err(AppError::operation)?;
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    fs::write(path, contents).map_err(AppError::operation)
}

fn bundled_reference(path: &str) -> Option<&'static str> {
    match path {
        "examples/v0.3/tasknotes-migration/v0.3/_contracts/tasknotes.task.md" => {
            Some(MDBASE_V03_TASKNOTES_CONTRACT)
        }
        "examples/v0.3/tasknotes-migration/v0.3/_types/task.md" => Some(MDBASE_V03_TASKNOTES_TYPE),
        "tests/v0.3/fixtures/data-contracts/valid-task.yml" => Some(MDBASE_V03_VALID_TASK),
        "tests/v0.3/fixtures/data-contracts/conflicting-tasknotes.task.md" => {
            Some(MDBASE_V03_CONFLICTING_TASKNOTES_CONTRACT)
        }
        _ => None,
    }
}

fn basename(path: &str) -> Result<&str, AppError> {
    path.rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::operation(format!("fixture path has no filename: {path}")))
}

fn yaml_string<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a str> {
    mapping
        .get(serde_yaml::Value::String(key.to_string()))
        .and_then(serde_yaml::Value::as_str)
}

fn required_yaml_string<'a>(
    mapping: &'a serde_yaml::Mapping,
    key: &str,
) -> Result<&'a str, AppError> {
    yaml_string(mapping, key)
        .ok_or_else(|| AppError::operation(format!("fixture input `{key}` is missing")))
}

fn yaml_to_json(value: &serde_yaml::Value) -> Result<serde_json::Value, AppError> {
    serde_json::to_value(value).map_err(AppError::operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_core_read_and_collection_semantics_fixtures_pass() {
        let report = run_mdbase_core_read_conformance().expect("conformance runs");
        let failures = report
            .cases
            .iter()
            .filter(|case| case.status != MdbaseConformanceCaseStatus::Pass)
            .collect::<Vec<_>>();
        assert!(failures.is_empty(), "fixture failures: {failures:#?}");
        assert!(report.valid, "profile gaps: {:#?}", report.profiles);
        assert!(report.cases.iter().any(|case| case.id == "core.valid_task"));
        assert!(report
            .cases
            .iter()
            .any(|case| case.id == "data-contract-projection"));
        assert!(report
            .profiles
            .iter()
            .any(|profile| profile.profile == "cel" && !profile.evaluated && !profile.supported));
    }

    #[test]
    fn canonical_claim_is_schema_valid_and_pins_passing_evidence() {
        let report = run_mdbase_core_read_conformance().expect("conformance runs");
        let claim = build_mdbase_conformance_claim_at(&report, "2026-09-06T12:00:00Z")
            .expect("passing report should produce a claim");

        assert_eq!(claim.kind, "mdbase.conformance");
        assert_eq!(claim.status, "verified");
        assert_eq!(claim.profiles, TARGET_PROFILES);
        assert_eq!(claim.evidence[0].result, "pass");
        assert!(claim.evidence[0]
            .artifact
            .contains(MDBASE_SPEC_UPSTREAM_COMMIT));
    }

    #[test]
    fn conformance_claim_is_withheld_after_any_required_failure() {
        let mut report = run_mdbase_core_read_conformance().expect("conformance runs");
        report.cases[0].status = MdbaseConformanceCaseStatus::Fail;

        let error = build_mdbase_conformance_claim_at(&report, "2026-09-06T12:00:00Z")
            .expect_err("failed evidence must not produce a claim");
        assert!(error.to_string().contains("claim withheld"));
    }

    #[test]
    fn unsupported_operations_are_never_counted_as_passes() {
        let case = FixtureCase {
            id: Some("test.unsupported".to_string()),
            name: "unsupported".to_string(),
            operation: "future_operation".to_string(),
            input: serde_yaml::from_str("{}\n").expect("input"),
            expect: serde_yaml::from_str("valid: true\n").expect("expect"),
            covers: vec!["core_read.future".to_string()],
        };
        let directory = tempfile::tempdir().expect("temp dir");
        materialize_setup(directory.path(), None).expect("setup");
        assert!(matches!(
            execute_case(directory.path(), &case).expect("execution"),
            CaseExecution::Unsupported(_)
        ));
    }
}
