//! Reader and validator for Markdown Wiki Package snapshots.
//!
//! Version 2 is a Container Core v1 format with required provenance and
//! optional source-map and Knowledge v1 sidecars. Version 1 packages remain
//! readable and importable with their original identity rule.

use crate::exchange::{
    compile_schema, container_identity, error_diag, has_errors, is_blake3_digest, parse_jsonl,
    sort_diagnostics, valid_member_namespace, validate_locator, validate_provenance,
    validate_schema, validate_sources, validate_span, validate_with, ExchangeByteSpan,
    ExchangeDiagnostic, ExchangeDiagnosticSeverity, ExchangeProducer, ExchangeProvenance,
    ExchangeSource, ExchangeSourceLocator, ProvenanceMember,
};
use crate::knowledge::{validate_knowledge_jsonl, KnowledgeHost, KnowledgeSnapshot};
use crate::textbundle::{
    copy_member, observe_members, read_member, TextBundleError, TextBundleMember,
    TextBundleRepresentation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::io::Write;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const WIKI_PACKAGE_FORMAT: &str = "dev.tionis.markdown-wiki-package";
/// The version Vulcan writes.
pub const WIKI_PACKAGE_VERSION: u32 = 2;
pub const WIKI_PACKAGE_V1: u32 = 1;
pub const WIKI_SOURCE_MAP_FORMAT: &str = "dev.tionis.wiki-source-map";
pub const WIKI_MANIFEST_PATH: &str = "wiki.json";
pub const WIKI_PROVENANCE_PATH: &str = "provenance.json";
pub const WIKI_SOURCE_MAP_PATH: &str = "source-map.jsonl";
pub const WIKI_KNOWLEDGE_PATH: &str = "knowledge.jsonl";
const MANIFEST_SCHEMA: &str = include_str!("../resources/wiki-package/v2/wiki.schema.json");
const SOURCE_MAP_RECORD_SCHEMA: &str =
    include_str!("../resources/wiki-package/v2/source-map-record.schema.json");
const PROVENANCE_SCHEMA: &str =
    include_str!("../resources/container-core/v1/provenance.schema.json");
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PROVENANCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_NOTE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SIDECAR_BYTES: u64 = 512 * 1024 * 1024;

pub type WikiPackageDiagnostic = ExchangeDiagnostic;
pub type WikiPackageDiagnosticSeverity = ExchangeDiagnosticSeverity;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WikiPackageMemberRole {
    Note,
    Asset,
    Provenance,
    SourceMap,
    Knowledge,
    Source,
    Environment,
    Extension,
}

impl WikiPackageMemberRole {
    #[must_use]
    pub fn is_content(self) -> bool {
        matches!(self, Self::Note | Self::Asset)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiPackageProducerV1 {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiPackageMemberV1 {
    pub path: String,
    pub role: WikiPackageMemberRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub size: u64,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WikiPackageManifestV1 {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub producer: WikiPackageProducerV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lineage: Vec<String>,
    pub members: Vec<WikiPackageMemberV1>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiPackageMemberV2 {
    pub path: String,
    pub role: WikiPackageMemberRole,
    pub media_type: String,
    pub size: u64,
    pub digest: String,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiPackageManifestV2 {
    pub format: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub producer: ExchangeProducer,
    pub members: Vec<WikiPackageMemberV2>,
    pub sources: Vec<ExchangeSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_from: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum WikiPackageManifest {
    V1(WikiPackageManifestV1),
    V2(WikiPackageManifestV2),
}

/// A declared note or asset that import materializes below the destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WikiContentMember {
    pub path: String,
    pub role: WikiPackageMemberRole,
    pub size: u64,
    pub digest: String,
}

impl WikiPackageManifest {
    #[must_use]
    pub fn version(&self) -> u32 {
        match self {
            Self::V1(_) => WIKI_PACKAGE_V1,
            Self::V2(_) => WIKI_PACKAGE_VERSION,
        }
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        match self {
            Self::V1(manifest) => manifest.title.as_deref(),
            Self::V2(manifest) => manifest.title.as_deref(),
        }
    }

    #[must_use]
    pub fn content_members(&self) -> Vec<WikiContentMember> {
        let members: Vec<(&str, WikiPackageMemberRole, u64, &str)> = match self {
            Self::V1(manifest) => manifest
                .members
                .iter()
                .map(|member| {
                    (
                        member.path.as_str(),
                        member.role,
                        member.size,
                        member.digest.as_str(),
                    )
                })
                .collect(),
            Self::V2(manifest) => manifest
                .members
                .iter()
                .map(|member| {
                    (
                        member.path.as_str(),
                        member.role,
                        member.size,
                        member.digest.as_str(),
                    )
                })
                .collect(),
        };
        members
            .into_iter()
            .filter(|(_, role, _, _)| role.is_content())
            .map(|(path, role, size, digest)| WikiContentMember {
                path: path.to_string(),
                role,
                size,
                digest: digest.to_string(),
            })
            .collect()
    }

    fn declared(&self, path: &str) -> Option<(u64, &str)> {
        match self {
            Self::V1(manifest) => manifest
                .members
                .iter()
                .find(|member| member.path == path)
                .map(|member| (member.size, member.digest.as_str())),
            Self::V2(manifest) => manifest
                .members
                .iter()
                .find(|member| member.path == path)
                .map(|member| (member.size, member.digest.as_str())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiSourceMapping {
    pub note: String,
    pub document: ExchangeByteSpan,
    pub source: ExchangeSourceLocator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiSourceReference {
    pub note: String,
    pub document: ExchangeByteSpan,
    pub target: ExchangeSourceLocator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "record", rename_all = "lowercase")]
enum WikiSourceMapRecord {
    Header { format: String, version: u32 },
    Mapping(WikiSourceMapping),
    Reference(WikiSourceReference),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WikiSourceMap {
    pub mappings: Vec<WikiSourceMapping>,
    pub references: Vec<WikiSourceReference>,
}

impl WikiSourceMap {
    /// Mappings whose document span lies in `note`, in source-map order.
    pub fn mappings_for<'a>(
        &'a self,
        note: &'a str,
    ) -> impl Iterator<Item = &'a WikiSourceMapping> {
        self.mappings
            .iter()
            .filter(move |mapping| mapping.note == note)
    }
}

/// Counts reported by inspection and import; not a quality measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct WikiPackageSummary {
    pub notes: usize,
    pub assets: usize,
    pub sources: usize,
    pub provenance_activities: usize,
    pub source_mappings: usize,
    pub source_references: usize,
    pub entities: usize,
    pub claims: usize,
    pub accepted_entities: usize,
    pub accepted_claims: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WikiPackage {
    #[serde(skip)]
    pub package_path: PathBuf,
    pub representation: TextBundleRepresentation,
    pub identity: String,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    pub manifest: Option<WikiPackageManifest>,
    pub summary: WikiPackageSummary,
    #[serde(skip)]
    pub provenance: Option<ExchangeProvenance>,
    #[serde(skip)]
    pub source_map: Option<WikiSourceMap>,
    #[serde(skip)]
    pub knowledge: Option<KnowledgeSnapshot>,
    pub diagnostics: Vec<WikiPackageDiagnostic>,
}

impl WikiPackage {
    pub fn copy_member_to(
        &self,
        path: &str,
        writer: &mut impl Write,
    ) -> Result<(), WikiPackageError> {
        let (expected_size, expected_digest) = self
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.declared(path))
            .ok_or_else(|| WikiPackageError::Invalid(format!("undeclared member: {path}")))?;
        let (size, digest) = copy_member(&self.package_path, self.representation, path, writer)?;
        if size != expected_size || digest != expected_digest {
            return Err(WikiPackageError::Invalid(format!(
                "member changed since inspection: {path}"
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn content_members(&self) -> Vec<WikiContentMember> {
        self.manifest
            .as_ref()
            .map(WikiPackageManifest::content_members)
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub enum WikiPackageError {
    Container(TextBundleError),
    Invalid(String),
}

impl Display for WikiPackageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(error) => write!(formatter, "portable package error: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid wiki package: {message}"),
        }
    }
}

impl std::error::Error for WikiPackageError {}

impl From<TextBundleError> for WikiPackageError {
    fn from(error: TextBundleError) -> Self {
        Self::Container(error)
    }
}

/// Inspect and validate a `.wikibundle` directory or `.wikipack` ZIP. Unsafe
/// containers are errors; everything else is a deterministic diagnostic.
pub fn inspect_wiki_package(path: &Path) -> Result<WikiPackage, WikiPackageError> {
    let representation = if path.is_dir() {
        TextBundleRepresentation::Directory
    } else {
        TextBundleRepresentation::Zip
    };
    let observed = observe_members(path, representation)?;
    let mut package = WikiPackage {
        package_path: path.to_path_buf(),
        representation,
        identity: observed_identity(&observed),
        valid: false,
        version: None,
        manifest: None,
        summary: WikiPackageSummary::default(),
        provenance: None,
        source_map: None,
        knowledge: None,
        diagnostics: Vec::new(),
    };
    let value = match read_member(path, representation, WIKI_MANIFEST_PATH, MAX_MANIFEST_BYTES)
        .map_err(|error| error.to_string())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()))
    {
        Ok(value) => Some(value),
        Err(error) => {
            let code = if observed.contains_key(WIKI_MANIFEST_PATH) {
                "manifest_invalid"
            } else {
                "manifest_missing"
            };
            error_diag(&mut package.diagnostics, code, error, WIKI_MANIFEST_PATH);
            None
        }
    };
    if let Some(value) = value {
        match value.get("version").and_then(Value::as_u64) {
            Some(1) => inspect_v1(&mut package, value, &observed),
            Some(2) => inspect_v2(&mut package, &value, &observed),
            _ => error_diag(
                &mut package.diagnostics,
                "version_unsupported",
                "unsupported package version",
                WIKI_MANIFEST_PATH,
            ),
        }
    }
    sort_diagnostics(&mut package.diagnostics);
    package.valid = !has_errors(&package.diagnostics);
    Ok(package)
}

fn observed_identity(observed: &BTreeMap<String, TextBundleMember>) -> String {
    container_identity(
        observed
            .values()
            .map(|member| (member.path.as_str(), member.size, member.digest.as_str())),
    )
}

fn inspect_v1(
    package: &mut WikiPackage,
    value: Value,
    observed: &BTreeMap<String, TextBundleMember>,
) {
    let diagnostics = &mut package.diagnostics;
    let manifest = match serde_json::from_value::<WikiPackageManifestV1>(value) {
        Ok(manifest) => manifest,
        Err(error) => {
            error_diag(
                diagnostics,
                "manifest_invalid",
                error.to_string(),
                WIKI_MANIFEST_PATH,
            );
            return;
        }
    };
    package.version = Some(WIKI_PACKAGE_V1);
    if manifest.format != WIKI_PACKAGE_FORMAT {
        error_diag(
            diagnostics,
            "format_unsupported",
            "unsupported package format",
            WIKI_MANIFEST_PATH,
        );
    }
    if manifest.producer.name.trim().is_empty() {
        error_diag(
            diagnostics,
            "producer_invalid",
            "producer name must not be empty",
            WIKI_MANIFEST_PATH,
        );
    }
    let mut declared = BTreeMap::new();
    let mut folded = BTreeSet::new();
    for member in &manifest.members {
        if !member.path.starts_with("content/") || !valid_declared_path(&member.path) {
            error_diag(
                diagnostics,
                "member_path_invalid",
                "member path must be below content/",
                &member.path,
            );
            continue;
        }
        if !member.role.is_content() {
            error_diag(
                diagnostics,
                "member_role_invalid",
                "version 1 members are notes or assets",
                &member.path,
            );
        }
        if !folded.insert(member.path.to_lowercase())
            || declared
                .insert(member.path.as_str(), (member.size, member.digest.as_str()))
                .is_some()
        {
            error_diag(
                diagnostics,
                "member_duplicate",
                "duplicate or case-fold-colliding declaration",
                &member.path,
            );
        }
        if !is_blake3_digest(&member.digest) {
            error_diag(
                diagnostics,
                "member_digest_invalid",
                "digest must use blake3 lowercase hex",
                &member.path,
            );
        }
        if (member.role == WikiPackageMemberRole::Note) != is_markdown_path(&member.path) {
            error_diag(
                diagnostics,
                "member_role_invalid",
                "note role must correspond exactly to .md paths",
                &member.path,
            );
        }
    }
    check_observed(&declared, observed, diagnostics);
    for member in &manifest.members {
        if member.role == WikiPackageMemberRole::Note && observed.contains_key(&member.path) {
            read_note(package, &member.path);
        }
    }
    package.summary.notes = count_role(&manifest.members, WikiPackageMemberRole::Note);
    package.summary.assets = count_role(&manifest.members, WikiPackageMemberRole::Asset);
    package.identity = logical_identity_v1(&manifest.members);
    package.manifest = Some(WikiPackageManifest::V1(manifest));
}

fn count_role(members: &[WikiPackageMemberV1], role: WikiPackageMemberRole) -> usize {
    members.iter().filter(|member| member.role == role).count()
}

#[allow(clippy::too_many_lines)]
fn inspect_v2(
    package: &mut WikiPackage,
    value: &Value,
    observed: &BTreeMap<String, TextBundleMember>,
) {
    package.version = Some(WIKI_PACKAGE_VERSION);
    let before = package.diagnostics.len();
    validate_schema(
        WIKI_MANIFEST_PATH,
        MANIFEST_SCHEMA,
        value,
        &mut package.diagnostics,
    );
    if package.diagnostics.len() != before {
        return;
    }
    let manifest = match serde_json::from_value::<WikiPackageManifestV2>(value.clone()) {
        Ok(manifest) => manifest,
        Err(error) => {
            error_diag(
                &mut package.diagnostics,
                "manifest_invalid",
                error.to_string(),
                WIKI_MANIFEST_PATH,
            );
            return;
        }
    };
    let diagnostics = &mut package.diagnostics;
    let mut declared = BTreeMap::new();
    let mut folded = BTreeSet::new();
    let mut document_ids = BTreeSet::new();
    for member in &manifest.members {
        if !valid_declared_path(&member.path) || member.path == WIKI_MANIFEST_PATH {
            error_diag(
                diagnostics,
                "member_path_invalid",
                "member path is unsafe or reserved",
                &member.path,
            );
            continue;
        }
        if !folded.insert(member.path.to_lowercase())
            || declared
                .insert(member.path.as_str(), (member.size, member.digest.as_str()))
                .is_some()
        {
            error_diag(
                diagnostics,
                "member_duplicate",
                "duplicate or case-fold-colliding declaration",
                &member.path,
            );
        }
        validate_role_path(member, diagnostics);
        if let Some(document_id) = member.document_id.as_deref() {
            if member.role != WikiPackageMemberRole::Note {
                error_diag(
                    diagnostics,
                    "document_id_invalid",
                    "only notes carry document ids",
                    &member.path,
                );
            } else if !document_ids.insert(document_id) {
                error_diag(
                    diagnostics,
                    "document_id_duplicate",
                    format!("document id {document_id} is declared more than once"),
                    &member.path,
                );
            }
        }
    }
    check_observed(&declared, observed, diagnostics);
    for (path, role) in [
        (WIKI_PROVENANCE_PATH, WikiPackageMemberRole::Provenance),
        (WIKI_SOURCE_MAP_PATH, WikiPackageMemberRole::SourceMap),
        (WIKI_KNOWLEDGE_PATH, WikiPackageMemberRole::Knowledge),
    ] {
        let required = role == WikiPackageMemberRole::Provenance;
        match manifest.members.iter().find(|member| member.path == path) {
            Some(member) if member.role == role => {}
            Some(_) => error_diag(
                diagnostics,
                "member_role_invalid",
                format!("{path} has the wrong role"),
                path,
            ),
            None if required || observed.contains_key(path) => error_diag(
                diagnostics,
                "member_declaration_missing",
                format!("{path} must be declared"),
                path,
            ),
            None => {}
        }
    }
    if let Some(root) = manifest.root.as_deref() {
        if !manifest
            .members
            .iter()
            .any(|member| member.path == root && member.role == WikiPackageMemberRole::Note)
        {
            error_diag(
                diagnostics,
                "root_invalid",
                "root must be a declared note",
                WIKI_MANIFEST_PATH,
            );
        }
    }
    let member_digests = manifest
        .members
        .iter()
        .map(|member| {
            (
                member.path.as_str(),
                (
                    member.digest.as_str(),
                    member.role == WikiPackageMemberRole::Source,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    validate_sources(
        &manifest.sources,
        &member_digests,
        WIKI_MANIFEST_PATH,
        diagnostics,
    );
    let source_ids = manifest
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .collect::<BTreeSet<_>>();

    let provenance = read_provenance(package, observed);
    if let Some(provenance) = provenance.as_ref() {
        let members = manifest
            .members
            .iter()
            .map(|member| ProvenanceMember {
                path: &member.path,
                created_by: &member.created_by,
            })
            .collect::<Vec<_>>();
        validate_provenance(
            provenance,
            &members,
            &source_ids,
            WIKI_PROVENANCE_PATH,
            &mut package.diagnostics,
        );
    }

    let sidecars =
        observed.contains_key(WIKI_SOURCE_MAP_PATH) || observed.contains_key(WIKI_KNOWLEDGE_PATH);
    let mut notes = BTreeMap::new();
    for member in &manifest.members {
        if member.role == WikiPackageMemberRole::Note && observed.contains_key(&member.path) {
            if let Some(text) = read_note(package, &member.path) {
                if sidecars {
                    notes.insert(member.path.clone(), text);
                }
            }
        }
    }
    let source_map = read_text_sidecar(package, observed, WIKI_SOURCE_MAP_PATH)
        .and_then(|text| validate_source_map(&text, &source_ids, &notes, &mut package.diagnostics));
    let knowledge = read_text_sidecar(package, observed, WIKI_KNOWLEDGE_PATH).and_then(|text| {
        validate_knowledge_jsonl(
            &text,
            WIKI_KNOWLEDGE_PATH,
            &KnowledgeHost {
                source_ids: &source_ids,
                notes: Some(&notes),
            },
            &mut package.diagnostics,
        )
    });

    let summary = &mut package.summary;
    for member in &manifest.members {
        match member.role {
            WikiPackageMemberRole::Note => summary.notes += 1,
            WikiPackageMemberRole::Asset => summary.assets += 1,
            _ => {}
        }
    }
    summary.sources = manifest.sources.len();
    summary.provenance_activities = provenance
        .as_ref()
        .map_or(0, |provenance| provenance.activities.len());
    if let Some(map) = source_map.as_ref() {
        summary.source_mappings = map.mappings.len();
        summary.source_references = map.references.len();
    }
    if let Some(knowledge) = knowledge.as_ref() {
        let counts = knowledge.summary();
        summary.entities = counts.entities;
        summary.claims = counts.claims;
        summary.accepted_entities = counts.accepted_entities;
        summary.accepted_claims = counts.accepted_claims;
    }
    package.provenance = provenance;
    package.source_map = source_map;
    package.knowledge = knowledge;
    package.manifest = Some(WikiPackageManifest::V2(manifest));
}

fn validate_role_path(member: &WikiPackageMemberV2, diagnostics: &mut Vec<WikiPackageDiagnostic>) {
    let path = member.path.as_str();
    let in_content = path.starts_with("content/");
    let valid = match member.role {
        WikiPackageMemberRole::Note => {
            in_content && is_markdown_path(path) && member.media_type == "text/markdown"
        }
        WikiPackageMemberRole::Asset => in_content && !is_markdown_path(path),
        WikiPackageMemberRole::Provenance => path == WIKI_PROVENANCE_PATH,
        WikiPackageMemberRole::SourceMap => path == WIKI_SOURCE_MAP_PATH,
        WikiPackageMemberRole::Knowledge => path == WIKI_KNOWLEDGE_PATH,
        WikiPackageMemberRole::Source => path.starts_with("sources/"),
        WikiPackageMemberRole::Environment => path.starts_with("environments/"),
        WikiPackageMemberRole::Extension => {
            let namespace = member.namespace.as_deref().unwrap_or_default();
            valid_member_namespace(namespace)
                && path
                    .strip_prefix("extensions/")
                    .and_then(|rest| rest.strip_prefix(namespace))
                    .is_some_and(|rest| rest.len() > 1 && rest.starts_with('/'))
        }
    };
    if !valid {
        error_diag(
            diagnostics,
            "member_role_path_mismatch",
            format!(
                "member path, media type, or namespace does not match role {:?}",
                member.role
            ),
            path,
        );
    }
    if member.namespace.is_some() && member.role != WikiPackageMemberRole::Extension {
        error_diag(
            diagnostics,
            "member_namespace_invalid",
            "only extension members carry a namespace",
            path,
        );
    }
}

fn check_observed(
    declared: &BTreeMap<&str, (u64, &str)>,
    observed: &BTreeMap<String, TextBundleMember>,
    diagnostics: &mut Vec<WikiPackageDiagnostic>,
) {
    for member in observed
        .values()
        .filter(|member| member.path != WIKI_MANIFEST_PATH)
    {
        match declared.get(member.path.as_str()) {
            None => error_diag(
                diagnostics,
                "member_undeclared",
                "package member is not declared",
                &member.path,
            ),
            Some((size, _)) if *size != member.size => error_diag(
                diagnostics,
                "member_size_mismatch",
                "declared size does not match bytes",
                &member.path,
            ),
            Some((_, digest)) if *digest != member.digest => error_diag(
                diagnostics,
                "member_digest_mismatch",
                "declared digest does not match bytes",
                &member.path,
            ),
            Some(_) => {}
        }
    }
    for path in declared.keys() {
        if !observed.contains_key(*path) {
            error_diag(
                diagnostics,
                "member_missing",
                "declared member is missing",
                *path,
            );
        }
    }
}

fn read_note(package: &mut WikiPackage, path: &str) -> Option<String> {
    let bytes = match read_member(
        &package.package_path,
        package.representation,
        path,
        MAX_NOTE_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(error) => {
            error_diag(
                &mut package.diagnostics,
                "member_unreadable",
                error.to_string(),
                path,
            );
            return None;
        }
    };
    let text = String::from_utf8(bytes).ok();
    if text.is_none() {
        error_diag(
            &mut package.diagnostics,
            "note_not_utf8",
            "note is not valid UTF-8",
            path,
        );
    }
    text
}

fn read_text_sidecar(
    package: &mut WikiPackage,
    observed: &BTreeMap<String, TextBundleMember>,
    path: &str,
) -> Option<String> {
    if !observed.contains_key(path) {
        return None;
    }
    let limit = if path == WIKI_PROVENANCE_PATH {
        MAX_PROVENANCE_BYTES
    } else {
        MAX_SIDECAR_BYTES
    };
    match read_member(&package.package_path, package.representation, path, limit)
        .map_err(|error| error.to_string())
        .and_then(|bytes| String::from_utf8(bytes).map_err(|error| error.to_string()))
    {
        Ok(text) => Some(text),
        Err(error) => {
            error_diag(&mut package.diagnostics, "member_unreadable", error, path);
            None
        }
    }
}

fn read_provenance(
    package: &mut WikiPackage,
    observed: &BTreeMap<String, TextBundleMember>,
) -> Option<ExchangeProvenance> {
    let path = WIKI_PROVENANCE_PATH;
    let text = read_text_sidecar(package, observed, path)?;
    let value = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(error) => {
            error_diag(
                &mut package.diagnostics,
                "invalid_json",
                error.to_string(),
                path,
            );
            return None;
        }
    };
    let before = package.diagnostics.len();
    validate_schema(path, PROVENANCE_SCHEMA, &value, &mut package.diagnostics);
    if package.diagnostics.len() != before {
        return None;
    }
    match serde_json::from_value(value) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            error_diag(
                &mut package.diagnostics,
                "control_document_invalid",
                error.to_string(),
                path,
            );
            None
        }
    }
}

fn validate_source_map(
    text: &str,
    source_ids: &BTreeSet<&str>,
    notes: &BTreeMap<String, String>,
    diagnostics: &mut Vec<WikiPackageDiagnostic>,
) -> Option<WikiSourceMap> {
    let path = WIKI_SOURCE_MAP_PATH;
    let records = parse_jsonl(text, path, diagnostics)?;
    let validator = compile_schema(SOURCE_MAP_RECORD_SCHEMA);
    let mut map = WikiSourceMap::default();
    let mut previous: Option<(String, usize, usize)> = None;
    for (index, (line, value)) in records.into_iter().enumerate() {
        let before = diagnostics.len();
        validate_with(&validator, path, Some(line), &value, diagnostics);
        if diagnostics.len() != before {
            continue;
        }
        let record = match serde_json::from_value::<WikiSourceMapRecord>(value) {
            Ok(record) => record,
            Err(error) => {
                error_diag(
                    diagnostics,
                    "control_document_invalid",
                    format!("line {line}: {error}"),
                    path,
                );
                continue;
            }
        };
        if matches!(record, WikiSourceMapRecord::Header { .. }) != (index == 0) {
            error_diag(
                diagnostics,
                "source_map_header_invalid",
                format!("line {line}: the header must be exactly the first record"),
                path,
            );
        }
        let (note, span) = match &record {
            WikiSourceMapRecord::Header { .. } => continue,
            WikiSourceMapRecord::Mapping(mapping) => {
                validate_locator(&mapping.source, source_ids, path, diagnostics);
                (mapping.note.clone(), mapping.document)
            }
            WikiSourceMapRecord::Reference(reference) => {
                validate_locator(&reference.target, source_ids, path, diagnostics);
                (reference.note.clone(), reference.document)
            }
        };
        match notes.get(&note) {
            Some(text) => {
                validate_span(span, text, path, diagnostics);
            }
            None => error_diag(
                diagnostics,
                "source_map_note_unknown",
                format!("line {line}: {note} is not a declared note"),
                path,
            ),
        }
        let key = (note, span.start, span.end);
        if previous.as_ref().is_some_and(|previous| {
            (previous.0.as_bytes(), previous.1, previous.2) > (key.0.as_bytes(), key.1, key.2)
        }) {
            error_diag(
                diagnostics,
                "record_order_invalid",
                format!("line {line}: records must be sorted by note, start, and end"),
                path,
            );
        }
        previous = Some(key);
        match record {
            WikiSourceMapRecord::Header { .. } => {}
            WikiSourceMapRecord::Mapping(mapping) => map.mappings.push(mapping),
            WikiSourceMapRecord::Reference(reference) => map.references.push(reference),
        }
    }
    if text.is_empty() {
        error_diag(
            diagnostics,
            "source_map_header_invalid",
            "the source map is empty",
            path,
        );
    }
    Some(map)
}

/// The version 1 logical identity. It hashes content members only and
/// excludes `wiki.json`; version 2 uses the Container Core identity.
#[must_use]
pub fn logical_identity_v1(members: &[WikiPackageMemberV1]) -> String {
    let mut members = members.to_vec();
    members.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = blake3::Hasher::new();
    for member in members {
        let path = serde_json::to_string(&member.path).expect("path serializes");
        hasher.update(
            format!(
                "{{\"path\":{path},\"role\":\"{}\",\"size\":{},\"digest\":\"{}\"}}\n",
                match member.role {
                    WikiPackageMemberRole::Note => "note",
                    _ => "asset",
                },
                member.size,
                member.digest
            )
            .as_bytes(),
        );
    }
    format!("blake3:{}", hasher.finalize())
}

#[must_use]
pub fn is_markdown_path(path: &str) -> bool {
    crate::paths::has_markdown_extension(path)
}

fn valid_declared_path(path: &str) -> bool {
    !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.nfc().collect::<String>() == path
        && path
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::{blake3_digest, canonical_json};
    use std::fs;
    use tempfile::tempdir;

    fn manifest_v1(note: &[u8]) -> WikiPackageManifestV1 {
        WikiPackageManifestV1 {
            format: WIKI_PACKAGE_FORMAT.to_string(),
            version: 1,
            title: Some("Synthetic".to_string()),
            producer: WikiPackageProducerV1 {
                name: "test".to_string(),
                version: None,
                extensions: serde_json::Map::new(),
            },
            lineage: Vec::new(),
            members: vec![WikiPackageMemberV1 {
                path: "content/Home.md".to_string(),
                role: WikiPackageMemberRole::Note,
                media_type: Some("text/markdown".to_string()),
                size: note.len() as u64,
                digest: blake3_digest(note),
                document_id: None,
                extensions: serde_json::Map::new(),
            }],
            extensions: serde_json::Map::new(),
        }
    }

    /// Write a v2 bundle from `(path, role, bytes)` members plus a generated
    /// single-activity provenance graph.
    fn write_v2(
        root: &Path,
        members: &[(&str, WikiPackageMemberRole, &[u8])],
        edit: impl FnOnce(&mut Value),
    ) {
        fs::create_dir_all(root).expect("root");
        let mut outputs = members
            .iter()
            .map(|(path, _, _)| (*path).to_string())
            .collect::<Vec<_>>();
        outputs.push(WIKI_PROVENANCE_PATH.to_string());
        let provenance = serde_json::json!({
            "version": 1,
            "activities": [{
                "id": "build", "kind": "test", "tools": [{"name": "test", "version": "1"}],
                "models": [], "inputs": ["source:game"], "outputs": outputs, "depends_on": [],
                "parameters": {}, "parameters_digest": blake3_digest(&canonical_json(&serde_json::json!({})))
            }]
        });
        let provenance = serde_json::to_vec_pretty(&provenance).expect("provenance");
        let mut declared = Vec::new();
        for (path, role, bytes) in members.iter().copied().chain([(
            WIKI_PROVENANCE_PATH,
            WikiPackageMemberRole::Provenance,
            provenance.as_slice(),
        )]) {
            let target = root.join(path);
            fs::create_dir_all(target.parent().expect("parent")).expect("dirs");
            fs::write(target, bytes).expect("member");
            let media_type = match role {
                WikiPackageMemberRole::Note => "text/markdown",
                WikiPackageMemberRole::SourceMap | WikiPackageMemberRole::Knowledge => {
                    "application/jsonl"
                }
                WikiPackageMemberRole::Provenance => "application/json",
                _ => "application/octet-stream",
            };
            declared.push(serde_json::json!({
                "path": path, "role": role, "media_type": media_type, "size": bytes.len(),
                "digest": blake3_digest(bytes), "created_by": "build"
            }));
        }
        let mut manifest = serde_json::json!({
            "format": WIKI_PACKAGE_FORMAT, "version": 2, "title": "Synthetic",
            "root": "content/Home.md",
            "producer": {"name": "test", "version": "1"},
            "members": declared,
            "sources": [{"id": "game", "media_type": "application/octet-stream", "digest": blake3_digest(b"game")}]
        });
        edit(&mut manifest);
        fs::write(
            root.join(WIKI_MANIFEST_PATH),
            serde_json::to_vec_pretty(&manifest).expect("manifest"),
        )
        .expect("manifest");
    }

    fn codes(package: &WikiPackage) -> BTreeSet<&str> {
        package
            .diagnostics
            .iter()
            .map(|item| item.code.as_str())
            .collect()
    }

    const SOURCE_MAP: &str = concat!(
        r#"{"record":"header","format":"dev.tionis.wiki-source-map","version":1}"#,
        "\n",
        r#"{"record":"mapping","note":"content/Home.md","document":{"start":0,"end":6},"source":{"source_id":"game","selectors":[{"type":"fragment","value":"start"}]},"method":"dev.tionis.test/label"}"#,
        "\n"
    );

    #[test]
    fn inspects_valid_v1_directory_and_detects_changed_bytes() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("sample.wikibundle");
        fs::create_dir_all(root.join("content")).expect("dirs");
        let note = b"# Home\n";
        fs::write(root.join("content/Home.md"), note).expect("note");
        fs::write(
            root.join("wiki.json"),
            serde_json::to_vec_pretty(&manifest_v1(note)).expect("json"),
        )
        .expect("manifest");
        let package = inspect_wiki_package(&root).expect("inspect");
        assert!(package.valid, "{:?}", package.diagnostics);
        assert_eq!(package.version, Some(1));
        fs::write(root.join("content/Home.md"), "changed").expect("change");
        let package = inspect_wiki_package(&root).expect("inspect changed");
        assert!(!package.valid);
        assert!(codes(&package).contains("member_digest_mismatch"));
    }

    #[test]
    fn rejects_casefold_duplicate_declarations() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("duplicate.wikibundle");
        fs::create_dir_all(root.join("content")).expect("dirs");
        let note = b"# Home\n";
        fs::write(root.join("content/Home.md"), note).expect("note");
        let mut manifest = manifest_v1(note);
        let mut duplicate = manifest.members[0].clone();
        duplicate.path = "content/home.md".to_string();
        manifest.members.push(duplicate);
        fs::write(
            root.join("wiki.json"),
            serde_json::to_vec_pretty(&manifest).expect("json"),
        )
        .expect("manifest");
        let package = inspect_wiki_package(&root).expect("inspect");
        assert!(!package.valid);
        assert!(codes(&package).contains("member_duplicate"));
    }

    #[test]
    fn v2_validates_sidecars_and_uses_container_identity() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("v2.wikibundle");
        write_v2(
            &root,
            &[
                (
                    "content/Home.md",
                    WikiPackageMemberRole::Note,
                    b"# Home\n\nBody\n",
                ),
                (
                    WIKI_SOURCE_MAP_PATH,
                    WikiPackageMemberRole::SourceMap,
                    SOURCE_MAP.as_bytes(),
                ),
            ],
            |_| {},
        );
        let package = inspect_wiki_package(&root).expect("inspect");
        assert!(package.valid, "{:?}", package.diagnostics);
        assert_eq!(package.version, Some(2));
        assert_eq!(package.summary.source_mappings, 1);
        assert_eq!(package.summary.provenance_activities, 1);
        let observed = observe_members(&root, TextBundleRepresentation::Directory).expect("obs");
        assert!(observed.contains_key(WIKI_MANIFEST_PATH));
        assert_eq!(package.identity, observed_identity(&observed));
    }

    #[test]
    fn v2_rejects_unbound_spans_unknown_sources_and_missing_provenance_links() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("bad.wikibundle");
        let bad_map = SOURCE_MAP
            .replace("\"end\":6", "\"end\":600")
            .replace("\"game\"", "\"other\"");
        write_v2(
            &root,
            &[
                ("content/Home.md", WikiPackageMemberRole::Note, b"# Home\n"),
                (
                    WIKI_SOURCE_MAP_PATH,
                    WikiPackageMemberRole::SourceMap,
                    bad_map.as_bytes(),
                ),
            ],
            |manifest| {
                manifest["members"][0]["created_by"] = serde_json::json!("elsewhere");
                manifest["root"] = serde_json::json!("content/Missing.md");
            },
        );
        let package = inspect_wiki_package(&root).expect("inspect");
        let codes = codes(&package);
        for code in [
            "document_span_invalid",
            "locator_source_unknown",
            "member_provenance_invalid",
            "root_invalid",
        ] {
            assert!(codes.contains(code), "{code} missing from {codes:?}");
        }
    }

    #[test]
    fn v2_requires_provenance_and_role_paths() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("roles.wikibundle");
        write_v2(
            &root,
            &[
                ("content/Home.md", WikiPackageMemberRole::Note, b"# Home\n"),
                ("content/data.bin", WikiPackageMemberRole::Note, b"x"),
                (
                    "extensions/dev.tionis.test/x.json",
                    WikiPackageMemberRole::Extension,
                    b"{}",
                ),
            ],
            |manifest| {
                let members = manifest["members"].as_array_mut().expect("members");
                members.retain(|member| member["path"] != WIKI_PROVENANCE_PATH);
            },
        );
        let package = inspect_wiki_package(&root).expect("inspect");
        let codes = codes(&package);
        for code in [
            "member_role_path_mismatch",
            "member_declaration_missing",
            "member_undeclared",
        ] {
            assert!(codes.contains(code), "{code} missing from {codes:?}");
        }
    }

    #[test]
    fn v2_knowledge_resolves_against_package_notes() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("knowledge.wikibundle");
        let knowledge = concat!(
            r#"{"record":"header","format":"dev.tionis.knowledge","version":1}"#,
            "\n",
            r#"{"record":"entity","id":"alice","name":"Alice","type":"character","note":"content/Home.md","review":"accepted","evidence":[{"role":"support","locator":{"source_id":"game","selectors":[]}}]}"#,
            "\n"
        );
        write_v2(
            &root,
            &[
                ("content/Home.md", WikiPackageMemberRole::Note, b"# Home\n"),
                (
                    WIKI_KNOWLEDGE_PATH,
                    WikiPackageMemberRole::Knowledge,
                    knowledge.as_bytes(),
                ),
            ],
            |_| {},
        );
        let package = inspect_wiki_package(&root).expect("inspect");
        assert!(package.valid, "{:?}", package.diagnostics);
        assert_eq!(package.summary.accepted_entities, 1);
    }

    #[test]
    fn checked_in_examples_match_identity_vectors() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo");
        for (example, vector) in [
            (
                "docs/specs/wiki-package/v1/examples/minimal.wikibundle",
                "docs/specs/wiki-package/v1/identity-test-vector.json",
            ),
            (
                "docs/specs/wiki-package/v2/examples/sourced.wikibundle",
                "docs/specs/wiki-package/v2/identity-test-vector.json",
            ),
        ] {
            let package = inspect_wiki_package(&repo.join(example)).expect("inspect example");
            assert!(package.valid, "{example}: {:?}", package.diagnostics);
            let vector: Value =
                serde_json::from_slice(&fs::read(repo.join(vector)).expect("vector"))
                    .expect("vector json");
            assert_eq!(vector["identity"].as_str(), Some(package.identity.as_str()));
        }
        let v1 = inspect_wiki_package(
            &repo.join("docs/specs/wiki-package/v1/examples/minimal.wikibundle"),
        )
        .expect("v1");
        let Some(WikiPackageManifest::V1(manifest)) = v1.manifest else {
            panic!("v1 manifest expected");
        };
        assert!(manifest.extensions.contains_key("example.extension"));
    }

    #[test]
    fn bundled_schemas_match_published_specs_and_share_core_definitions() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo");
        let read = |path: &str| -> Value {
            serde_json::from_slice(&fs::read(repo.join(path)).expect(path)).expect(path)
        };
        for (resource, spec) in [
            (
                "vulcan-core/resources/container-core/v1/defs.schema.json",
                "docs/specs/container-core/v1/defs.schema.json",
            ),
            (
                "vulcan-core/resources/container-core/v1/provenance.schema.json",
                "docs/specs/container-core/v1/provenance.schema.json",
            ),
            (
                "vulcan-core/resources/knowledge/v1/knowledge-record.schema.json",
                "docs/specs/knowledge/v1/knowledge-record.schema.json",
            ),
            (
                "vulcan-core/resources/wiki-package/v2/wiki.schema.json",
                "docs/specs/wiki-package/v2/wiki.schema.json",
            ),
            (
                "vulcan-core/resources/wiki-package/v2/source-map-record.schema.json",
                "docs/specs/wiki-package/v2/source-map-record.schema.json",
            ),
            (
                "vulcan-core/resources/mdaf/v1/source-map.schema.json",
                "docs/specs/mdaf/v1/source-map.schema.json",
            ),
        ] {
            assert_eq!(read(resource), read(spec), "{resource} differs from {spec}");
        }
        let core = read("docs/specs/container-core/v1/defs.schema.json")["$defs"].clone();
        for schema in [
            "docs/specs/mdaf/v1/source-map.schema.json",
            "docs/specs/mdaf/v1/outline.schema.json",
            "docs/specs/mdaf/v1/info.schema.json",
            "docs/specs/knowledge/v1/knowledge-record.schema.json",
            "docs/specs/wiki-package/v2/wiki.schema.json",
            "docs/specs/wiki-package/v2/source-map-record.schema.json",
        ] {
            let defs = read(schema)["$defs"].clone();
            for (name, definition) in defs.as_object().expect("defs") {
                if let Some(shared) = core.get(name) {
                    assert_eq!(definition, shared, "{schema} redefines core $defs/{name}");
                }
            }
        }
        let mut core_provenance = read("docs/specs/container-core/v1/provenance.schema.json");
        let mut mdaf_provenance = read("docs/specs/mdaf/v1/provenance.schema.json");
        for schema in [&mut core_provenance, &mut mdaf_provenance] {
            let object = schema.as_object_mut().expect("object");
            object.remove("$id");
            object.remove("title");
        }
        assert_eq!(core_provenance, mdaf_provenance);
    }
}
