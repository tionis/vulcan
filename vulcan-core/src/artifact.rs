//! Reader and semantic validator for the extractor-neutral Markdown Artifact
//! Format (MDAF).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;
use zip::ZipArchive;

const INFO_SCHEMA: &str = include_str!("../resources/mdaf/v1/info.schema.json");
const SOURCE_MAP_SCHEMA: &str = include_str!("../resources/mdaf/v1/source-map.schema.json");
const OUTLINE_SCHEMA: &str = include_str!("../resources/mdaf/v1/outline.schema.json");
const PROVENANCE_SCHEMA: &str = include_str!("../resources/mdaf/v1/provenance.schema.json");

pub const MDAF_MAX_FILES: usize = 100_000;
pub const MDAF_MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MDAF_MAX_MEMBER_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MDAF_MAX_NON_ASSET_BYTES: u64 = 512 * 1024 * 1024;
pub const MDAF_MAX_COMPRESSION_RATIO: u64 = 1_000;
const MDAF_MAX_CONTROL_BYTES: u64 = 32 * 1024 * 1024;
const MDAF_MAX_MARKDOWN_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MdafRepresentation {
    Directory,
    Zip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MdafMemberRole {
    Primary,
    Provenance,
    SourceMap,
    Outline,
    Asset,
    Rendition,
    Source,
    Environment,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafMarkdownBinding {
    pub path: String,
    pub digest: String,
    pub media_type: String,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafProducer {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafMember {
    pub path: String,
    pub role: MdafMemberRole,
    pub media_type: String,
    pub size: u64,
    pub digest: String,
    pub created_by: String,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafSource {
    pub id: String,
    pub media_type: String,
    pub digest: String,
    #[serde(default)]
    pub alternate_digests: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub embedded_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafManifest {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub title: Option<String>,
    pub markdown: MdafMarkdownBinding,
    pub producer: MdafProducer,
    pub members: Vec<MdafMember>,
    pub sources: Vec<MdafSource>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub derived_from: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafByteSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MdafSelector {
    Interval {
        unit: String,
        start: f64,
        end: f64,
        #[serde(default)]
        origin: Option<f64>,
        #[serde(default)]
        label_start: Option<String>,
        #[serde(default)]
        label_end: Option<String>,
    },
    Rectangle {
        unit: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Polygon {
        unit: String,
        points: Vec<MdafPoint>,
    },
    Grid {
        #[serde(default)]
        sheet: Option<String>,
        row_start: u64,
        row_end: u64,
        column_start: u64,
        column_end: u64,
    },
    TextQuote {
        exact: String,
        #[serde(default)]
        prefix: Option<String>,
        #[serde(default)]
        suffix: Option<String>,
    },
    Fragment {
        value: String,
        #[serde(default)]
        conforms_to: Option<String>,
    },
    Extension {
        namespace: String,
        data: serde_json::Value,
    },
}

impl MdafSelector {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Interval { .. } => "interval",
            Self::Rectangle { .. } => "rectangle",
            Self::Polygon { .. } => "polygon",
            Self::Grid { .. } => "grid",
            Self::TextQuote { .. } => "text-quote",
            Self::Fragment { .. } => "fragment",
            Self::Extension { .. } => "extension",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafSourceLocator {
    pub source_id: String,
    pub selectors: Vec<MdafSelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafSourceMapping {
    pub document: MdafByteSpan,
    pub source: MdafSourceLocator,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafSourceReference {
    pub document: MdafByteSpan,
    pub target: MdafSourceLocator,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafSourceMap {
    pub version: u32,
    pub document_digest: String,
    pub mappings: Vec<MdafSourceMapping>,
    pub references: Vec<MdafSourceReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafOutlineNode {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub level: u32,
    pub title: String,
    pub heading: MdafByteSpan,
    pub section: MdafByteSpan,
    #[serde(default)]
    pub source: Option<MdafSourceLocator>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafOutline {
    pub version: u32,
    pub document_digest: String,
    #[serde(default)]
    pub title: Option<String>,
    pub nodes: Vec<MdafOutlineNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafTool {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub package_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MdafModelResolution {
    Pinned,
    MutableAlias,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafModel {
    pub provider: String,
    pub identifier: String,
    #[serde(default)]
    pub returned_identifier: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub checksum: Option<String>,
    pub resolution: MdafModelResolution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafActivity {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    pub tools: Vec<MdafTool>,
    pub models: Vec<MdafModel>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub depends_on: Vec<String>,
    pub parameters: serde_json::Map<String, Value>,
    pub parameters_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafRedaction {
    pub member: String,
    pub location: String,
    pub reason: String,
    #[serde(default)]
    pub original_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafProvenance {
    pub version: u32,
    pub activities: Vec<MdafActivity>,
    #[serde(default)]
    pub redactions: Vec<MdafRedaction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MdafDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdafDiagnostic {
    pub severity: MdafDiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdafObservedMember {
    pub path: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MdafArtifact {
    #[serde(skip)]
    pub artifact_path: PathBuf,
    pub representation: MdafRepresentation,
    pub identity: String,
    pub valid: bool,
    pub manifest: Option<MdafManifest>,
    pub markdown: Option<String>,
    pub source_map: Option<MdafSourceMap>,
    pub outline: Option<MdafOutline>,
    pub provenance: Option<MdafProvenance>,
    pub observed_members: Vec<MdafObservedMember>,
    pub diagnostics: Vec<MdafDiagnostic>,
}

impl MdafArtifact {
    #[must_use]
    pub fn member(&self, path: &str) -> Option<&MdafMember> {
        self.manifest
            .as_ref()?
            .members
            .iter()
            .find(|member| member.path == path)
    }

    /// Copy one declared member while rechecking its digest. This makes
    /// directory artifacts fail closed if a member changed after inspection.
    pub fn copy_member_to(&self, path: &str, writer: &mut impl Write) -> Result<(), MdafError> {
        let member = self
            .member(path)
            .ok_or_else(|| MdafError::InvalidMember(format!("undeclared member: {path}")))?;
        let (size, digest) = copy_member(&self.artifact_path, self.representation, path, writer)?;
        if size != member.size || digest != member.digest {
            return Err(MdafError::InvalidMember(format!(
                "member changed since validation: {path}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum MdafError {
    Io(io::Error),
    Zip(zip::result::ZipError),
    InvalidContainer(String),
    InvalidMember(String),
}

impl Display for MdafError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "MDAF I/O error: {error}"),
            Self::Zip(error) => write!(formatter, "MDAF ZIP error: {error}"),
            Self::InvalidContainer(message) => {
                write!(formatter, "invalid MDAF container: {message}")
            }
            Self::InvalidMember(message) => write!(formatter, "invalid MDAF member: {message}"),
        }
    }
}

impl std::error::Error for MdafError {}

impl From<io::Error> for MdafError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<zip::result::ZipError> for MdafError {
    fn from(value: zip::result::ZipError) -> Self {
        Self::Zip(value)
    }
}

/// Inspect and validate either an unpacked MDAF directory or a `.mdaf` ZIP.
/// Structural and semantic failures are deterministic diagnostics; failures to
/// open or safely enumerate the container are returned as errors.
pub fn inspect_mdaf(path: &Path) -> Result<MdafArtifact, MdafError> {
    let representation = if path.is_dir() {
        MdafRepresentation::Directory
    } else {
        MdafRepresentation::Zip
    };
    let observed = observe_members(path, representation)?;
    let identity = logical_identity(&observed);
    let mut diagnostics = Vec::new();

    let info_value = read_json_member(path, representation, "info.json", &mut diagnostics);
    if let Some(value) = info_value.as_ref() {
        validate_schema("info.json", INFO_SCHEMA, value, &mut diagnostics);
    }
    let manifest = info_value.and_then(|value| parse_control("info.json", value, &mut diagnostics));

    let markdown = read_text_member(
        path,
        representation,
        "text.md",
        MDAF_MAX_MARKDOWN_BYTES,
        &mut diagnostics,
    );
    let provenance = read_and_parse_control::<MdafProvenance>(
        path,
        representation,
        "provenance.json",
        PROVENANCE_SCHEMA,
        &mut diagnostics,
    );
    let source_map = if observed.contains_key("source-map.json") {
        read_and_parse_control::<MdafSourceMap>(
            path,
            representation,
            "source-map.json",
            SOURCE_MAP_SCHEMA,
            &mut diagnostics,
        )
    } else {
        None
    };
    let outline = if observed.contains_key("outline.json") {
        read_and_parse_control::<MdafOutline>(
            path,
            representation,
            "outline.json",
            OUTLINE_SCHEMA,
            &mut diagnostics,
        )
    } else {
        None
    };

    if let Some(manifest) = manifest.as_ref() {
        validate_manifest(manifest, &observed, &mut diagnostics);
        validate_semantics(
            manifest,
            markdown.as_deref(),
            source_map.as_ref(),
            outline.as_ref(),
            provenance.as_ref(),
            &mut diagnostics,
        );
    }
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    let valid = !diagnostics
        .iter()
        .any(|item| item.severity == MdafDiagnosticSeverity::Error);

    Ok(MdafArtifact {
        artifact_path: path.to_path_buf(),
        representation,
        identity,
        valid,
        manifest,
        markdown,
        source_map,
        outline,
        provenance,
        observed_members: observed.into_values().collect(),
        diagnostics,
    })
}

fn observe_members(
    path: &Path,
    representation: MdafRepresentation,
) -> Result<BTreeMap<String, MdafObservedMember>, MdafError> {
    match representation {
        MdafRepresentation::Directory => observe_directory(path),
        MdafRepresentation::Zip => observe_zip(path),
    }
}

fn observe_directory(root: &Path) -> Result<BTreeMap<String, MdafObservedMember>, MdafError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(MdafError::InvalidContainer(format!(
                    "symbolic links are forbidden: {}",
                    entry.path().display()
                )));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                return Err(MdafError::InvalidContainer(format!(
                    "non-regular member is forbidden: {}",
                    entry.path().display()
                )));
            }
            let entry_path = entry.path();
            let relative = entry_path.strip_prefix(root).map_err(|_| {
                MdafError::InvalidContainer("member escaped artifact root".to_string())
            })?;
            let member_path = normalized_member_path(relative)?;
            let size = entry.metadata()?.len();
            enforce_member_limits(files.len() + 1, size, &mut total)?;
            let digest = hash_reader(File::open(entry_path)?, size)?;
            insert_observed(&mut files, member_path, size, digest)?;
        }
    }
    Ok(files)
}

fn observe_zip(path: &Path) -> Result<BTreeMap<String, MdafObservedMember>, MdafError> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    if archive.len() > MDAF_MAX_FILES {
        return Err(MdafError::InvalidContainer(format!(
            "archive has {} entries; limit is {MDAF_MAX_FILES}",
            archive.len()
        )));
    }
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let enclosed = file.enclosed_name().ok_or_else(|| {
            MdafError::InvalidContainer(format!("unsafe ZIP member path: {}", file.name()))
        })?;
        let member_path = normalized_member_path(enclosed)?;
        if file.is_dir() {
            continue;
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err(MdafError::InvalidContainer(format!(
                "symbolic ZIP members are forbidden: {member_path}"
            )));
        }
        let size = file.size();
        enforce_member_limits(files.len() + 1, size, &mut total)?;
        if size > 0
            && (file.compressed_size() == 0
                || size / file.compressed_size().max(1) > MDAF_MAX_COMPRESSION_RATIO)
        {
            return Err(MdafError::InvalidContainer(format!(
                "ZIP member exceeds compression-ratio limit: {member_path}"
            )));
        }
        let digest = hash_reader(&mut file, size)?;
        insert_observed(&mut files, member_path, size, digest)?;
    }
    Ok(files)
}

fn enforce_member_limits(count: usize, size: u64, total: &mut u64) -> Result<(), MdafError> {
    if count > MDAF_MAX_FILES {
        return Err(MdafError::InvalidContainer(format!(
            "artifact has more than {MDAF_MAX_FILES} files"
        )));
    }
    if size > MDAF_MAX_MEMBER_BYTES {
        return Err(MdafError::InvalidContainer(format!(
            "member size {size} exceeds {MDAF_MAX_MEMBER_BYTES} byte limit"
        )));
    }
    *total = total
        .checked_add(size)
        .ok_or_else(|| MdafError::InvalidContainer("expanded size overflow".to_string()))?;
    if *total > MDAF_MAX_TOTAL_BYTES {
        return Err(MdafError::InvalidContainer(format!(
            "expanded artifact exceeds {MDAF_MAX_TOTAL_BYTES} byte limit"
        )));
    }
    Ok(())
}

fn insert_observed(
    files: &mut BTreeMap<String, MdafObservedMember>,
    path: String,
    size: u64,
    digest: String,
) -> Result<(), MdafError> {
    let folded = path.to_lowercase();
    if files
        .keys()
        .any(|existing| existing.to_lowercase() == folded)
    {
        return Err(MdafError::InvalidContainer(format!(
            "duplicate or case-fold-colliding member: {path}"
        )));
    }
    files.insert(path.clone(), MdafObservedMember { path, size, digest });
    Ok(())
}

fn normalized_member_path(path: &Path) -> Result<String, MdafError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    MdafError::InvalidContainer("member path is not UTF-8".to_string())
                })?;
                if value.is_empty() || value.chars().any(char::is_control) || value.contains('\\') {
                    return Err(MdafError::InvalidContainer(format!(
                        "invalid member path component: {value:?}"
                    )));
                }
                let normalized = value.nfc().collect::<String>();
                if normalized != value {
                    return Err(MdafError::InvalidContainer(format!(
                        "member path is not NFC-normalized: {}",
                        path.display()
                    )));
                }
                parts.push(value);
            }
            _ => {
                return Err(MdafError::InvalidContainer(format!(
                    "member path must be relative and normalized: {}",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(MdafError::InvalidContainer("empty member path".to_string()));
    }
    Ok(parts.join("/"))
}

fn hash_reader(mut reader: impl Read, expected_size: u64) -> Result<String, MdafError> {
    let mut hasher = blake3::Hasher::new();
    let copied = io::copy(&mut reader, &mut DigestWriter(&mut hasher))?;
    if copied != expected_size {
        return Err(MdafError::InvalidContainer(format!(
            "member declared {expected_size} bytes but yielded {copied}"
        )));
    }
    Ok(tagged_blake3(hasher.finalize()))
}

struct DigestWriter<'a>(&'a mut blake3::Hasher);

impl Write for DigestWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn logical_identity(observed: &BTreeMap<String, MdafObservedMember>) -> String {
    let mut hasher = blake3::Hasher::new();
    for member in observed.values() {
        let path = serde_json::to_string(&member.path).expect("member path serializes");
        let record = format!(
            "{{\"path\":{path},\"size\":{},\"digest\":\"{}\"}}\n",
            member.size, member.digest
        );
        hasher.update(record.as_bytes());
    }
    tagged_blake3(hasher.finalize())
}

fn read_member_bytes(
    artifact: &Path,
    representation: MdafRepresentation,
    member: &str,
    limit: u64,
) -> Result<Vec<u8>, MdafError> {
    match representation {
        MdafRepresentation::Directory => {
            read_bounded(open_directory_member(artifact, member)?, limit)
        }
        MdafRepresentation::Zip => {
            let mut archive = ZipArchive::new(File::open(artifact)?)?;
            let file = archive.by_name(member)?;
            read_bounded(file, limit)
        }
    }
}

fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, MdafError> {
    let capacity = usize::try_from(limit.min(1024 * 1024)).unwrap_or(1024 * 1024);
    let mut bytes = Vec::with_capacity(capacity);
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(MdafError::InvalidMember(format!(
            "control member exceeds {limit} byte read limit"
        )));
    }
    Ok(bytes)
}

fn copy_member(
    artifact: &Path,
    representation: MdafRepresentation,
    member: &str,
    writer: &mut impl Write,
) -> Result<(u64, String), MdafError> {
    let mut hasher = blake3::Hasher::new();
    let size = match representation {
        MdafRepresentation::Directory => copy_and_hash(
            open_directory_member(artifact, member)?,
            writer,
            &mut hasher,
        )?,
        MdafRepresentation::Zip => {
            let mut archive = ZipArchive::new(File::open(artifact)?)?;
            let file = archive.by_name(member)?;
            copy_and_hash(file, writer, &mut hasher)?
        }
    };
    Ok((size, tagged_blake3(hasher.finalize())))
}

fn open_directory_member(root: &Path, member: &str) -> Result<File, MdafError> {
    let relative = Path::new(member);
    normalized_member_path(relative)?;
    let canonical_root = fs::canonicalize(root)?;
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(MdafError::InvalidMember(member.to_string()));
    }
    let canonical_path = fs::canonicalize(&path)?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(MdafError::InvalidMember(format!(
            "member escapes artifact root: {member}"
        )));
    }
    Ok(File::open(canonical_path)?)
}

fn copy_and_hash(
    mut reader: impl Read,
    writer: &mut impl Write,
    hasher: &mut blake3::Hasher,
) -> Result<u64, MdafError> {
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > MDAF_MAX_MEMBER_BYTES {
            return Err(MdafError::InvalidMember(
                "member grew beyond the size limit".to_string(),
            ));
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    Ok(size)
}

fn read_text_member(
    path: &Path,
    representation: MdafRepresentation,
    member: &str,
    limit: u64,
    diagnostics: &mut Vec<MdafDiagnostic>,
) -> Option<String> {
    match read_member_bytes(path, representation, member, limit) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Some(text),
            Err(error) => {
                error_diag(diagnostics, "invalid_utf8", error.to_string(), member);
                None
            }
        },
        Err(error) => {
            error_diag(diagnostics, "member_unreadable", error.to_string(), member);
            None
        }
    }
}

fn read_json_member(
    path: &Path,
    representation: MdafRepresentation,
    member: &str,
    diagnostics: &mut Vec<MdafDiagnostic>,
) -> Option<Value> {
    let text = read_text_member(
        path,
        representation,
        member,
        MDAF_MAX_CONTROL_BYTES,
        diagnostics,
    )?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            error_diag(diagnostics, "invalid_json", error.to_string(), member);
            None
        }
    }
}

fn read_and_parse_control<T: for<'de> Deserialize<'de>>(
    path: &Path,
    representation: MdafRepresentation,
    member: &str,
    schema: &str,
    diagnostics: &mut Vec<MdafDiagnostic>,
) -> Option<T> {
    let value = read_json_member(path, representation, member, diagnostics)?;
    validate_schema(member, schema, &value, diagnostics);
    parse_control(member, value, diagnostics)
}

fn parse_control<T: for<'de> Deserialize<'de>>(
    member: &str,
    value: Value,
    diagnostics: &mut Vec<MdafDiagnostic>,
) -> Option<T> {
    match serde_json::from_value(value) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            error_diag(
                diagnostics,
                "control_document_invalid",
                error.to_string(),
                member,
            );
            None
        }
    }
}

fn validate_schema(
    member: &str,
    schema_text: &str,
    value: &Value,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    let schema: Value = serde_json::from_str(schema_text).expect("bundled MDAF schema is valid");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .expect("bundled MDAF schema compiles");
    for error in validator.iter_errors(value) {
        error_diag(
            diagnostics,
            "schema_violation",
            format!("{}: {error}", error.instance_path()),
            member,
        );
    }
}

fn validate_manifest(
    manifest: &MdafManifest,
    observed: &BTreeMap<String, MdafObservedMember>,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    let mut declared = BTreeSet::new();
    for member in &manifest.members {
        if normalized_member_path(Path::new(&member.path)).is_err() {
            error_diag(
                diagnostics,
                "member_path_invalid",
                "member path is unsafe",
                &member.path,
            );
        }
        if !declared.insert(member.path.clone()) {
            error_diag(
                diagnostics,
                "member_duplicate",
                "member is declared more than once",
                &member.path,
            );
        }
        validate_member_role_path(member, diagnostics);
        if member.role != MdafMemberRole::Asset && member.size > MDAF_MAX_NON_ASSET_BYTES {
            error_diag(
                diagnostics,
                "member_size_limit",
                format!("non-asset member exceeds {MDAF_MAX_NON_ASSET_BYTES} byte limit"),
                &member.path,
            );
        }
        match observed.get(&member.path) {
            None => error_diag(
                diagnostics,
                "member_missing",
                "declared member is absent",
                &member.path,
            ),
            Some(actual) => {
                if actual.size != member.size {
                    error_diag(
                        diagnostics,
                        "member_size_mismatch",
                        format!("declared {}, observed {}", member.size, actual.size),
                        &member.path,
                    );
                }
                if actual.digest != member.digest {
                    error_diag(
                        diagnostics,
                        "member_digest_mismatch",
                        "declared BLAKE3 digest does not match",
                        &member.path,
                    );
                }
            }
        }
    }
    for path in observed.keys() {
        if path != "info.json" && !declared.contains(path) {
            error_diag(
                diagnostics,
                "member_undeclared",
                "regular member is not declared",
                path,
            );
        }
    }
    require_role(manifest, "text.md", MdafMemberRole::Primary, diagnostics);
    require_role(
        manifest,
        "provenance.json",
        MdafMemberRole::Provenance,
        diagnostics,
    );
    if observed.contains_key("source-map.json") {
        require_role(
            manifest,
            "source-map.json",
            MdafMemberRole::SourceMap,
            diagnostics,
        );
    }
    if observed.contains_key("outline.json") {
        require_role(
            manifest,
            "outline.json",
            MdafMemberRole::Outline,
            diagnostics,
        );
    }
}

fn validate_member_role_path(member: &MdafMember, diagnostics: &mut Vec<MdafDiagnostic>) {
    let valid = match member.role {
        MdafMemberRole::Primary => member.path == "text.md",
        MdafMemberRole::Provenance => member.path == "provenance.json",
        MdafMemberRole::SourceMap => member.path == "source-map.json",
        MdafMemberRole::Outline => member.path == "outline.json",
        MdafMemberRole::Asset => member.path.starts_with("assets/"),
        MdafMemberRole::Rendition => member.path.starts_with("renditions/"),
        MdafMemberRole::Source => member.path.starts_with("sources/"),
        MdafMemberRole::Environment => member.path.starts_with("environments/"),
        MdafMemberRole::Extension => member.path.starts_with("extensions/"),
    };
    if !valid {
        error_diag(
            diagnostics,
            "member_role_path_mismatch",
            format!("member path does not match role {:?}", member.role),
            &member.path,
        );
    }
    if matches!(
        member.role,
        MdafMemberRole::Rendition | MdafMemberRole::Extension
    ) && member.namespace.as_deref().is_none_or(str::is_empty)
    {
        error_diag(
            diagnostics,
            "member_namespace_missing",
            "rendition and extension members require a namespace",
            &member.path,
        );
    }
}

fn require_role(
    manifest: &MdafManifest,
    path: &str,
    expected: MdafMemberRole,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    match manifest.members.iter().find(|member| member.path == path) {
        Some(member) if member.role == expected => {}
        Some(_) => error_diag(
            diagnostics,
            "member_role_invalid",
            format!("{path} has the wrong role"),
            path,
        ),
        None => error_diag(
            diagnostics,
            "member_declaration_missing",
            format!("{path} must be declared"),
            path,
        ),
    }
}

fn validate_semantics(
    manifest: &MdafManifest,
    markdown: Option<&str>,
    source_map: Option<&MdafSourceMap>,
    outline: Option<&MdafOutline>,
    provenance: Option<&MdafProvenance>,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    let source_ids = manifest
        .sources
        .iter()
        .map(|source| source.id.as_str())
        .collect::<BTreeSet<_>>();
    if source_ids.len() != manifest.sources.len() {
        error_diag(
            diagnostics,
            "source_id_duplicate",
            "source ids must be unique",
            "info.json",
        );
    }
    for source in &manifest.sources {
        if let Some(path) = source.embedded_path.as_deref() {
            match manifest.members.iter().find(|member| member.path == path) {
                Some(member)
                    if member.role == MdafMemberRole::Source && member.digest == source.digest => {}
                _ => error_diag(
                    diagnostics,
                    "embedded_source_invalid",
                    "embedded source must be declared with matching digest",
                    path,
                ),
            }
        }
    }
    let Some(markdown) = markdown else { return };
    let markdown_digest = blake3_digest(markdown.as_bytes());
    if manifest.markdown.path != "text.md"
        || manifest.markdown.media_type != "text/markdown"
        || manifest.markdown.digest != markdown_digest
    {
        error_diag(
            diagnostics,
            "markdown_binding_invalid",
            "primary Markdown binding does not match text.md",
            "info.json",
        );
    }
    if let Some(source_map) = source_map {
        validate_source_map(
            source_map,
            markdown,
            &markdown_digest,
            &source_ids,
            diagnostics,
        );
    }
    if let Some(outline) = outline {
        validate_outline(
            outline,
            markdown,
            &markdown_digest,
            &source_ids,
            diagnostics,
        );
    }
    if let Some(provenance) = provenance {
        validate_provenance(provenance, manifest, &source_ids, diagnostics);
    }
    validate_capability(manifest, "source-map", source_map.is_some(), diagnostics);
    validate_capability(manifest, "outline", outline.is_some(), diagnostics);
    validate_role_capability(
        manifest,
        "native-renditions",
        MdafMemberRole::Rendition,
        diagnostics,
    );
    validate_role_capability(
        manifest,
        "embedded-sources",
        MdafMemberRole::Source,
        diagnostics,
    );
    validate_role_capability(
        manifest,
        "environments",
        MdafMemberRole::Environment,
        diagnostics,
    );
    validate_role_capability(
        manifest,
        "extensions",
        MdafMemberRole::Extension,
        diagnostics,
    );
}

fn validate_role_capability(
    manifest: &MdafManifest,
    capability: &str,
    role: MdafMemberRole,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    validate_capability(
        manifest,
        capability,
        manifest.members.iter().any(|member| member.role == role),
        diagnostics,
    );
}

fn validate_capability(
    manifest: &MdafManifest,
    capability: &str,
    present: bool,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    if manifest
        .capabilities
        .iter()
        .any(|value| value == capability)
        != present
    {
        error_diag(
            diagnostics,
            "capability_mismatch",
            format!("capability {capability:?} must match member presence"),
            "info.json",
        );
    }
}

fn validate_source_map(
    map: &MdafSourceMap,
    markdown: &str,
    digest: &str,
    source_ids: &BTreeSet<&str>,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    if map.document_digest != digest {
        error_diag(
            diagnostics,
            "document_digest_mismatch",
            "source map targets different Markdown",
            "source-map.json",
        );
    }
    for mapping in &map.mappings {
        validate_span(mapping.document, markdown, "source-map.json", diagnostics);
        validate_locator(&mapping.source, source_ids, "source-map.json", diagnostics);
    }
    for reference in &map.references {
        validate_span(reference.document, markdown, "source-map.json", diagnostics);
        validate_locator(
            &reference.target,
            source_ids,
            "source-map.json",
            diagnostics,
        );
    }
}

fn validate_locator(
    locator: &MdafSourceLocator,
    source_ids: &BTreeSet<&str>,
    path: &str,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    if !source_ids.contains(locator.source_id.as_str()) {
        error_diag(
            diagnostics,
            "locator_source_unknown",
            format!("unknown source {}", locator.source_id),
            path,
        );
    }
    for selector in &locator.selectors {
        let valid = match selector {
            MdafSelector::Interval {
                unit,
                start,
                end,
                origin,
                ..
            } => {
                !unit.is_empty()
                    && start.is_finite()
                    && end.is_finite()
                    && *start >= 0.0
                    && start < end
                    && origin.is_none_or(f64::is_finite)
            }
            MdafSelector::Rectangle {
                unit,
                x,
                y,
                width,
                height,
            } => {
                let finite = [x, y, width, height]
                    .into_iter()
                    .all(|value| value.is_finite());
                let bounded = match unit.as_str() {
                    "percent" => *x + *width <= 100.0 && *y + *height <= 100.0,
                    "normalized" => *x + *width <= 1.0 && *y + *height <= 1.0,
                    _ => true,
                };
                !unit.is_empty()
                    && finite
                    && *x >= 0.0
                    && *y >= 0.0
                    && *width > 0.0
                    && *height > 0.0
                    && bounded
            }
            MdafSelector::Polygon { unit, points } => {
                !unit.is_empty()
                    && points.len() >= 3
                    && points
                        .iter()
                        .all(|point| point.x.is_finite() && point.y.is_finite())
                    && polygon_area(points).abs() > f64::EPSILON
                    && match unit.as_str() {
                        "percent" => points.iter().all(|point| {
                            (0.0..=100.0).contains(&point.x) && (0.0..=100.0).contains(&point.y)
                        }),
                        "normalized" => points.iter().all(|point| {
                            (0.0..=1.0).contains(&point.x) && (0.0..=1.0).contains(&point.y)
                        }),
                        _ => true,
                    }
            }
            MdafSelector::Grid {
                row_start,
                row_end,
                column_start,
                column_end,
                ..
            } => row_start < row_end && column_start < column_end,
            MdafSelector::TextQuote { exact, .. } => !exact.is_empty(),
            MdafSelector::Fragment { value, conforms_to } => {
                !value.is_empty() && conforms_to.as_deref().is_none_or(|value| !value.is_empty())
            }
            MdafSelector::Extension { namespace, .. } => valid_namespace(namespace),
        };
        if !valid {
            error_diag(
                diagnostics,
                "source_selector_invalid",
                "source selector is empty, non-finite, degenerate, out of bounds, or not namespaced",
                path,
            );
        }
    }
}

fn polygon_area(points: &[MdafPoint]) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(left, right)| left.x * right.y - right.x * left.y)
        .sum::<f64>()
        / 2.0
}

fn valid_namespace(namespace: &str) -> bool {
    namespace
        .split_once('/')
        .is_some_and(|(authority, name)| authority.contains('.') && !name.is_empty())
}

fn validate_outline(
    outline: &MdafOutline,
    markdown: &str,
    digest: &str,
    source_ids: &BTreeSet<&str>,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    if outline.document_digest != digest {
        error_diag(
            diagnostics,
            "document_digest_mismatch",
            "outline targets different Markdown",
            "outline.json",
        );
    }
    let mut nodes: BTreeMap<&str, &MdafOutlineNode> = BTreeMap::new();
    let mut previous_start = None;
    for node in &outline.nodes {
        validate_span(node.heading, markdown, "outline.json", diagnostics);
        validate_span(node.section, markdown, "outline.json", diagnostics);
        if node.section.start > node.heading.start || node.section.end < node.heading.end {
            error_diag(
                diagnostics,
                "outline_section_invalid",
                format!("section does not contain heading for {}", node.id),
                "outline.json",
            );
        }
        if previous_start.is_some_and(|start| node.section.start < start) {
            error_diag(
                diagnostics,
                "outline_order_invalid",
                "outline nodes are not ordered by section start",
                "outline.json",
            );
        }
        previous_start = Some(node.section.start);
        for other in nodes.values() {
            let overlaps =
                node.section.start < other.section.end && other.section.start < node.section.end;
            let nested = (node.section.start >= other.section.start
                && node.section.end <= other.section.end)
                || (other.section.start >= node.section.start
                    && other.section.end <= node.section.end);
            if overlaps && !nested {
                error_diag(
                    diagnostics,
                    "outline_overlap_invalid",
                    format!(
                        "sections {} and {} overlap without nesting",
                        other.id, node.id
                    ),
                    "outline.json",
                );
            }
        }
        if let Some(parent) = node.parent.as_deref() {
            match nodes.get(parent) {
                Some(parent_node)
                    if parent_node.level < node.level
                        && parent_node.section.start <= node.section.start
                        && parent_node.section.end >= node.section.end => {}
                _ => error_diag(
                    diagnostics,
                    "outline_parent_invalid",
                    format!("invalid parent {parent} for {}", node.id),
                    "outline.json",
                ),
            }
        }
        if let Some(source) = node.source.as_ref() {
            validate_locator(source, source_ids, "outline.json", diagnostics);
        }
        if nodes.insert(node.id.as_str(), node).is_some() {
            error_diag(
                diagnostics,
                "outline_id_duplicate",
                format!("duplicate node id {}", node.id),
                "outline.json",
            );
        }
    }
}

fn validate_span(
    span: MdafByteSpan,
    markdown: &str,
    path: &str,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    if span.start >= span.end
        || span.end > markdown.len()
        || !markdown.is_char_boundary(span.start)
        || !markdown.is_char_boundary(span.end)
    {
        error_diag(
            diagnostics,
            "document_span_invalid",
            format!("invalid UTF-8 byte span {}..{}", span.start, span.end),
            path,
        );
    }
}

fn validate_provenance(
    provenance: &MdafProvenance,
    manifest: &MdafManifest,
    source_ids: &BTreeSet<&str>,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    let member_paths = manifest
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    let activity_ids = provenance
        .activities
        .iter()
        .map(|activity| activity.id.as_str())
        .collect::<BTreeSet<_>>();
    if activity_ids.len() != provenance.activities.len() {
        error_diag(
            diagnostics,
            "activity_id_duplicate",
            "activity ids must be unique",
            "provenance.json",
        );
    }
    for activity in &provenance.activities {
        let expected = blake3_digest(&canonical_json(&Value::Object(activity.parameters.clone())));
        if expected != activity.parameters_digest {
            error_diag(
                diagnostics,
                "parameters_digest_mismatch",
                format!("parameter digest mismatch for {}", activity.id),
                "provenance.json",
            );
        }
        for dependency in &activity.depends_on {
            if !activity_ids.contains(dependency.as_str()) || dependency == &activity.id {
                error_diag(
                    diagnostics,
                    "activity_dependency_invalid",
                    format!("invalid dependency {dependency}"),
                    "provenance.json",
                );
            }
        }
        for output in &activity.outputs {
            if !member_paths.contains(output.as_str()) {
                error_diag(
                    diagnostics,
                    "activity_output_unknown",
                    format!("unknown output {output}"),
                    "provenance.json",
                );
            }
        }
        for input in &activity.inputs {
            let source = input.strip_prefix("source:").unwrap_or(input);
            if !member_paths.contains(input.as_str()) && !source_ids.contains(source) {
                error_diag(
                    diagnostics,
                    "activity_input_unknown",
                    format!("unknown input {input}"),
                    "provenance.json",
                );
            }
        }
        for model in &activity.models {
            if model.resolution != MdafModelResolution::Pinned {
                warning_diag(
                    diagnostics,
                    "model_not_pinned",
                    format!("model {} is not pinned", model.identifier),
                    "provenance.json",
                );
            }
        }
    }
    for member in &manifest.members {
        match provenance
            .activities
            .iter()
            .find(|activity| activity.id == member.created_by)
        {
            Some(activity) if activity.outputs.contains(&member.path) => {}
            _ => error_diag(
                diagnostics,
                "member_provenance_invalid",
                format!("{} is not emitted by {}", member.path, member.created_by),
                "provenance.json",
            ),
        }
    }
    if has_activity_cycle(&provenance.activities) {
        error_diag(
            diagnostics,
            "activity_cycle",
            "activity dependencies contain a cycle",
            "provenance.json",
        );
    }
}

fn has_activity_cycle(activities: &[MdafActivity]) -> bool {
    fn visit<'a>(
        id: &'a str,
        graph: &BTreeMap<&'a str, &'a [String]>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
    ) -> bool {
        if visited.contains(id) {
            return false;
        }
        if !visiting.insert(id) {
            return true;
        }
        if graph.get(id).is_some_and(|dependencies| {
            dependencies
                .iter()
                .any(|dependency| visit(dependency, graph, visiting, visited))
        }) {
            return true;
        }
        visiting.remove(id);
        visited.insert(id);
        false
    }
    let graph = activities
        .iter()
        .map(|activity| (activity.id.as_str(), activity.depends_on.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    graph
        .keys()
        .any(|id| visit(id, &graph, &mut visiting, &mut visited))
}

fn canonical_json(value: &Value) -> Vec<u8> {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), sort(value)))
                    .collect(),
            ),
            Value::Array(array) => Value::Array(array.iter().map(sort).collect()),
            _ => value.clone(),
        }
    }
    serde_json::to_vec(&sort(value)).expect("JSON value serializes")
}

fn blake3_digest(bytes: &[u8]) -> String {
    tagged_blake3(blake3::hash(bytes))
}

fn tagged_blake3(hash: blake3::Hash) -> String {
    format!("blake3:{hash}")
}

fn error_diag(
    diagnostics: &mut Vec<MdafDiagnostic>,
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
) {
    diagnostics.push(MdafDiagnostic {
        severity: MdafDiagnosticSeverity::Error,
        code: code.into(),
        message: message.into(),
        path: Some(path.into()),
    });
}

fn warning_diag(
    diagnostics: &mut Vec<MdafDiagnostic>,
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
) {
    diagnostics.push(MdafDiagnostic {
        severity: MdafDiagnosticSeverity::Warning,
        code: code.into(),
        message: message.into(),
        path: Some(path.into()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;
    use zip::write::FileOptions;

    fn write_minimal(root: &Path, native_path: Option<&str>) {
        fs::create_dir_all(root).expect("create artifact");
        let markdown = "# Synthetic\n\nPrivate-free fixture.\n";
        let parameters = serde_json::json!({"mode":"synthetic"});
        let parameters_digest = blake3_digest(&canonical_json(&parameters));
        let mut outputs = vec!["text.md", "provenance.json"];
        if let Some(path) = native_path {
            outputs.push(path);
        }
        let provenance = serde_json::json!({
            "version": 1,
            "activities": [{
                "id": "activity:extract", "kind": "document-extraction",
                "tools": [{"name":"synthetic-extractor","version":"2.0.0"}],
                "models": [], "inputs": ["source:synthetic"], "outputs": outputs,
                "depends_on": [], "parameters": parameters,
                "parameters_digest": parameters_digest
            }],
            "redactions": []
        });
        let provenance_bytes =
            serde_json::to_vec_pretty(&provenance).expect("serialize provenance");
        fs::write(root.join("text.md"), markdown).expect("write Markdown");
        fs::write(root.join("provenance.json"), &provenance_bytes).expect("write provenance");
        if let Some(path) = native_path {
            let target = root.join(path);
            fs::create_dir_all(target.parent().expect("native parent"))
                .expect("create native parent");
            fs::write(target, br#"{"opaque":[1,2,3]}"#).expect("write native");
        }
        let mut members = vec![
            serde_json::json!({"path":"text.md","role":"primary","media_type":"text/markdown","size":markdown.len(),"digest":blake3_digest(markdown.as_bytes()),"created_by":"activity:extract"}),
            serde_json::json!({"path":"provenance.json","role":"provenance","media_type":"application/json","size":provenance_bytes.len(),"digest":blake3_digest(&provenance_bytes),"created_by":"activity:extract"}),
        ];
        let mut capabilities = Vec::<String>::new();
        if let Some(path) = native_path {
            let bytes = fs::read(root.join(path)).expect("read native");
            members.push(serde_json::json!({"path":path,"role":"rendition","media_type":"application/json","size":bytes.len(),"digest":blake3_digest(&bytes),"created_by":"activity:extract","namespace":"example.test/extractor"}));
            capabilities.push("native-renditions".to_string());
        }
        let info = serde_json::json!({
            "format":"mdaf", "version":1,
            "markdown":{"path":"text.md","digest":blake3_digest(markdown.as_bytes()),"media_type":"text/markdown"},
            "producer":{"name":"fixture-builder","version":"1.0.0"},
            "members":members,
            "sources":[{"id":"synthetic","media_type":"application/octet-stream","digest":blake3_digest(b"not embedded"),"alternate_digests":["sha256:0f45120997903fa1935dfe7f31b99e939842129c76d6bf7140348ebcf054deb3"]}],
            "capabilities":capabilities
        });
        fs::write(
            root.join("info.json"),
            serde_json::to_vec_pretty(&info).expect("serialize info"),
        )
        .expect("write info");
    }

    fn zip_directory(root: &Path, destination: &Path) {
        let file = File::create(destination).expect("create ZIP");
        let mut writer = zip::ZipWriter::new(file);
        let options = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for path in [
            "info.json",
            "text.md",
            "provenance.json",
            "renditions/provider/native.json",
        ] {
            let source = root.join(path);
            if !source.exists() {
                continue;
            }
            writer.start_file(path, options).expect("start ZIP member");
            writer
                .write_all(&fs::read(source).expect("read member"))
                .expect("write member");
        }
        writer.finish().expect("finish ZIP");
    }

    #[test]
    fn directory_and_zip_have_the_same_identity_and_preserve_native_members() {
        let temporary = TempDir::new().expect("temporary directory");
        let root = temporary.path().join("artifact");
        write_minimal(&root, Some("renditions/provider/native.json"));
        let zip = temporary.path().join("artifact.mdaf");
        zip_directory(&root, &zip);

        let directory = inspect_mdaf(&root).expect("inspect directory");
        let archive = inspect_mdaf(&zip).expect("inspect ZIP");
        assert!(directory.valid, "{:?}", directory.diagnostics);
        assert!(archive.valid, "{:?}", archive.diagnostics);
        assert_eq!(directory.identity, archive.identity);
        assert!(archive.member("renditions/provider/native.json").is_some());
        let mut copied = Vec::new();
        archive
            .copy_member_to("renditions/provider/native.json", &mut copied)
            .expect("copy native member");
        assert_eq!(copied, br#"{"opaque":[1,2,3]}"#);
    }

    #[test]
    fn digest_mismatches_are_reported_without_interpreting_native_data() {
        let temporary = TempDir::new().expect("temporary directory");
        write_minimal(temporary.path(), Some("renditions/provider/native.json"));
        fs::write(
            temporary.path().join("renditions/provider/native.json"),
            b"changed",
        )
        .expect("change native");
        let artifact = inspect_mdaf(temporary.path()).expect("inspect");
        assert!(!artifact.valid);
        assert!(artifact
            .diagnostics
            .iter()
            .any(|item| item.code == "member_digest_mismatch"));
    }

    #[test]
    fn legacy_sha256_manifest_fields_are_rejected() {
        let temporary = TempDir::new().expect("temporary directory");
        write_minimal(temporary.path(), None);
        let info_path = temporary.path().join("info.json");
        let mut info: Value = serde_json::from_slice(&fs::read(&info_path).expect("read manifest"))
            .expect("parse manifest");
        let markdown = info["markdown"].as_object_mut().expect("Markdown binding");
        let digest = markdown.remove("digest").expect("canonical digest");
        markdown.insert("sha256".to_string(), digest);
        fs::write(
            info_path,
            serde_json::to_vec_pretty(&info).expect("serialize legacy manifest"),
        )
        .expect("write legacy manifest");

        let artifact = inspect_mdaf(temporary.path()).expect("inspect");
        assert!(!artifact.valid);
        assert!(artifact
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "schema_violation"));
        assert!(artifact
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "control_document_invalid"));
    }

    #[test]
    fn zip_traversal_and_symlinked_directory_members_fail_closed() {
        let temporary = TempDir::new().expect("temporary directory");
        let zip_path = temporary.path().join("unsafe.mdaf");
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut bytes);
            writer
                .start_file("../outside", FileOptions::default())
                .expect("start unsafe path");
            writer.write_all(b"bad").expect("write unsafe path");
            writer.finish().expect("finish ZIP");
        }
        fs::write(&zip_path, bytes.into_inner()).expect("write ZIP");
        assert!(inspect_mdaf(&zip_path).is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let root = temporary.path().join("directory");
            write_minimal(&root, None);
            symlink(root.join("text.md"), root.join("linked.md")).expect("create symlink");
            assert!(inspect_mdaf(&root).is_err());
        }
    }

    #[test]
    fn checked_in_identity_vector_matches_the_algorithm() {
        let vector: Value = serde_json::from_str(include_str!(
            "../../docs/specs/mdaf/v1/identity-test-vector.json"
        ))
        .expect("parse vector");
        let files = vector["files"].as_object().expect("files");
        let observed = files
            .iter()
            .map(|(path, contents)| {
                let bytes = contents.as_str().expect("contents").as_bytes();
                let item = MdafObservedMember {
                    path: path.clone(),
                    size: bytes.len() as u64,
                    digest: blake3_digest(bytes),
                };
                (item.path.clone(), item)
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            logical_identity(&observed),
            vector["identity"].as_str().expect("identity")
        );
    }

    #[test]
    fn bundled_schemas_match_the_published_spec() {
        assert_eq!(
            INFO_SCHEMA,
            include_str!("../../docs/specs/mdaf/v1/info.schema.json")
        );
        assert_eq!(
            SOURCE_MAP_SCHEMA,
            include_str!("../../docs/specs/mdaf/v1/source-map.schema.json")
        );
        assert_eq!(
            OUTLINE_SCHEMA,
            include_str!("../../docs/specs/mdaf/v1/outline.schema.json")
        );
        assert_eq!(
            PROVENANCE_SCHEMA,
            include_str!("../../docs/specs/mdaf/v1/provenance.schema.json")
        );
        let source_map: Value = serde_json::from_str(SOURCE_MAP_SCHEMA).expect("source-map schema");
        let outline: Value = serde_json::from_str(OUTLINE_SCHEMA).expect("outline schema");
        for definition in [
            "locator",
            "selector",
            "interval",
            "rectangle",
            "point",
            "polygon",
            "grid",
            "text_quote",
            "fragment",
            "extension",
        ] {
            assert_eq!(
                source_map["$defs"][definition], outline["$defs"][definition],
                "shared locator schema definition {definition} drifted"
            );
        }
    }

    #[test]
    fn rejects_invalid_normalized_selectors_and_accepts_whole_source_locators() {
        let source_ids = BTreeSet::from(["known"]);
        let mut diagnostics = Vec::new();
        validate_locator(
            &MdafSourceLocator {
                source_id: "known".to_string(),
                selectors: Vec::new(),
            },
            &source_ids,
            "source-map.json",
            &mut diagnostics,
        );
        assert!(diagnostics.is_empty());

        let invalid = MdafSourceLocator {
            source_id: "missing".to_string(),
            selectors: vec![
                MdafSelector::Interval {
                    unit: "page".to_string(),
                    start: 2.0,
                    end: 2.0,
                    origin: None,
                    label_start: None,
                    label_end: None,
                },
                MdafSelector::Rectangle {
                    unit: "normalized".to_string(),
                    x: 0.8,
                    y: 0.0,
                    width: 0.3,
                    height: 0.5,
                },
                MdafSelector::Polygon {
                    unit: "pixel".to_string(),
                    points: vec![
                        MdafPoint { x: 0.0, y: 0.0 },
                        MdafPoint { x: 1.0, y: 1.0 },
                        MdafPoint { x: 2.0, y: 2.0 },
                    ],
                },
                MdafSelector::Grid {
                    sheet: None,
                    row_start: 1,
                    row_end: 1,
                    column_start: 0,
                    column_end: 1,
                },
                MdafSelector::TextQuote {
                    exact: String::new(),
                    prefix: None,
                    suffix: None,
                },
                MdafSelector::Fragment {
                    value: String::new(),
                    conforms_to: None,
                },
                MdafSelector::Extension {
                    namespace: "not-namespaced".to_string(),
                    data: Value::Null,
                },
            ],
        };
        validate_locator(&invalid, &source_ids, "source-map.json", &mut diagnostics);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "source_selector_invalid")
                .count(),
            7
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "locator_source_unknown"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn validates_source_neutral_selectors_aligned_outline_and_multi_tool_provenance() {
        let markdown = "# A\n\nBody\n\n## B\n\nMore\n";
        let digest = blake3_digest(markdown.as_bytes());
        let manifest = MdafManifest {
            format: "mdaf".to_string(),
            version: 1,
            title: None,
            markdown: MdafMarkdownBinding {
                path: "text.md".to_string(),
                digest: digest.clone(),
                media_type: "text/markdown".to_string(),
                variant: None,
                features: Vec::new(),
            },
            producer: MdafProducer {
                name: "neutral-producer".to_string(),
                version: "1".to_string(),
                revision: None,
            },
            members: vec![MdafMember {
                path: "text.md".to_string(),
                role: MdafMemberRole::Primary,
                media_type: "text/markdown".to_string(),
                size: markdown.len() as u64,
                digest: digest.clone(),
                created_by: "activity:normalize".to_string(),
                schema: None,
                namespace: None,
            }],
            sources: vec![MdafSource {
                id: "source-a".to_string(),
                media_type: "application/octet-stream".to_string(),
                digest: blake3_digest(b"source"),
                alternate_digests: vec![
                    "sha256:41cf6794ba4200b839c53531555f0f3998df4cbb01a4d5cb0b94e3ca5e23947d"
                        .to_string(),
                ],
                name: None,
                embedded_path: None,
            }],
            capabilities: vec!["source-map".to_string(), "outline".to_string()],
            derived_from: vec![blake3_digest(b"parent")],
        };
        let source_map = MdafSourceMap {
            version: 1,
            document_digest: digest.clone(),
            mappings: vec![MdafSourceMapping {
                document: MdafByteSpan {
                    start: 0,
                    end: markdown.len(),
                },
                source: MdafSourceLocator {
                    source_id: "source-a".to_string(),
                    selectors: vec![
                        MdafSelector::Interval {
                            unit: "page".to_string(),
                            start: 1.0,
                            end: 3.0,
                            origin: Some(1.0),
                            label_start: Some("1".to_string()),
                            label_end: Some("2".to_string()),
                        },
                        MdafSelector::Rectangle {
                            unit: "percent".to_string(),
                            x: 10.0,
                            y: 20.0,
                            width: 30.0,
                            height: 40.0,
                        },
                        MdafSelector::Polygon {
                            unit: "normalized".to_string(),
                            points: vec![
                                MdafPoint { x: 0.1, y: 0.1 },
                                MdafPoint { x: 0.4, y: 0.1 },
                                MdafPoint { x: 0.2, y: 0.4 },
                            ],
                        },
                        MdafSelector::Grid {
                            sheet: Some("Rules".to_string()),
                            row_start: 0,
                            row_end: 2,
                            column_start: 1,
                            column_end: 3,
                        },
                        MdafSelector::TextQuote {
                            exact: "Body".to_string(),
                            prefix: None,
                            suffix: None,
                        },
                        MdafSelector::Fragment {
                            value: "chapter-a".to_string(),
                            conforms_to: Some("https://example.test/fragments".to_string()),
                        },
                        MdafSelector::Extension {
                            namespace: "example.test/selector".to_string(),
                            data: serde_json::json!({"mask": "region-1"}),
                        },
                    ],
                },
                confidence: Some(0.9),
                method: Some("provider.example/alignment".to_string()),
            }],
            references: Vec::new(),
        };
        let outline = MdafOutline {
            version: 1,
            document_digest: digest,
            title: None,
            nodes: vec![
                MdafOutlineNode {
                    id: "a".to_string(),
                    parent: None,
                    level: 1,
                    title: "A".to_string(),
                    heading: MdafByteSpan { start: 0, end: 3 },
                    section: MdafByteSpan {
                        start: 0,
                        end: markdown.len(),
                    },
                    source: None,
                },
                MdafOutlineNode {
                    id: "b".to_string(),
                    parent: Some("a".to_string()),
                    level: 2,
                    title: "B".to_string(),
                    heading: MdafByteSpan { start: 12, end: 16 },
                    section: MdafByteSpan {
                        start: 12,
                        end: markdown.len(),
                    },
                    source: Some(MdafSourceLocator {
                        source_id: "source-a".to_string(),
                        selectors: vec![MdafSelector::Interval {
                            unit: "page".to_string(),
                            start: 2.0,
                            end: 3.0,
                            origin: Some(1.0),
                            label_start: Some("2".to_string()),
                            label_end: None,
                        }],
                    }),
                },
            ],
        };
        let parameters = serde_json::Map::from_iter([(
            "threshold".to_string(),
            Value::String("0.9".to_string()),
        )]);
        let provenance = MdafProvenance {
            version: 1,
            activities: vec![MdafActivity {
                id: "activity:normalize".to_string(),
                kind: "normalization".to_string(),
                started_at: None,
                ended_at: None,
                tools: vec![
                    MdafTool {
                        name: "extractor".to_string(),
                        version: "2.1".to_string(),
                        revision: Some("abc".to_string()),
                        package_url: None,
                    },
                    MdafTool {
                        name: "normalizer".to_string(),
                        version: "1.3".to_string(),
                        revision: None,
                        package_url: None,
                    },
                ],
                models: vec![MdafModel {
                    provider: "provider.example".to_string(),
                    identifier: "ocr-stable".to_string(),
                    returned_identifier: None,
                    revision: Some("2026-01".to_string()),
                    checksum: None,
                    resolution: MdafModelResolution::Pinned,
                }],
                inputs: vec!["source:source-a".to_string()],
                outputs: vec!["text.md".to_string()],
                depends_on: Vec::new(),
                parameters_digest: blake3_digest(&canonical_json(&Value::Object(
                    parameters.clone(),
                ))),
                parameters,
            }],
            redactions: Vec::new(),
        };
        let mut diagnostics = Vec::new();
        validate_semantics(
            &manifest,
            Some(markdown),
            Some(&source_map),
            Some(&outline),
            Some(&provenance),
            &mut diagnostics,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}
