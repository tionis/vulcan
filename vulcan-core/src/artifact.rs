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

use crate::exchange::{
    blake3_digest, error_diag, has_errors, sort_diagnostics, tagged_blake3, validate_locator,
    validate_provenance, validate_schema, validate_sources, validate_span, ExchangeActivity,
    ExchangeByteSpan, ExchangeDiagnostic, ExchangeDiagnosticSeverity, ExchangeModel,
    ExchangeModelResolution, ExchangePoint, ExchangeProducer, ExchangeProvenance,
    ExchangeRedaction, ExchangeSelector, ExchangeSource, ExchangeSourceLocator, ExchangeTool,
    ProvenanceMember,
};

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

pub type MdafProducer = ExchangeProducer;
pub type MdafSource = ExchangeSource;
pub type MdafByteSpan = ExchangeByteSpan;
pub type MdafSelector = ExchangeSelector;
pub type MdafPoint = ExchangePoint;
pub type MdafSourceLocator = ExchangeSourceLocator;
pub type MdafTool = ExchangeTool;
pub type MdafModelResolution = ExchangeModelResolution;
pub type MdafModel = ExchangeModel;
pub type MdafActivity = ExchangeActivity;
pub type MdafRedaction = ExchangeRedaction;
pub type MdafProvenance = ExchangeProvenance;
pub type MdafDiagnosticSeverity = ExchangeDiagnosticSeverity;
pub type MdafDiagnostic = ExchangeDiagnostic;

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
    /// The primary Markdown's language, a BCP 47 tag such as `en` or `de-CH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// A declared image asset that represents the document (a cover or thumbnail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
}

/// How a run of a paged source's pages is labeled, from its zero-based `start` page until the
/// next range: the model of PDF page labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MdafPageLabelRange {
    pub start: u64,
    pub style: MdafPageLabelStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// The number of the range's first page (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MdafPageLabelStyle {
    Decimal,
    RomanUpper,
    RomanLower,
    AlphaUpper,
    AlphaLower,
    /// Only the prefix, no number.
    None,
}

/// The label of a zero-based page under `ranges` (sorted by `start`), or `None` when no range
/// covers it.
#[must_use]
pub fn page_label(ranges: &[MdafPageLabelRange], page: u64) -> Option<String> {
    let range = ranges.iter().rev().find(|range| range.start <= page)?;
    let number = range.first.unwrap_or(1) + (page - range.start);
    let numeral = match range.style {
        MdafPageLabelStyle::Decimal => number.to_string(),
        MdafPageLabelStyle::RomanUpper => roman(number),
        MdafPageLabelStyle::RomanLower => roman(number).to_lowercase(),
        MdafPageLabelStyle::AlphaUpper => alpha(number),
        MdafPageLabelStyle::AlphaLower => alpha(number).to_lowercase(),
        MdafPageLabelStyle::None => String::new(),
    };
    Some(format!(
        "{}{numeral}",
        range.prefix.as_deref().unwrap_or("")
    ))
}

fn roman(mut number: u64) -> String {
    const NUMERALS: [(u64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, numeral) in NUMERALS {
        while number >= value {
            out.push_str(numeral);
            number -= value;
        }
    }
    out
}

/// PDF's letters: A to Z, then AA to ZZ, then AAA and so on (the letter repeated).
fn alpha(number: u64) -> String {
    if number == 0 {
        return String::new();
    }
    let letter = char::from(b'A' + u8::try_from((number - 1) % 26).unwrap_or(0));
    let times = usize::try_from((number - 1) / 26 + 1).unwrap_or(1);
    letter.to_string().repeat(times)
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
    /// Page labels of paged sources, by source ID (printed page numbers, roman front matter).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub page_labels: BTreeMap<String, Vec<MdafPageLabelRange>>,
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
    sort_diagnostics(&mut diagnostics);
    let valid = !has_errors(&diagnostics);

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
            MdafError::InvalidContainer(format!(
                "unsafe ZIP member path: {}",
                String::from_utf8_lossy(file.name_raw())
            ))
        })?;
        let member_path = normalized_member_path(&enclosed)?;
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
    let member_digests = manifest
        .members
        .iter()
        .map(|member| {
            (
                member.path.as_str(),
                (
                    member.digest.as_str(),
                    member.role == MdafMemberRole::Source,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    validate_sources(&manifest.sources, &member_digests, "info.json", diagnostics);
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
            "provenance.json",
            diagnostics,
        );
    }
    validate_document_metadata(manifest, diagnostics);
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

/// `language` is a well-formed BCP 47 tag; `cover` names a declared image asset.
fn validate_document_metadata(manifest: &MdafManifest, diagnostics: &mut Vec<MdafDiagnostic>) {
    if let Some(language) = &manifest.language {
        if !is_language_tag(language) {
            error_diag(
                diagnostics,
                "language_invalid",
                format!("language {language:?} is not a BCP 47 tag"),
                "info.json",
            );
        }
    }
    if let Some(cover) = &manifest.cover {
        let member = manifest.members.iter().find(|member| &member.path == cover);
        if !member.is_some_and(|member| {
            member.role == MdafMemberRole::Asset && member.media_type.starts_with("image/")
        }) {
            error_diag(
                diagnostics,
                "cover_invalid",
                format!("cover {cover:?} must name a declared image asset"),
                "info.json",
            );
        }
    }
}

/// A well-formed BCP 47 tag (the syntax: a 2 to 8 letter language, then subtags of 1 to 8
/// letters or digits); registered subtags are not checked.
fn is_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    (2..=8).contains(&language.len())
        && language.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|part| {
            (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

/// Page label ranges name declared sources and start in strictly increasing order.
fn validate_page_labels(
    map: &MdafSourceMap,
    source_ids: &BTreeSet<&str>,
    diagnostics: &mut Vec<MdafDiagnostic>,
) {
    for (source, ranges) in &map.page_labels {
        if !source_ids.contains(source.as_str()) {
            error_diag(
                diagnostics,
                "page_labels_source_unknown",
                format!("page labels name unknown source {source:?}"),
                "source-map.json",
            );
        }
        let increasing = ranges.windows(2).all(|pair| pair[0].start < pair[1].start);
        let numbered = ranges
            .iter()
            .all(|range| range.first.is_none_or(|first| first >= 1));
        if ranges.is_empty() || !increasing || !numbered {
            error_diag(
                diagnostics,
                "page_labels_invalid",
                format!(
                    "page labels of {source:?} need ranges in increasing order, numbered from 1"
                ),
                "source-map.json",
            );
        }
    }
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
    validate_page_labels(map, source_ids, diagnostics);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::canonical_json;
    use std::io::Cursor;
    use tempfile::TempDir;
    use zip::write::SimpleFileOptions;

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
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
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
                .start_file("../outside", SimpleFileOptions::default())
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
            language: None,
            cover: None,
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
            page_labels: BTreeMap::new(),
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

    fn labels(
        ranges: &[(u64, MdafPageLabelStyle, Option<&str>, Option<u64>)],
    ) -> Vec<MdafPageLabelRange> {
        ranges
            .iter()
            .map(|(start, style, prefix, first)| MdafPageLabelRange {
                start: *start,
                style: *style,
                prefix: prefix.map(str::to_string),
                first: *first,
            })
            .collect()
    }

    #[test]
    fn labels_pages_as_pdf_page_labels_do() {
        // A cover, roman front matter, then the book from its printed page 1, and an appendix.
        let ranges = labels(&[
            (0, MdafPageLabelStyle::None, Some("Cover"), None),
            (1, MdafPageLabelStyle::RomanLower, None, None),
            (5, MdafPageLabelStyle::Decimal, None, None),
            (300, MdafPageLabelStyle::AlphaUpper, Some("App. "), Some(1)),
        ]);
        let label = |page| page_label(&ranges, page);
        assert_eq!(label(0).as_deref(), Some("Cover"));
        assert_eq!(label(1).as_deref(), Some("i"));
        assert_eq!(label(4).as_deref(), Some("iv"));
        assert_eq!(label(5).as_deref(), Some("1"));
        assert_eq!(label(299).as_deref(), Some("295"));
        assert_eq!(label(300).as_deref(), Some("App. A"));
        assert_eq!(label(327).as_deref(), Some("App. BB"));
        assert_eq!(roman(1994), "MCMXCIV");
        // An offset: printed page 1 is the PDF's third page.
        let offset = labels(&[(2, MdafPageLabelStyle::Decimal, None, None)]);
        assert_eq!(page_label(&offset, 0), None);
        assert_eq!(page_label(&offset, 2).as_deref(), Some("1"));
    }

    #[test]
    fn validates_language_cover_and_page_labels() {
        let member = |path: &str, role, media_type: &str| MdafMember {
            path: path.to_string(),
            role,
            media_type: media_type.to_string(),
            size: 1,
            digest: blake3_digest(b"x"),
            created_by: "activity:a".to_string(),
            schema: None,
            namespace: None,
        };
        let mut manifest: MdafManifest = serde_json::from_value(serde_json::json!({
            "format": "mdaf", "version": 1,
            "markdown": { "path": "text.md", "digest": blake3_digest(b"x"), "media_type": "text/markdown" },
            "producer": { "name": "p", "version": "1" },
            "members": [], "sources": [], "language": "de-CH", "cover": "assets/cover.jpg"
        }))
        .expect("a manifest with language and cover");
        manifest.members = vec![
            member("assets/cover.jpg", MdafMemberRole::Asset, "image/jpeg"),
            member("assets/notes.txt", MdafMemberRole::Asset, "text/plain"),
        ];
        let mut diagnostics = Vec::new();
        validate_document_metadata(&manifest, &mut diagnostics);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        manifest.language = Some("not a tag".to_string());
        manifest.cover = Some("assets/notes.txt".to_string());
        validate_document_metadata(&manifest, &mut diagnostics);
        let codes = diagnostics
            .iter()
            .map(|d| d.code.as_str())
            .collect::<Vec<_>>();
        assert_eq!(codes, ["language_invalid", "cover_invalid"]);

        // Page labels: of known sources, in increasing order, numbered from 1.
        let mut map = MdafSourceMap {
            version: 1,
            document_digest: blake3_digest(b"x"),
            mappings: Vec::new(),
            references: Vec::new(),
            page_labels: BTreeMap::from([(
                "book".to_string(),
                labels(&[
                    (0, MdafPageLabelStyle::RomanLower, None, None),
                    (4, MdafPageLabelStyle::Decimal, None, None),
                ]),
            )]),
        };
        let mut diagnostics = Vec::new();
        validate_page_labels(&map, &BTreeSet::from(["book"]), &mut diagnostics);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        map.page_labels.insert(
            "other".to_string(),
            labels(&[
                (4, MdafPageLabelStyle::Decimal, None, None),
                (2, MdafPageLabelStyle::Decimal, None, Some(0)),
            ]),
        );
        validate_page_labels(&map, &BTreeSet::from(["book"]), &mut diagnostics);
        let codes = diagnostics
            .iter()
            .map(|d| d.code.as_str())
            .collect::<Vec<_>>();
        assert_eq!(codes, ["page_labels_source_unknown", "page_labels_invalid"]);

        // The published schemas take the new fields.
        let mut schema_diagnostics = Vec::new();
        validate_schema(
            "info.json",
            INFO_SCHEMA,
            &serde_json::json!({
                "format": "mdaf", "version": 1,
                "markdown": { "path": "text.md", "digest": blake3_digest(b"x"), "media_type": "text/markdown" },
                "producer": { "name": "p", "version": "1" },
                "members": [], "sources": [], "language": "de-CH", "cover": "assets/cover.jpg"
            }),
            &mut schema_diagnostics,
        );
        validate_schema(
            "source-map.json",
            SOURCE_MAP_SCHEMA,
            &serde_json::json!({
                "version": 1, "document_digest": blake3_digest(b"x"), "mappings": [], "references": [],
                "page_labels": { "book": [ { "start": 0, "style": "roman-lower" }, { "start": 4, "style": "decimal", "prefix": "S-", "first": 1 } ] }
            }),
            &mut schema_diagnostics,
        );
        assert!(schema_diagnostics.is_empty(), "{schema_diagnostics:?}");
    }

    #[test]
    fn the_labeled_example_is_valid_and_labels_its_pages() {
        let example = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("docs/specs/mdaf/v1/examples/labeled");
        let artifact = inspect_mdaf(&example).expect("the example");
        assert!(artifact.valid, "{:?}", artifact.diagnostics);
        let manifest = artifact.manifest.expect("manifest");
        assert_eq!(manifest.language.as_deref(), Some("en"));
        assert_eq!(manifest.cover.as_deref(), Some("assets/cover.svg"));
        let ranges = &artifact.source_map.expect("source map").page_labels["document"];
        let labels = (0..3)
            .map(|page| page_label(ranges, page).expect("a label"))
            .collect::<Vec<_>>();
        assert_eq!(labels, ["Cover", "i", "1"]);
    }
}
