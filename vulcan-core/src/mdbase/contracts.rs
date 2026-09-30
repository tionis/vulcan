use super::{
    bundled_mdbase_schema, compile_mdbase_schema_wrapper, discover_control_files,
    validate_mdbase_schema_value_with_local_refs, MdbaseCollection, MdbaseCompiledSchema,
    MdbaseTypeDefinition, MdbaseTypeRegistry, MDBASE_CANONICAL_SCHEMA_BASE,
};
use crate::config::VaultConfig;
use crate::parser::parse_document;
use crate::paths::secure_read_to_string;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MdbaseContractType {
    Record,
    Event,
    Action,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MdbaseContractIdentity {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseContractDefinition {
    pub identity: MdbaseContractIdentity,
    pub contract_type: MdbaseContractType,
    pub path: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub schemas: BTreeMap<String, serde_json::Value>,
    pub behavior: Option<serde_json::Value>,
    pub digest: String,
    pub frontmatter: serde_json::Value,
    validation_schemas: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    compiled_schemas: BTreeMap<String, Arc<MdbaseCompiledSchema>>,
}

impl MdbaseContractDefinition {
    /// Immutable validator and exact reference dependencies captured at load.
    #[must_use]
    pub fn compiled_schema(&self, name: &str) -> Option<&MdbaseCompiledSchema> {
        self.compiled_schemas.get(name).map(Arc::as_ref)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MdbaseContractImplementation {
    pub contract: MdbaseContractIdentity,
    pub contract_digest: String,
    pub type_name: String,
    pub type_path: String,
    pub fields: BTreeMap<String, String>,
    pub binding: Option<serde_json::Value>,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseContractDiagnostic {
    pub code: String,
    pub message: String,
    pub path: String,
    pub field: String,
    pub contract_id: Option<String>,
    pub contract_version: Option<String>,
    pub type_name: Option<String>,
    pub related_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MdbaseContractRegistry {
    contracts: BTreeMap<MdbaseContractIdentity, MdbaseContractDefinition>,
    implementations: BTreeMap<MdbaseContractIdentity, Vec<MdbaseContractImplementation>>,
    pub diagnostics: Vec<MdbaseContractDiagnostic>,
}

impl MdbaseContractRegistry {
    #[must_use]
    pub fn get(&self, id: &str, version: &str) -> Option<&MdbaseContractDefinition> {
        self.contracts.get(&MdbaseContractIdentity {
            id: id.to_string(),
            version: version.to_string(),
        })
    }

    #[must_use]
    pub fn implementations(&self, id: &str, version: &str) -> &[MdbaseContractImplementation] {
        self.implementations
            .get(&MdbaseContractIdentity {
                id: id.to_string(),
                version: version.to_string(),
            })
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn iter(&self) -> impl Iterator<Item = &MdbaseContractDefinition> {
        self.contracts.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.contracts.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.contracts.is_empty()
    }
}

#[derive(Debug)]
pub enum MdbaseContractRegistryError {
    PermissionDenied,
    Discovery(super::MdbaseDiscoveryError),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    BundledSchema(serde_json::Error),
}

impl Display for MdbaseContractRegistryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PermissionDenied => {
                formatter.write_str("permission denied for required mdbase controls")
            }
            Self::Discovery(error) => {
                write!(formatter, "failed to discover mdbase contracts: {error}")
            }
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "failed to read mdbase contract {}: {source}",
                    path.display()
                )
            }
            Self::BundledSchema(error) => {
                write!(
                    formatter,
                    "bundled mdbase contract schema is invalid: {error}"
                )
            }
        }
    }
}

impl std::error::Error for MdbaseContractRegistryError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseContractView {
    pub contract: MdbaseContractIdentity,
    pub contract_digest: String,
    pub implementation_digest: String,
    pub type_name: String,
    pub binding: Option<serde_json::Value>,
    pub view: serde_json::Value,
    pub diagnostics: Vec<MdbaseContractDiagnostic>,
}

/// Load exact-version collection contracts and validate every type implementation.
pub fn load_mdbase_contract_registry(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
) -> Result<MdbaseContractRegistry, MdbaseContractRegistryError> {
    load_mdbase_contract_registry_authorized(collection, types, None)
}

/// Load complete contract controls with pre-discovery and pre-read checks.
pub fn load_mdbase_contract_registry_authorized(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    filter: Option<&crate::permissions::PermissionFilter>,
) -> Result<MdbaseContractRegistry, MdbaseContractRegistryError> {
    let access = super::control_access::ControlAccess::new(filter);
    let folder = &collection.config.settings.contracts_folder;
    if !access.folder_allowed(folder) {
        return Err(MdbaseContractRegistryError::PermissionDenied);
    }
    let mut paths = Vec::new();
    discover_control_files(&collection.root, folder, &mut paths)
        .map_err(MdbaseContractRegistryError::Discovery)?;
    build_mdbase_contract_registry(collection, types, &paths, &access)
}

fn build_mdbase_contract_registry(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    contract_files: &[String],
    access: &super::control_access::ControlAccess<'_>,
) -> Result<MdbaseContractRegistry, MdbaseContractRegistryError> {
    let canonical = format!("{MDBASE_CANONICAL_SCHEMA_BASE}data-contract.schema.json");
    let bundled = bundled_mdbase_schema(&canonical).expect("data-contract schema is bundled");
    let contract_schema = serde_json::from_str::<serde_json::Value>(bundled.json)
        .map_err(MdbaseContractRegistryError::BundledSchema)?;
    let mut candidates = BTreeMap::<MdbaseContractIdentity, Vec<MdbaseContractDefinition>>::new();
    let mut diagnostics = Vec::new();
    let mut paths = contract_files.to_vec();
    paths.sort();
    paths.dedup();
    for path in paths {
        if !access.path_allowed(&path) {
            return Err(MdbaseContractRegistryError::PermissionDenied);
        }
        let result = load_contract_file(collection, &path, &contract_schema, access)?;
        if access.denied() {
            return Err(MdbaseContractRegistryError::PermissionDenied);
        }
        match result {
            Ok(contract) => candidates
                .entry(contract.identity.clone())
                .or_default()
                .push(contract),
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }

    let mut contracts = BTreeMap::new();
    let mut conflicted = BTreeSet::new();
    for (identity, mut definitions) in candidates {
        definitions.sort_by(|left, right| left.path.cmp(&right.path));
        if definitions
            .iter()
            .all(|definition| definition.digest == definitions[0].digest)
        {
            contracts.insert(identity, definitions.remove(0));
            continue;
        }
        conflicted.insert(identity.clone());
        let paths = definitions
            .iter()
            .map(|definition| definition.path.clone())
            .collect::<Vec<_>>();
        for definition in definitions {
            diagnostics.push(contract_diagnostic(
                "data_contract_conflict",
                format!(
                    "data contract {}@{} has conflicting portable content",
                    identity.id, identity.version
                ),
                &definition.path,
                "",
                Some(&identity),
                None,
                paths
                    .iter()
                    .filter(|path| *path != &definition.path)
                    .cloned()
                    .collect(),
            ));
        }
    }

    let mut registry = MdbaseContractRegistry {
        contracts,
        implementations: BTreeMap::new(),
        diagnostics,
    };
    validate_type_implementations(types, &conflicted, &mut registry);
    for implementations in registry.implementations.values_mut() {
        implementations.sort_by(|left, right| {
            left.type_name
                .to_ascii_lowercase()
                .cmp(&right.type_name.to_ascii_lowercase())
        });
    }
    sort_contract_diagnostics(&mut registry.diagnostics);
    Ok(registry)
}

fn load_contract_file(
    collection: &MdbaseCollection,
    path: &str,
    contract_schema: &serde_json::Value,
    access: &super::control_access::ControlAccess<'_>,
) -> Result<Result<MdbaseContractDefinition, MdbaseContractDiagnostic>, MdbaseContractRegistryError>
{
    let source = secure_read_to_string(&collection.root, Path::new(path)).map_err(|source| {
        MdbaseContractRegistryError::Read {
            path: collection.root.join(path),
            source,
        }
    })?;
    let frontmatter = match parse_contract_frontmatter(&source, path) {
        Ok(frontmatter) => frontmatter,
        Err(diagnostic) => return Ok(Err(*diagnostic)),
    };
    let absolute_path = collection.root.join(path);
    if let Some(diagnostic) = validate_contract_envelope(
        collection,
        path,
        &absolute_path,
        contract_schema,
        &frontmatter,
    ) {
        return Ok(Err(diagnostic));
    }
    let identity = contract_identity_from_value(&frontmatter)
        .expect("validated contract should have an identity");
    let (contract_type, schemas, validation_schemas, compiled_schemas) = match load_contract_schemas(
        collection,
        path,
        &absolute_path,
        &frontmatter,
        &identity,
        access,
    ) {
        Ok(schemas) => schemas,
        Err(diagnostic) => return Ok(Err(*diagnostic)),
    };
    let behavior = frontmatter.get("behavior").cloned();
    let digest = contract_digest(contract_type, &identity, &schemas, behavior.as_ref());
    Ok(Ok(MdbaseContractDefinition {
        identity,
        contract_type,
        path: path.to_string(),
        name: frontmatter
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        description: frontmatter
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        schemas,
        behavior,
        digest,
        frontmatter,
        validation_schemas,
        compiled_schemas,
    }))
}

fn validate_contract_envelope(
    collection: &MdbaseCollection,
    path: &str,
    absolute_path: &Path,
    contract_schema: &serde_json::Value,
    frontmatter: &serde_json::Value,
) -> Option<MdbaseContractDiagnostic> {
    match validate_mdbase_schema_value_with_local_refs(
        contract_schema,
        frontmatter,
        absolute_path,
        &collection.root,
    ) {
        Ok(diagnostics) if !diagnostics.is_empty() => Some(contract_diagnostic(
            "invalid_data_contract",
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            path,
            &diagnostics[0].instance_path,
            contract_identity_from_value(frontmatter).as_ref(),
            None,
            Vec::new(),
        )),
        Err(error) => Some(contract_diagnostic(
            "invalid_data_contract",
            format!("failed to compile data-contract schema: {error}"),
            path,
            "",
            contract_identity_from_value(frontmatter).as_ref(),
            None,
            Vec::new(),
        )),
        Ok(_) => None,
    }
}

type LoadedContractSchemas = (
    MdbaseContractType,
    BTreeMap<String, serde_json::Value>,
    BTreeMap<String, serde_json::Value>,
    BTreeMap<String, Arc<MdbaseCompiledSchema>>,
);

fn load_contract_schemas(
    collection: &MdbaseCollection,
    path: &str,
    absolute_path: &Path,
    frontmatter: &serde_json::Value,
    identity: &MdbaseContractIdentity,
    access: &super::control_access::ControlAccess<'_>,
) -> Result<LoadedContractSchemas, Box<MdbaseContractDiagnostic>> {
    let contract_type = match frontmatter["contract_type"].as_str() {
        Some("record") => MdbaseContractType::Record,
        Some("event") => MdbaseContractType::Event,
        Some("action") => MdbaseContractType::Action,
        _ => unreachable!("validated contract type should be known"),
    };
    let mut schemas = BTreeMap::new();
    let mut validation_schemas = BTreeMap::new();
    let mut compiled_schemas = BTreeMap::new();
    for key in subject_schema_keys(contract_type) {
        let Some(wrapper) = frontmatter.get(key) else {
            continue;
        };
        let (resolved, compiled) =
            compile_mdbase_schema_wrapper(wrapper, absolute_path, &collection.root, &|path| {
                access.schema(path)
            })
            .map_err(|error| {
                Box::new(contract_diagnostic(
                    "invalid_data_contract",
                    format!("failed to resolve or compile `{key}`: {error}"),
                    path,
                    key,
                    Some(identity),
                    None,
                    Vec::new(),
                ))
            })?;
        // Preserve the existing report representation; execution uses only the
        // compiled snapshot, never this serialized wrapper.
        let validation_schema = wrapper
            .get("value")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"$ref":wrapper["ref"]}));
        schemas.insert((*key).to_string(), resolved);
        validation_schemas.insert((*key).to_string(), validation_schema);
        compiled_schemas.insert((*key).to_string(), Arc::new(compiled));
    }
    Ok((contract_type, schemas, validation_schemas, compiled_schemas))
}

fn parse_contract_frontmatter(
    source: &str,
    path: &str,
) -> Result<serde_json::Value, Box<MdbaseContractDiagnostic>> {
    let parsed = parse_document(source, &VaultConfig::default());
    let raw = parsed.raw_frontmatter.ok_or_else(|| {
        Box::new(contract_diagnostic(
            "invalid_data_contract",
            "mdbase contract files require leading YAML frontmatter",
            path,
            "",
            None,
            None,
            Vec::new(),
        ))
    })?;
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(&raw).map_err(|error| {
        Box::new(contract_diagnostic(
            "invalid_data_contract",
            format!("failed to parse contract frontmatter: {error}"),
            path,
            "",
            None,
            None,
            Vec::new(),
        ))
    })?;
    serde_json::to_value(yaml).map_err(|error| {
        Box::new(contract_diagnostic(
            "invalid_data_contract",
            format!("contract frontmatter is not JSON-compatible: {error}"),
            path,
            "",
            None,
            None,
            Vec::new(),
        ))
    })
}

fn subject_schema_keys(contract_type: MdbaseContractType) -> &'static [&'static str] {
    match contract_type {
        MdbaseContractType::Record => &["record_schema", "binding_schema"],
        MdbaseContractType::Event => &["data_schema", "source_schema"],
        MdbaseContractType::Action => &[
            "input_schema",
            "output_schema",
            "error_schema",
            "provider_schema",
        ],
    }
}

fn contract_identity_from_value(value: &serde_json::Value) -> Option<MdbaseContractIdentity> {
    Some(MdbaseContractIdentity {
        id: value.get("id")?.as_str()?.to_string(),
        version: value.get("version")?.as_str()?.to_string(),
    })
}

fn contract_digest(
    contract_type: MdbaseContractType,
    identity: &MdbaseContractIdentity,
    schemas: &BTreeMap<String, serde_json::Value>,
    behavior: Option<&serde_json::Value>,
) -> String {
    let kind = match contract_type {
        MdbaseContractType::Record => "record",
        MdbaseContractType::Event => "event",
        MdbaseContractType::Action => "action",
    };
    let mut value = serde_json::Map::new();
    value.insert("kind".to_string(), serde_json::json!("mdbase.contract"));
    value.insert("contract_type".to_string(), serde_json::json!(kind));
    value.insert("id".to_string(), serde_json::json!(identity.id));
    value.insert("version".to_string(), serde_json::json!(identity.version));
    for (key, schema) in schemas {
        value.insert(key.clone(), schema.clone());
    }
    if let Some(behavior) = behavior {
        value.insert("behavior".to_string(), behavior.clone());
    }
    sha256_jcs(&serde_json::Value::Object(value))
}

fn sha256_jcs(value: &serde_json::Value) -> String {
    let bytes = serde_json_canonicalizer::to_vec(value)
        .expect("JSON values should have an RFC 8785 representation");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn validate_type_implementations(
    types: &MdbaseTypeRegistry,
    conflicted: &BTreeSet<MdbaseContractIdentity>,
    registry: &mut MdbaseContractRegistry,
) {
    for definition in types.iter() {
        let Some(entries) = definition
            .frontmatter
            .get("implements")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        let mut seen = BTreeSet::new();
        for (index, entry) in entries.iter().enumerate() {
            let identity = MdbaseContractIdentity {
                id: entry["contract"]
                    .as_str()
                    .expect("validated contract id")
                    .to_string(),
                version: entry["version"]
                    .as_str()
                    .expect("validated version")
                    .to_string(),
            };
            let field = format!("implements[{index}]");
            if !seen.insert(identity.clone()) {
                registry.diagnostics.push(contract_diagnostic(
                    "data_contract_field_invalid",
                    "a type cannot implement the same exact contract more than once",
                    &definition.path,
                    &field,
                    Some(&identity),
                    Some(&definition.name),
                    Vec::new(),
                ));
                continue;
            }
            if conflicted.contains(&identity) {
                continue;
            }
            let Some(contract) = registry.contracts.get(&identity) else {
                registry.diagnostics.push(contract_diagnostic(
                    "data_contract_not_found",
                    format!(
                        "no local contract {}@{} exists",
                        identity.id, identity.version
                    ),
                    &definition.path,
                    &field,
                    Some(&identity),
                    Some(&definition.name),
                    Vec::new(),
                ));
                continue;
            };
            match validate_implementation(definition, contract, entry, &field) {
                Ok(implementation) => registry
                    .implementations
                    .entry(identity)
                    .or_default()
                    .push(implementation),
                Err(mut diagnostics) => registry.diagnostics.append(&mut diagnostics),
            }
        }
    }
}

fn validate_implementation(
    definition: &MdbaseTypeDefinition,
    contract: &MdbaseContractDefinition,
    entry: &serde_json::Value,
    field: &str,
) -> Result<MdbaseContractImplementation, Vec<MdbaseContractDiagnostic>> {
    let identity = &contract.identity;
    if contract.contract_type != MdbaseContractType::Record {
        return Err(vec![contract_diagnostic(
            "data_contract_field_invalid",
            "types can implement only record contracts",
            &definition.path,
            field,
            Some(identity),
            Some(&definition.name),
            Vec::new(),
        )]);
    }
    let fields = entry["fields"]
        .as_object()
        .expect("validated implementation fields")
        .iter()
        .map(|(contract, record)| {
            (
                contract.clone(),
                record
                    .as_str()
                    .expect("validated record selector")
                    .to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut diagnostics =
        validate_field_map(definition, contract, &definition.schema, &fields, field);
    let binding = entry.get("binding");
    if let Some(diagnostic) = validate_implementation_binding(definition, contract, binding, field)
    {
        diagnostics.push(diagnostic);
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    Ok(build_contract_implementation(
        definition, contract, entry, fields, binding,
    ))
}

fn validate_implementation_binding(
    definition: &MdbaseTypeDefinition,
    contract: &MdbaseContractDefinition,
    binding: Option<&serde_json::Value>,
    field: &str,
) -> Option<MdbaseContractDiagnostic> {
    let identity = &contract.identity;
    if let Some(binding_schema) = contract.compiled_schema("binding_schema") {
        let empty = serde_json::json!({});
        let errors = binding_schema.validate(binding.unwrap_or(&empty));
        return (!errors.is_empty()).then(|| {
            contract_diagnostic(
                "data_contract_binding_invalid",
                errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
                &definition.path,
                &format!("{field}.binding"),
                Some(identity),
                Some(&definition.name),
                Vec::new(),
            )
        });
    }
    binding
        .is_some_and(|binding| {
            binding
                .as_object()
                .is_none_or(|binding| !binding.is_empty())
        })
        .then(|| {
            contract_diagnostic(
                "data_contract_binding_invalid",
                "binding must be absent or empty when the contract has no binding schema",
                &definition.path,
                &format!("{field}.binding"),
                Some(identity),
                Some(&definition.name),
                Vec::new(),
            )
        })
}

fn build_contract_implementation(
    definition: &MdbaseTypeDefinition,
    contract: &MdbaseContractDefinition,
    entry: &serde_json::Value,
    fields: BTreeMap<String, String>,
    binding: Option<&serde_json::Value>,
) -> MdbaseContractImplementation {
    let identity = &contract.identity;
    let portable_type = portable_type_semantics(definition);
    let digest = sha256_jcs(&serde_json::json!({
        "contract_digest": contract.digest,
        "type": portable_type,
        "implementation": entry,
    }));
    MdbaseContractImplementation {
        contract: identity.clone(),
        contract_digest: contract.digest.clone(),
        type_name: definition.name.clone(),
        type_path: definition.path.clone(),
        fields,
        binding: binding.cloned(),
        digest,
    }
}

fn portable_type_semantics(definition: &MdbaseTypeDefinition) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    value.insert("name".to_string(), serde_json::json!(definition.name));
    if let Some(version) = definition.version {
        value.insert("version".to_string(), serde_json::json!(version));
    }
    for key in ["match", "collection", "lifecycle"] {
        if let Some(member) = definition.frontmatter.get(key) {
            value.insert(key.to_string(), member.clone());
        }
    }
    value.insert(
        "schema".to_string(),
        definition
            .frontmatter
            .get("schema")
            .expect("validated type should contain schema")
            .clone(),
    );
    serde_json::Value::Object(value)
}

fn validate_field_map(
    definition: &MdbaseTypeDefinition,
    contract: &MdbaseContractDefinition,
    type_schema: &serde_json::Value,
    fields: &BTreeMap<String, String>,
    field: &str,
) -> Vec<MdbaseContractDiagnostic> {
    let contract_schema = &contract.schemas["record_schema"];
    let mut diagnostics = Vec::new();
    for required in unconditionally_required_fields(contract_schema) {
        let pointer = format!("/{}", required.replace('~', "~0").replace('/', "~1"));
        if !fields.contains_key(required) && !fields.contains_key(&pointer) {
            diagnostics.push(implementation_field_diagnostic(
                definition,
                contract,
                field,
                format!("required contract field `{required}` is not mapped"),
            ));
        }
    }
    for (contract_field, record_field) in fields {
        if !schema_declares_selector(contract_schema, contract_field) {
            diagnostics.push(implementation_field_diagnostic(
                definition,
                contract,
                field,
                format!("contract field `{contract_field}` is not declared"),
            ));
        }
        if !schema_declares_selector(type_schema, record_field) {
            diagnostics.push(implementation_field_diagnostic(
                definition,
                contract,
                field,
                format!("record field `{record_field}` is not declared by the type schema"),
            ));
        }
    }
    diagnostics
}

fn unconditionally_required_fields(schema: &serde_json::Value) -> BTreeSet<&str> {
    let mut required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<BTreeSet<_>>();
    if let Some(branches) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        for branch in branches {
            required.extend(unconditionally_required_fields(branch));
        }
    }
    required
}

fn schema_declares_selector(schema: &serde_json::Value, selector: &str) -> bool {
    let components = if let Some(pointer) = selector.strip_prefix('/') {
        pointer
            .split('/')
            .map(super::decode_json_pointer_token)
            .collect::<Vec<_>>()
    } else {
        selector
            .split('.')
            .map(|value| value.strip_suffix("[]").unwrap_or(value).to_string())
            .collect::<Vec<_>>()
    };
    let mut current = schema;
    for component in components {
        let Some(next) = schema_property(current, &component) else {
            return false;
        };
        current = next;
    }
    true
}

fn schema_property<'a>(schema: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    if let Some(property) = schema
        .get("properties")
        .and_then(|properties| properties.get(name))
    {
        return Some(property);
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(property) = schema
            .get(keyword)
            .and_then(serde_json::Value::as_array)
            .and_then(|branches| {
                branches
                    .iter()
                    .find_map(|branch| schema_property(branch, name))
            })
        {
            return Some(property);
        }
    }
    schema
        .get("items")
        .and_then(|items| schema_property(items, name))
}

fn implementation_field_diagnostic(
    definition: &MdbaseTypeDefinition,
    contract: &MdbaseContractDefinition,
    field: &str,
    message: String,
) -> MdbaseContractDiagnostic {
    contract_diagnostic(
        "data_contract_field_invalid",
        message,
        &definition.path,
        field,
        Some(&contract.identity),
        Some(&definition.name),
        Vec::new(),
    )
}

/// Project one implementing type's normalized view from effective frontmatter.
#[must_use]
pub fn project_mdbase_contract_view(
    _collection: &MdbaseCollection,
    registry: &MdbaseContractRegistry,
    id: &str,
    version: &str,
    type_name: &str,
    effective_frontmatter: &serde_json::Value,
) -> MdbaseContractView {
    let identity = MdbaseContractIdentity {
        id: id.to_string(),
        version: version.to_string(),
    };
    let contract = registry.get(id, version);
    let implementation = registry
        .implementations(id, version)
        .iter()
        .find(|implementation| implementation.type_name.eq_ignore_ascii_case(type_name));
    let Some((contract, implementation)) = contract.zip(implementation) else {
        return MdbaseContractView {
            contract: identity.clone(),
            contract_digest: contract.map_or_else(String::new, |value| value.digest.clone()),
            implementation_digest: String::new(),
            type_name: type_name.to_string(),
            binding: None,
            view: serde_json::json!({}),
            diagnostics: vec![contract_diagnostic(
                "data_contract_not_found",
                format!("type `{type_name}` does not implement {id}@{version}"),
                "",
                "",
                Some(&identity),
                Some(type_name),
                Vec::new(),
            )],
        };
    };
    let mut view = serde_json::json!({});
    let mut diagnostics = Vec::new();
    for (target, source) in &implementation.fields {
        let resolved = effective_frontmatter
            .as_object()
            .map(|frontmatter| super::resolve_match_field(frontmatter, source));
        let Some(resolved) = resolved.filter(|resolved| resolved.exists) else {
            continue;
        };
        let value = if resolved.values.len() == 1 && !source.contains("[]") {
            resolved.values[0].clone()
        } else {
            serde_json::Value::Array(resolved.values.into_iter().cloned().collect())
        };
        if let Err(message) = set_contract_view_value(&mut view, target, value) {
            diagnostics.push(contract_diagnostic(
                "data_contract_record_invalid",
                message,
                "",
                target,
                Some(&identity),
                Some(&implementation.type_name),
                Vec::new(),
            ));
        }
    }
    let schema = contract
        .compiled_schema("record_schema")
        .expect("valid record contract has a compiled record schema");
    diagnostics.extend(schema.validate(&view).into_iter().map(|error| {
        contract_diagnostic(
            "data_contract_record_invalid",
            error.message,
            "",
            &error.instance_path,
            Some(&identity),
            Some(&implementation.type_name),
            Vec::new(),
        )
    }));
    sort_contract_diagnostics(&mut diagnostics);
    MdbaseContractView {
        contract: identity,
        contract_digest: contract.digest.clone(),
        implementation_digest: implementation.digest.clone(),
        type_name: implementation.type_name.clone(),
        binding: implementation.binding.clone(),
        view,
        diagnostics,
    }
}

fn set_contract_view_value(
    view: &mut serde_json::Value,
    selector: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let components = if let Some(pointer) = selector.strip_prefix('/') {
        pointer
            .split('/')
            .map(super::decode_json_pointer_token)
            .collect::<Vec<_>>()
    } else {
        selector
            .split('.')
            .map(|component| {
                component
                    .strip_suffix("[]")
                    .unwrap_or(component)
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    let mut current = view;
    for component in &components[..components.len().saturating_sub(1)] {
        let object = current.as_object_mut().ok_or_else(|| {
            format!("contract field mapping `{selector}` conflicts with another mapped value")
        })?;
        current = object
            .entry(component)
            .or_insert_with(|| serde_json::json!({}));
    }
    if let Some(last) = components.last() {
        let object = current.as_object_mut().ok_or_else(|| {
            format!("contract field mapping `{selector}` conflicts with another mapped value")
        })?;
        object.insert(last.clone(), value);
    }
    Ok(())
}

fn contract_diagnostic(
    code: &str,
    message: impl Into<String>,
    path: &str,
    field: &str,
    identity: Option<&MdbaseContractIdentity>,
    type_name: Option<&str>,
    related_paths: Vec<String>,
) -> MdbaseContractDiagnostic {
    MdbaseContractDiagnostic {
        code: code.to_string(),
        message: message.into(),
        path: path.to_string(),
        field: field.to_string(),
        contract_id: identity.map(|identity| identity.id.clone()),
        contract_version: identity.map(|identity| identity.version.clone()),
        type_name: type_name.map(ToOwned::to_owned),
        related_paths,
    }
}

fn sort_contract_diagnostics(diagnostics: &mut [MdbaseContractDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.field.cmp(&right.field))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.contract_id.cmp(&right.contract_id))
            .then_with(|| left.contract_version.cmp(&right.contract_version))
            .then_with(|| left.type_name.cmp(&right.type_name))
            .then_with(|| left.message.cmp(&right.message))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::{load_mdbase_collection, load_mdbase_type_registry};
    use std::fs;
    use tempfile::tempdir;

    fn write_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file should have a parent"))
            .expect("fixture directory should be created");
        fs::write(path, contents).expect("fixture should be written");
    }

    fn frontmatter(contents: &str) -> String {
        format!("---\n{contents}---\n")
    }

    fn setup_collection(
        contracts: &[(&str, &str)],
        types: &[(&str, &str)],
    ) -> (tempfile::TempDir, MdbaseCollection, MdbaseTypeRegistry) {
        let directory = tempdir().expect("temporary collection should exist");
        write_file(directory.path(), "mdbase.yaml", "spec_version: \"0.3.0\"\n");
        for (path, contents) in contracts {
            write_file(directory.path(), path, &frontmatter(contents));
        }
        for (path, contents) in types {
            write_file(directory.path(), path, &frontmatter(contents));
        }
        let collection = load_mdbase_collection(directory.path())
            .expect("collection should load")
            .expect("collection should be detected");
        let registry = load_mdbase_type_registry(&collection).expect("types should load");
        (directory, collection, registry)
    }

    const NOTE_CONTRACT: &str = r"kind: mdbase.contract
contract_type: record
id: example.note
version: 1.0.0
name: Note
record_schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [title]
    additionalProperties: false
    properties:
      title: {type: string}
      category: {type: string}
binding_schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [mode]
    additionalProperties: false
    properties:
      mode: {enum: [personal, work]}
";

    const PERSONAL_TYPE: &str = r"kind: mdbase.type
name: personal_note
version: 1
schema:
  dialect: json-schema-2020-12
  value:
    type: object
    required: [headline]
    properties:
      headline: {type: string}
      kind: {type: string}
implements:
  - contract: example.note
    version: 1.0.0
    fields:
      title: headline
      category: kind
    binding: {mode: personal}
";

    #[test]
    fn contract_snapshots_share_validators_and_freeze_transitive_reference_bytes() {
        let (directory, collection, types) = setup_collection(
            &[(
                "_contracts/note.md",
                r"kind: mdbase.contract
contract_type: record
id: example.note
version: 1.0.0
record_schema:
  dialect: json-schema-2020-12
  ref: schemas/record.yaml#/$defs/view
binding_schema:
  dialect: json-schema-2020-12
  ref: schemas/binding.yaml
",
            )],
            &[("_types/personal.md", PERSONAL_TYPE)],
        );
        write_file(&collection.root, "_contracts/schemas/record.yaml", "$defs:\n  view:\n    type: object\n    properties:\n      title: {$ref: '#/$defs/title'}\n      category: {type: string}\n    $defs:\n      title: {$ref: title.txt}\n");
        write_file(
            &collection.root,
            "_contracts/schemas/title.txt",
            "type: string\nminLength: 3\n",
        );
        write_file(
            &collection.root,
            "_contracts/schemas/binding.yaml",
            "type: object\nproperties:\n  mode: {enum: [personal]}\n",
        );
        let registry = load_mdbase_contract_registry(&collection, &types).unwrap();
        assert!(
            registry.diagnostics.is_empty(),
            "{:?}",
            registry.diagnostics
        );
        let cloned = registry.clone();
        let contract = registry.get("example.note", "1.0.0").unwrap();
        let schema = contract.compiled_schema("record_schema").unwrap();
        assert_eq!(schema.dependencies().len(), 2);
        assert!(std::ptr::eq(
            schema,
            cloned
                .get("example.note", "1.0.0")
                .unwrap()
                .compiled_schema("record_schema")
                .unwrap()
        ));
        assert!(serde_json::to_value(contract)
            .unwrap()
            .get("compiled_schemas")
            .is_none());
        let project = |registry: &MdbaseContractRegistry| {
            project_mdbase_contract_view(
                &collection,
                registry,
                "example.note",
                "1.0.0",
                "personal_note",
                &serde_json::json!({"headline":"Hi"}),
            )
        };
        let before = project(&registry);
        assert_eq!(before.diagnostics.len(), 1);
        assert_eq!(before.diagnostics[0].code, "data_contract_record_invalid");
        write_file(
            &collection.root,
            "_contracts/schemas/title.txt",
            "type: string\nminLength: 1\n",
        );
        let fresh = load_mdbase_contract_registry(&collection, &types).unwrap();
        assert!(project(&fresh).diagnostics.is_empty());
        assert_ne!(registry, fresh);
        // Both binding validation and per-record projection are now pure reads
        // of the captured control snapshots, even after files disappear.
        directory.close().unwrap();
        assert!(validate_implementation_binding(
            types.get("personal_note").unwrap(),
            contract,
            Some(&serde_json::json!({"mode":"personal"})),
            "implements[0]"
        )
        .is_none());
        assert!(validate_implementation_binding(
            types.get("personal_note").unwrap(),
            contract,
            Some(&serde_json::json!({"mode":"other"})),
            "implements[0]"
        )
        .is_some());
        for _ in 0..10 {
            assert_eq!(project(&cloned), before);
        }
    }

    #[test]
    fn implementation_checks_use_the_existing_type_snapshot() {
        let (_directory, collection, _) =
            setup_collection(&[("_contracts/note.md", NOTE_CONTRACT)], &[]);
        let value: serde_yaml::Value = serde_yaml::from_str(PERSONAL_TYPE).unwrap();
        let mut value = serde_json::to_value(value).unwrap();
        let schema = value["schema"]["value"].take();
        value["schema"] = serde_json::json!({"dialect":"json-schema-2020-12","ref":"shape.yaml"});
        write_file(
            &collection.root,
            "_types/shape.yaml",
            &serde_yaml::to_string(&schema).unwrap(),
        );
        write_file(
            &collection.root,
            "_types/personal.md",
            &frontmatter(&serde_yaml::to_string(&value).unwrap()),
        );
        let types = load_mdbase_type_registry(&collection).unwrap();
        assert!(types.diagnostics.is_empty());
        fs::remove_file(collection.root.join("_types/shape.yaml")).unwrap();
        let contracts = load_mdbase_contract_registry(&collection, &types).unwrap();
        assert!(
            contracts.diagnostics.is_empty(),
            "{:?}",
            contracts.diagnostics
        );
        assert_eq!(contracts.implementations("example.note", "1.0.0").len(), 1);
    }

    #[test]
    fn registry_loads_exact_versions_and_valid_implementations() {
        let (_directory, collection, types) = setup_collection(
            &[
                ("_contracts/note-v1.md", NOTE_CONTRACT),
                (
                    "_contracts/note-v1-copy.md",
                    &NOTE_CONTRACT.replace("name: Note", "name: Same semantics"),
                ),
                (
                    "_contracts/note-v2.md",
                    &NOTE_CONTRACT.replace("1.0.0", "2.0.0"),
                ),
            ],
            &[
                ("_types/personal.md", PERSONAL_TYPE),
                (
                    "_types/work.md",
                    &PERSONAL_TYPE
                        .replace("personal_note", "work_note")
                        .replace("mode: personal", "mode: work"),
                ),
            ],
        );

        let registry = load_mdbase_contract_registry(&collection, &types)
            .expect("contract registry should load");

        assert_eq!(registry.len(), 2);
        assert!(registry.diagnostics.is_empty());
        let contract = registry
            .get("example.note", "1.0.0")
            .expect("exact contract should exist");
        assert!(contract.digest.starts_with("sha256:"));
        assert_eq!(contract.digest.len(), 71);
        assert!(registry.get("example.note", "1.1.0").is_none());
        let implementations = registry.implementations("example.note", "1.0.0");
        assert_eq!(implementations.len(), 2);
        assert_eq!(implementations[0].type_name, "personal_note");
        assert_eq!(implementations[1].type_name, "work_note");
        assert!(implementations[0].digest.starts_with("sha256:"));
    }

    #[test]
    fn registry_detects_conflicts_and_invalid_field_or_binding_claims() {
        let invalid_type = PERSONAL_TYPE
            .replace("category: kind", "category: missing")
            .replace("mode: personal", "mode: invalid");
        let missing_required = PERSONAL_TYPE
            .replace("personal_note", "missing_required")
            .replace("      title: headline\n", "");
        let (_directory, collection, types) = setup_collection(
            &[
                ("_contracts/a.md", NOTE_CONTRACT),
                (
                    "_contracts/b.md",
                    &NOTE_CONTRACT.replace("title: {type: string}", "title: {type: number}"),
                ),
                (
                    "_contracts/other.md",
                    &NOTE_CONTRACT.replace("example.note", "example.other"),
                ),
            ],
            &[
                (
                    "_types/invalid.md",
                    &invalid_type.replace("example.note", "example.other"),
                ),
                (
                    "_types/missing.md",
                    &missing_required.replace("example.note", "example.other"),
                ),
            ],
        );

        let registry = load_mdbase_contract_registry(&collection, &types)
            .expect("contract registry should load");
        let codes = registry
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<BTreeSet<_>>();

        assert!(codes.contains("data_contract_conflict"));
        assert!(codes.contains("data_contract_binding_invalid"));
        assert!(codes.contains("data_contract_field_invalid"));
        assert!(registry
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("required contract field")));
        assert!(registry.get("example.note", "1.0.0").is_none());
        assert!(registry
            .implementations("example.other", "1.0.0")
            .is_empty());
    }

    #[test]
    fn projected_views_support_json_pointers_and_validate_contract_schema() {
        let contract = include_str!(
            "../../resources/mdbase/v0.3/upstream/tests/fixtures/data-contracts/json-pointer-contact.contract.md"
        );
        let type_file = include_str!(
            "../../resources/mdbase/v0.3/upstream/tests/fixtures/data-contracts/json-pointer-contact-type.md"
        );
        let directory = tempdir().expect("temporary collection should exist");
        write_file(directory.path(), "mdbase.yaml", "spec_version: \"0.3.0\"\n");
        write_file(directory.path(), "_contracts/contact.md", contract);
        write_file(directory.path(), "_types/contact.md", type_file);
        let collection = load_mdbase_collection(directory.path())
            .expect("collection should load")
            .expect("collection should be detected");
        let types = load_mdbase_type_registry(&collection).expect("types should load");
        let registry =
            load_mdbase_contract_registry(&collection, &types).expect("contracts should load");
        assert!(
            registry.diagnostics.is_empty(),
            "{:?}",
            registry.diagnostics
        );

        let valid = project_mdbase_contract_view(
            &collection,
            &registry,
            "example.typed-contact",
            "1.0.0",
            "contact_card",
            &serde_json::json!({"card": {
                "@type": "Contact", "label": "Ada", "a/b": "slash", "a~b": "tilde"
            }}),
        );
        assert!(valid.diagnostics.is_empty(), "{:?}", valid.diagnostics);
        assert_eq!(valid.view["@type"], "Contact");
        assert_eq!(valid.view["a/b"], "slash");
        assert_eq!(valid.view["a~b"], "tilde");

        let invalid = project_mdbase_contract_view(
            &collection,
            &registry,
            "example.typed-contact",
            "1.0.0",
            "contact_card",
            &serde_json::json!({"card": {"@type": "Contact"}}),
        );
        assert!(invalid
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "data_contract_record_invalid"));
    }

    #[test]
    fn conflicting_projection_targets_return_a_diagnostic_instead_of_panicking() {
        let mut view = serde_json::json!({});
        set_contract_view_value(&mut view, "metadata", serde_json::json!("scalar"))
            .expect("first mapping should succeed");

        let error = set_contract_view_value(&mut view, "metadata.owner", serde_json::json!("Ada"))
            .expect_err("overlapping mapping should fail safely");

        assert!(error.contains("conflicts with another mapped value"));
    }

    #[test]
    fn portable_digests_ignore_human_metadata_but_include_schema_annotations() {
        let (_directory, collection, types) = setup_collection(
            &[("_contracts/note.md", NOTE_CONTRACT)],
            &[("_types/personal.md", PERSONAL_TYPE)],
        );
        let first = load_mdbase_contract_registry(&collection, &types)
            .expect("contract registry should load");
        let first_contract = first.get("example.note", "1.0.0").expect("contract");
        let first_impl = &first.implementations("example.note", "1.0.0")[0];

        let changed = NOTE_CONTRACT
            .replace("name: Note", "name: Renamed")
            .replace(
                "    type: object\n    required",
                "    description: ignored\n    type: object\n    required",
            );
        write_file(
            &collection.root,
            "_contracts/note.md",
            &frontmatter(&changed),
        );
        let second = load_mdbase_contract_registry(&collection, &types)
            .expect("contract registry should reload");
        let second_contract = second.get("example.note", "1.0.0").expect("contract");
        let second_impl = &second.implementations("example.note", "1.0.0")[0];

        assert_ne!(first_contract.digest, second_contract.digest);
        assert_ne!(first_impl.digest, second_impl.digest);
        let renamed_only = NOTE_CONTRACT.replace("name: Note", "name: Renamed");
        write_file(
            &collection.root,
            "_contracts/note.md",
            &frontmatter(&renamed_only),
        );
        let third = load_mdbase_contract_registry(&collection, &types)
            .expect("contract registry should reload");
        assert_eq!(
            first_contract.digest,
            third.get("example.note", "1.0.0").expect("contract").digest
        );
    }

    #[test]
    fn implementation_digest_pins_the_authored_type_schema_wrapper() {
        let (_directory, collection, types) = setup_collection(
            &[("_contracts/note.md", NOTE_CONTRACT)],
            &[("_types/personal.md", PERSONAL_TYPE)],
        );
        let inline = load_mdbase_contract_registry(&collection, &types)
            .expect("inline registry should load");
        let contract_digest = inline
            .get("example.note", "1.0.0")
            .expect("inline contract")
            .digest
            .clone();
        let implementation_digest = inline.implementations("example.note", "1.0.0")[0]
            .digest
            .clone();

        write_file(
            &collection.root,
            "_contracts/schemas.json",
            r#"{"$defs":{"record":{"type":"object","required":["title"],"additionalProperties":false,"properties":{"title":{"type":"string"},"category":{"type":"string"}}},"binding":{"type":"object","required":["mode"],"additionalProperties":false,"properties":{"mode":{"enum":["personal","work"]}}}}}"#,
        );
        write_file(
            &collection.root,
            "_contracts/note.md",
            &frontmatter(
                r"kind: mdbase.contract
contract_type: record
id: example.note
version: 1.0.0
record_schema:
  dialect: json-schema-2020-12
  ref: ./schemas.json#/$defs/record
binding_schema:
  dialect: json-schema-2020-12
  ref: ./schemas.json#/$defs/binding
",
            ),
        );
        let referenced = load_mdbase_contract_registry(&collection, &types)
            .expect("referenced contract should load");
        assert_eq!(
            contract_digest,
            referenced
                .get("example.note", "1.0.0")
                .expect("referenced contract")
                .digest
        );
        assert_eq!(
            implementation_digest,
            referenced.implementations("example.note", "1.0.0")[0].digest
        );

        write_file(
            &collection.root,
            "_types/personal.schema.json",
            r#"{"type":"object","required":["headline"],"properties":{"headline":{"type":"string"},"kind":{"type":"string"}}}"#,
        );
        write_file(
            &collection.root,
            "_types/personal.md",
            &frontmatter(
                r"kind: mdbase.type
name: personal_note
version: 1
schema:
  dialect: json-schema-2020-12
  ref: ./personal.schema.json
implements:
  - contract: example.note
    version: 1.0.0
    fields:
      title: headline
      category: kind
    binding: {mode: personal}
",
            ),
        );
        let referenced_types =
            load_mdbase_type_registry(&collection).expect("referenced type should load");
        let all_referenced = load_mdbase_contract_registry(&collection, &referenced_types)
            .expect("referenced implementation should load");
        assert!(all_referenced.diagnostics.is_empty());
        assert_ne!(
            implementation_digest,
            all_referenced.implementations("example.note", "1.0.0")[0].digest
        );
    }
}
