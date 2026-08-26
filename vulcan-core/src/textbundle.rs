//! Safe reader for `TextBundle` directories and compressed `TextPack` archives.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;
use zip::ZipArchive;

const MAX_FILES: usize = 100_000;
const MAX_MEMBER_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_CONTROL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextBundleRepresentation {
    Directory,
    Zip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBundleInfo {
    pub version: u32,
    #[serde(default, rename = "type")]
    pub text_type: Option<String>,
    #[serde(default)]
    pub transient: bool,
    #[serde(default, rename = "creatorURL")]
    pub creator_url: Option<String>,
    #[serde(default, rename = "creatorIdentifier")]
    pub creator_identifier: Option<String>,
    #[serde(default, rename = "sourceURL")]
    pub source_url: Option<String>,
    #[serde(flatten)]
    pub extensions: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextBundleMember {
    pub path: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextBundleDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextBundleDiagnostic {
    pub severity: TextBundleDiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextBundle {
    #[serde(skip)]
    pub package_path: PathBuf,
    pub representation: TextBundleRepresentation,
    pub identity: String,
    pub valid: bool,
    pub info: Option<TextBundleInfo>,
    pub text_path: Option<String>,
    pub text: Option<String>,
    pub assets: Vec<TextBundleMember>,
    pub members: Vec<TextBundleMember>,
    pub diagnostics: Vec<TextBundleDiagnostic>,
}

impl TextBundle {
    pub fn copy_member_to(
        &self,
        path: &str,
        writer: &mut impl Write,
    ) -> Result<(), TextBundleError> {
        let expected = self
            .members
            .iter()
            .find(|member| member.path == path)
            .ok_or_else(|| TextBundleError::InvalidMember(format!("undeclared member: {path}")))?;
        let (size, digest) = copy_member(&self.package_path, self.representation, path, writer)?;
        if size != expected.size || digest != expected.digest {
            return Err(TextBundleError::InvalidMember(format!(
                "member changed since inspection: {path}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum TextBundleError {
    Io(io::Error),
    Zip(zip::result::ZipError),
    InvalidContainer(String),
    InvalidMember(String),
}

impl Display for TextBundleError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "TextBundle I/O error: {error}"),
            Self::Zip(error) => write!(formatter, "TextPack ZIP error: {error}"),
            Self::InvalidContainer(message) => write!(formatter, "invalid TextBundle: {message}"),
            Self::InvalidMember(message) => {
                write!(formatter, "invalid TextBundle member: {message}")
            }
        }
    }
}

impl std::error::Error for TextBundleError {}

impl From<io::Error> for TextBundleError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<zip::result::ZipError> for TextBundleError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error)
    }
}

#[allow(clippy::too_many_lines)]
pub fn inspect_text_bundle(path: &Path) -> Result<TextBundle, TextBundleError> {
    let representation = if path.is_dir() {
        TextBundleRepresentation::Directory
    } else {
        TextBundleRepresentation::Zip
    };
    let observed = observe_members(path, representation)?;
    let identity = logical_identity(&observed);
    let mut diagnostics = Vec::new();

    let info = match read_member(path, representation, "info.json", MAX_CONTROL_BYTES) {
        Ok(bytes) => match serde_json::from_slice::<TextBundleInfo>(&bytes) {
            Ok(info) => Some(info),
            Err(error) => {
                diagnostic(
                    &mut diagnostics,
                    "info_invalid",
                    error.to_string(),
                    "info.json",
                );
                None
            }
        },
        Err(error) => {
            diagnostic(
                &mut diagnostics,
                "info_missing",
                error.to_string(),
                "info.json",
            );
            None
        }
    };
    if let Some(info) = info.as_ref() {
        if !(1..=2).contains(&info.version) {
            diagnostic(
                &mut diagnostics,
                "version_unsupported",
                format!("TextBundle version {} is unsupported", info.version),
                "info.json",
            );
        }
    }

    let text_paths = observed
        .keys()
        .filter(|member| member.starts_with("text.") && !member.contains('/'))
        .cloned()
        .collect::<Vec<_>>();
    if text_paths.len() != 1 {
        diagnostic(
            &mut diagnostics,
            "text_member_count",
            format!(
                "expected exactly one root text.* member, found {}",
                text_paths.len()
            ),
            "info.json",
        );
    }
    let text_path = (text_paths.len() == 1).then(|| text_paths[0].clone());
    let text = text_path.as_deref().and_then(|member| {
        match read_member(path, representation, member, MAX_TEXT_BYTES) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Some(text),
                Err(error) => {
                    diagnostic(&mut diagnostics, "text_not_utf8", error.to_string(), member);
                    None
                }
            },
            Err(error) => {
                diagnostic(
                    &mut diagnostics,
                    "text_unreadable",
                    error.to_string(),
                    member,
                );
                None
            }
        }
    });

    for member in observed.keys() {
        if member != "info.json"
            && text_path.as_deref() != Some(member)
            && !member.starts_with("assets/")
        {
            diagnostic(
                &mut diagnostics,
                "member_unexpected",
                "members must be info.json, one root text.*, or files below assets/",
                member,
            );
        }
    }
    let assets = observed
        .values()
        .filter(|member| member.path.starts_with("assets/"))
        .cloned()
        .collect::<Vec<_>>();
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.code.cmp(&right.code))
    });
    let valid = !diagnostics
        .iter()
        .any(|item| item.severity == TextBundleDiagnosticSeverity::Error);

    Ok(TextBundle {
        package_path: path.to_path_buf(),
        representation,
        identity,
        valid,
        info,
        text_path,
        text,
        assets,
        members: observed.into_values().collect(),
        diagnostics,
    })
}

pub(crate) fn observe_members(
    path: &Path,
    representation: TextBundleRepresentation,
) -> Result<BTreeMap<String, TextBundleMember>, TextBundleError> {
    match representation {
        TextBundleRepresentation::Directory => observe_directory(path),
        TextBundleRepresentation::Zip => observe_zip(path),
    }
}

fn observe_directory(root: &Path) -> Result<BTreeMap<String, TextBundleMember>, TextBundleError> {
    let mut pending = vec![root.to_path_buf()];
    let mut members = BTreeMap::new();
    let mut total = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(TextBundleError::InvalidContainer(format!(
                    "symbolic links are forbidden: {}",
                    entry.path().display()
                )));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                let entry_path = entry.path();
                let relative = entry_path.strip_prefix(root).map_err(|_| {
                    TextBundleError::InvalidContainer("member escaped package root".to_string())
                })?;
                let member_path = normalized_member_path(relative)?;
                let size = entry.metadata()?.len();
                enforce_limits(members.len() + 1, size, &mut total)?;
                let digest = hash_reader(File::open(entry_path)?, size)?;
                insert_member(&mut members, member_path, size, digest)?;
            } else {
                return Err(TextBundleError::InvalidContainer(format!(
                    "non-regular member is forbidden: {}",
                    entry.path().display()
                )));
            }
        }
    }
    Ok(members)
}

fn observe_zip(path: &Path) -> Result<BTreeMap<String, TextBundleMember>, TextBundleError> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    if archive.len() > MAX_FILES {
        return Err(TextBundleError::InvalidContainer(
            "too many ZIP entries".to_string(),
        ));
    }
    let mut members = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let enclosed = file.enclosed_name().ok_or_else(|| {
            TextBundleError::InvalidContainer(format!("unsafe ZIP path: {}", file.name()))
        })?;
        if file.is_dir() {
            continue;
        }
        if let Some(mode) = file.unix_mode() {
            let kind = mode & 0o170_000;
            if kind != 0 && kind != 0o100_000 {
                return Err(TextBundleError::InvalidContainer(format!(
                    "non-regular ZIP member is forbidden: {}",
                    file.name()
                )));
            }
        }
        let member_path = normalized_member_path(enclosed)?;
        let size = file.size();
        enforce_limits(members.len() + 1, size, &mut total)?;
        if size > 0
            && (file.compressed_size() == 0
                || size / file.compressed_size().max(1) > MAX_COMPRESSION_RATIO)
        {
            return Err(TextBundleError::InvalidContainer(format!(
                "ZIP member exceeds compression-ratio limit: {member_path}"
            )));
        }
        let digest = hash_reader(&mut file, size)?;
        insert_member(&mut members, member_path, size, digest)?;
    }
    Ok(members)
}

fn normalized_member_path(path: &Path) -> Result<String, TextBundleError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    TextBundleError::InvalidContainer("member path is not UTF-8".to_string())
                })?;
                if value.is_empty() || value.contains('\\') || value.chars().any(char::is_control) {
                    return Err(TextBundleError::InvalidContainer(
                        "invalid member path".to_string(),
                    ));
                }
                if value.nfc().collect::<String>() != value {
                    return Err(TextBundleError::InvalidContainer(
                        "member path is not NFC-normalized".to_string(),
                    ));
                }
                parts.push(value);
            }
            _ => {
                return Err(TextBundleError::InvalidContainer(
                    "member path must be normalized and relative".to_string(),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(TextBundleError::InvalidContainer(
            "empty member path".to_string(),
        ));
    }
    Ok(parts.join("/"))
}

fn insert_member(
    members: &mut BTreeMap<String, TextBundleMember>,
    path: String,
    size: u64,
    digest: String,
) -> Result<(), TextBundleError> {
    let folded = path.to_lowercase();
    if members
        .keys()
        .any(|existing| existing.to_lowercase() == folded)
    {
        return Err(TextBundleError::InvalidContainer(format!(
            "duplicate or case-fold-colliding member: {path}"
        )));
    }
    members.insert(path.clone(), TextBundleMember { path, size, digest });
    Ok(())
}

fn enforce_limits(count: usize, size: u64, total: &mut u64) -> Result<(), TextBundleError> {
    if count > MAX_FILES || size > MAX_MEMBER_BYTES {
        return Err(TextBundleError::InvalidContainer(
            "package limit exceeded".to_string(),
        ));
    }
    *total = total
        .checked_add(size)
        .ok_or_else(|| TextBundleError::InvalidContainer("expanded size overflow".to_string()))?;
    if *total > MAX_TOTAL_BYTES {
        return Err(TextBundleError::InvalidContainer(
            "expanded size limit exceeded".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn read_member(
    package: &Path,
    representation: TextBundleRepresentation,
    member: &str,
    limit: u64,
) -> Result<Vec<u8>, TextBundleError> {
    let mut bytes = Vec::new();
    match representation {
        TextBundleRepresentation::Directory => {
            let relative = Path::new(member);
            normalized_member_path(relative)?;
            let root = fs::canonicalize(package)?;
            let path = package.join(relative);
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(TextBundleError::InvalidMember(member.to_string()));
            }
            let canonical = fs::canonicalize(path)?;
            if !canonical.starts_with(root) {
                return Err(TextBundleError::InvalidMember(member.to_string()));
            }
            File::open(canonical)?
                .take(limit + 1)
                .read_to_end(&mut bytes)?;
        }
        TextBundleRepresentation::Zip => {
            let mut archive = ZipArchive::new(File::open(package)?)?;
            archive
                .by_name(member)?
                .take(limit + 1)
                .read_to_end(&mut bytes)?;
        }
    }
    if bytes.len() as u64 > limit {
        return Err(TextBundleError::InvalidMember(format!(
            "member exceeds read limit: {member}"
        )));
    }
    Ok(bytes)
}

pub(crate) fn copy_member(
    package: &Path,
    representation: TextBundleRepresentation,
    member: &str,
    writer: &mut impl Write,
) -> Result<(u64, String), TextBundleError> {
    let mut hasher = blake3::Hasher::new();
    let mut seen = 0_u64;
    let mut copy = |reader: &mut dyn Read| -> Result<(), TextBundleError> {
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            seen += count as u64;
            if seen > MAX_MEMBER_BYTES {
                return Err(TextBundleError::InvalidMember(
                    "member grew beyond limit".to_string(),
                ));
            }
            hasher.update(&buffer[..count]);
            writer.write_all(&buffer[..count])?;
        }
        Ok(())
    };
    match representation {
        TextBundleRepresentation::Directory => {
            let mut file = open_directory_member(package, member)?;
            copy(&mut file)?;
        }
        TextBundleRepresentation::Zip => {
            let mut archive = ZipArchive::new(File::open(package)?)?;
            let mut file = archive.by_name(member)?;
            copy(&mut file)?;
        }
    }
    Ok((seen, format!("blake3:{}", hasher.finalize())))
}

fn open_directory_member(root: &Path, member: &str) -> Result<File, TextBundleError> {
    let relative = Path::new(member);
    normalized_member_path(relative)?;
    let canonical_root = fs::canonicalize(root)?;
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TextBundleError::InvalidMember(member.to_string()));
    }
    let canonical_path = fs::canonicalize(path)?;
    if !canonical_path.starts_with(canonical_root) {
        return Err(TextBundleError::InvalidMember(member.to_string()));
    }
    Ok(File::open(canonical_path)?)
}

fn hash_reader(mut reader: impl Read, expected_size: u64) -> Result<String, TextBundleError> {
    let mut hasher = blake3::Hasher::new();
    let mut seen = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        seen += count as u64;
        hasher.update(&buffer[..count]);
    }
    if seen != expected_size {
        return Err(TextBundleError::InvalidContainer(
            "member size changed while reading".to_string(),
        ));
    }
    Ok(format!("blake3:{}", hasher.finalize()))
}

fn logical_identity(members: &BTreeMap<String, TextBundleMember>) -> String {
    let mut hasher = blake3::Hasher::new();
    for member in members.values() {
        let path = serde_json::to_string(&member.path).expect("path serializes");
        hasher.update(
            format!(
                "{{\"path\":{path},\"size\":{},\"digest\":\"{}\"}}\n",
                member.size, member.digest
            )
            .as_bytes(),
        );
    }
    format!("blake3:{}", hasher.finalize())
}

fn diagnostic(
    diagnostics: &mut Vec<TextBundleDiagnostic>,
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
) {
    diagnostics.push(TextBundleDiagnostic {
        severity: TextBundleDiagnosticSeverity::Error,
        code: code.into(),
        message: message.into(),
        path: Some(path.into()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use zip::write::FileOptions;

    fn write_fixture(root: &Path) {
        fs::create_dir_all(root.join("assets")).expect("assets");
        fs::write(root.join("text.md"), "# Note\n\n![](assets/map.png)\n").expect("text");
        fs::write(root.join("assets/map.png"), b"synthetic").expect("asset");
        fs::write(
            root.join("info.json"),
            r#"{"version":2,"type":"net.daringfireball.markdown","org.example":{"version":1,"theme":"blue"}}"#,
        )
        .expect("info");
    }

    #[test]
    fn directory_and_textpack_match_and_preserve_extension_metadata() {
        let temp = tempdir().expect("temp");
        let root = temp.path().join("note.textbundle");
        write_fixture(&root);
        let zip_path = temp.path().join("note.textpack");
        let file = File::create(&zip_path).expect("ZIP");
        let mut zip = zip::ZipWriter::new(file);
        for member in ["info.json", "text.md", "assets/map.png"] {
            zip.start_file(member, FileOptions::default())
                .expect("member");
            zip.write_all(&fs::read(root.join(member)).expect("read"))
                .expect("write");
        }
        zip.finish().expect("finish");

        let directory = inspect_text_bundle(&root).expect("directory");
        let archive = inspect_text_bundle(&zip_path).expect("archive");
        assert!(directory.valid, "{:?}", directory.diagnostics);
        assert_eq!(directory.identity, archive.identity);
        assert_eq!(directory.info, archive.info);
        assert_eq!(
            directory.info.expect("info").extensions["org.example"]["theme"],
            "blue"
        );
        let mut copied = Vec::new();
        archive
            .copy_member_to("assets/map.png", &mut copied)
            .expect("copy");
        assert_eq!(copied, b"synthetic");
    }

    #[test]
    fn rejects_unsafe_or_nonstandard_members() {
        let temp = tempdir().expect("temp");
        write_fixture(temp.path());
        fs::write(temp.path().join("extra.bin"), b"unexpected").expect("extra");
        let inspected = inspect_text_bundle(temp.path()).expect("inspect");
        assert!(!inspected.valid);
        assert!(inspected
            .diagnostics
            .iter()
            .any(|item| item.code == "member_unexpected"));

        let zip_path = temp.path().join("unsafe.textpack");
        let file = File::create(&zip_path).expect("ZIP");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("../outside", FileOptions::default())
            .expect("member");
        zip.write_all(b"bad").expect("write");
        zip.finish().expect("finish");
        assert!(inspect_text_bundle(&zip_path).is_err());
    }
}
