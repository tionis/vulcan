//! Forge-neutral update-channel verification and portable binary replacement.

use crate::AppError;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use flate2::read::GzDecoder;
use fs2::FileExt as _;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::Path;

const CHANNEL_METADATA_LIMIT: usize = 1024 * 1024;
const UPDATE_ARCHIVE_LIMIT: usize = 256 * 1024 * 1024;
const UPDATE_BINARY_LIMIT: u64 = 192 * 1024 * 1024;
const UPDATE_TAR_EXPANDED_LIMIT: u64 = 224 * 1024 * 1024;
const SUPPORTED_UPDATE_TARGETS: &[(&str, &str)] = &[
    ("aarch64-apple-darwin", "tar.gz"),
    ("aarch64-unknown-linux-gnu", "tar.gz"),
    ("x86_64-apple-darwin", "tar.gz"),
    ("x86_64-pc-windows-msvc", "zip"),
    ("x86_64-unknown-linux-gnu", "tar.gz"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedUpdateKey {
    pub key_id: String,
    pub public_key: [u8; 32],
}

pub trait UpdateSource {
    fn fetch(&self, url: &str, limit: usize) -> Result<Vec<u8>, AppError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateChannelEnvelope {
    pub schema_version: u32,
    pub payload: String,
    #[serde(default)]
    pub signatures: Vec<UpdateSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSignature {
    pub algorithm: String,
    pub key_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateChannelPayload {
    pub schema_version: u32,
    pub product: String,
    pub channel: String,
    pub version: String,
    pub source_commit: String,
    pub published_at: String,
    pub prerelease: bool,
    pub artifacts: Vec<UpdateArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateArtifact {
    pub target: String,
    pub kind: String,
    pub format: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub top_level_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckRequest<'a> {
    pub channel_url: &'a str,
    pub expected_channel: &'a str,
    pub current_version: &'a str,
    pub target: &'a str,
    pub require_signature: bool,
    pub trusted_keys: &'a [TrustedUpdateKey],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateCheckReport {
    pub channel: String,
    pub channel_url: String,
    pub current_version: String,
    pub available_version: String,
    pub source_commit: String,
    pub published_at: String,
    pub prerelease: bool,
    pub target: String,
    pub update_available: bool,
    pub signature_verified: bool,
    pub verified_key_id: Option<String>,
    pub artifact: UpdateArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedUpdate {
    pub check: UpdateCheckReport,
    pub binary: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateApplyReport {
    pub action: String,
    pub dry_run: bool,
    pub channel: String,
    pub previous_version: String,
    pub installed_version: String,
    pub executable: String,
    pub signature_verified: bool,
    pub retained_backup: Option<String>,
}

pub fn check_for_update(
    source: &dyn UpdateSource,
    request: &UpdateCheckRequest<'_>,
) -> Result<UpdateCheckReport, AppError> {
    validate_https_url(request.channel_url, "channel")?;
    let envelope_bytes = source.fetch(request.channel_url, CHANNEL_METADATA_LIMIT)?;
    let envelope: UpdateChannelEnvelope =
        serde_json::from_slice(&envelope_bytes).map_err(|error| {
            AppError::operation(format!("invalid update channel envelope: {error}"))
        })?;
    if envelope.schema_version != 1 {
        return Err(AppError::operation(format!(
            "unsupported update channel envelope version {}",
            envelope.schema_version
        )));
    }
    let payload_bytes = BASE64.decode(&envelope.payload).map_err(|error| {
        AppError::operation(format!("invalid update channel payload encoding: {error}"))
    })?;
    if payload_bytes.len() > CHANNEL_METADATA_LIMIT {
        return Err(AppError::operation(
            "update channel payload exceeds the size limit",
        ));
    }
    let verified_key_id =
        verify_signatures(&payload_bytes, &envelope.signatures, request.trusted_keys)?;
    if request.require_signature && verified_key_id.is_none() {
        return Err(AppError::operation(
            "update channel is not signed by a trusted key; refusing the update",
        ));
    }
    let payload: UpdateChannelPayload = serde_json::from_slice(&payload_bytes)
        .map_err(|error| AppError::operation(format!("invalid update channel payload: {error}")))?;
    validate_payload(&payload, request.expected_channel)?;
    let artifact = payload
        .artifacts
        .iter()
        .find(|artifact| artifact.target == request.target && artifact.kind == "archive")
        .cloned()
        .ok_or_else(|| {
            AppError::operation(format!(
                "update channel has no portable archive for target {}",
                request.target
            ))
        })?;
    let current = Version::parse(request.current_version).map_err(|error| {
        AppError::operation(format!(
            "current build version `{}` is not semantic: {error}",
            request.current_version
        ))
    })?;
    let available = Version::parse(&payload.version).map_err(|error| {
        AppError::operation(format!(
            "channel version `{}` is not semantic: {error}",
            payload.version
        ))
    })?;
    Ok(UpdateCheckReport {
        channel: payload.channel,
        channel_url: request.channel_url.to_string(),
        current_version: request.current_version.to_string(),
        available_version: payload.version,
        source_commit: payload.source_commit,
        published_at: payload.published_at,
        prerelease: payload.prerelease,
        target: request.target.to_string(),
        update_available: available > current,
        signature_verified: verified_key_id.is_some(),
        verified_key_id,
        artifact,
    })
}

pub fn prepare_update(
    source: &dyn UpdateSource,
    check: UpdateCheckReport,
    allow_downgrade: bool,
) -> Result<PreparedUpdate, AppError> {
    if !check.update_available && !allow_downgrade {
        return Err(AppError::operation(format!(
            "{} is not newer than installed version {}; use the explicit downgrade override to reinstall or downgrade",
            check.available_version, check.current_version
        )));
    }
    let archive = source.fetch(&check.artifact.url, UPDATE_ARCHIVE_LIMIT)?;
    if archive.len() as u64 != check.artifact.size {
        return Err(AppError::operation(format!(
            "update archive size mismatch: expected {}, received {}",
            check.artifact.size,
            archive.len()
        )));
    }
    let digest = format!("{:x}", Sha256::digest(&archive));
    if digest != check.artifact.sha256 {
        return Err(AppError::operation("update archive SHA-256 mismatch"));
    }
    let binary = extract_binary(&archive, &check.artifact)?;
    Ok(PreparedUpdate { check, binary })
}

pub fn apply_prepared_update(
    prepared: &PreparedUpdate,
    executable: &Path,
    dry_run: bool,
) -> Result<UpdateApplyReport, AppError> {
    if prepared.binary.is_empty() {
        return Err(AppError::operation(
            "refusing to install an empty executable",
        ));
    }
    if !executable.is_file() {
        return Err(AppError::operation(format!(
            "current executable is not a regular file: {}",
            executable.display()
        )));
    }
    let executable = executable.canonicalize().map_err(|error| {
        AppError::operation(format!("cannot resolve current executable: {error}"))
    })?;
    let report = UpdateApplyReport {
        action: "update".to_string(),
        dry_run,
        channel: prepared.check.channel.clone(),
        previous_version: prepared.check.current_version.clone(),
        installed_version: prepared.check.available_version.clone(),
        executable: executable.display().to_string(),
        signature_verified: prepared.check.signature_verified,
        retained_backup: None,
    };
    if dry_run {
        return Ok(report);
    }

    let parent = executable
        .parent()
        .ok_or_else(|| AppError::operation("current executable has no parent directory"))?;
    let file_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::operation("current executable name is not valid UTF-8"))?;
    let temporary = parent.join(format!(
        ".{file_name}.vulcan-update-{}.tmp",
        std::process::id()
    ));
    let backup = parent.join(format!(
        ".{file_name}.vulcan-update-backup-{}",
        ulid::Ulid::new()
    ));
    let lock_path = parent.join(format!(".{file_name}.vulcan-update.lock"));
    let _lock = UpdateLock::acquire(&lock_path)?;
    if temporary.exists() {
        return Err(AppError::operation(
            "a previous update temporary file exists; inspect and remove it before retrying",
        ));
    }

    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| AppError::operation(format!("cannot create update file: {error}")))?;
    let install_result = (|| -> Result<UpdateApplyReport, AppError> {
        output.write_all(&prepared.binary)?;
        output.sync_all()?;
        drop(output);
        copy_executable_permissions(&executable, &temporary)?;
        fs::rename(&executable, &backup).map_err(|error| {
            AppError::operation(format!("cannot move current executable aside: {error}"))
        })?;
        if let Err(error) = fs::rename(&temporary, &executable) {
            let rollback = fs::rename(&backup, &executable);
            let _ = sync_directory(parent);
            return Err(AppError::operation(format!(
                "cannot install downloaded executable: {error}; rollback {}",
                if rollback.is_ok() {
                    "succeeded"
                } else {
                    "failed"
                }
            )));
        }
        sync_directory(parent)?;
        let mut completed = report;
        if fs::remove_file(&backup).is_err() {
            completed.retained_backup = Some(backup.display().to_string());
        }
        Ok(completed)
    })();
    if install_result.is_err() && temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    install_result
}

struct UpdateLock {
    file: fs::File,
}

impl UpdateLock {
    fn acquire(path: &Path) -> Result<Self, AppError> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| {
                AppError::operation(format!(
                    "cannot open the self-update lock at {}: {error}",
                    path.display()
                ))
            })?;
        lock.try_lock_exclusive().map_err(|error| {
            AppError::operation(format!(
                "cannot acquire the self-update lock at {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self { file: lock })
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn validate_payload(
    payload: &UpdateChannelPayload,
    expected_channel: &str,
) -> Result<(), AppError> {
    if payload.schema_version != 1 || payload.product != "vulcan" {
        return Err(AppError::operation(
            "unsupported update channel payload identity",
        ));
    }
    if payload.channel != expected_channel {
        return Err(AppError::operation(format!(
            "update channel mismatch: expected `{expected_channel}`, received `{}`",
            payload.channel
        )));
    }
    if !matches!(payload.channel.as_str(), "stable" | "main") {
        return Err(AppError::operation("unsupported update channel name"));
    }
    let version = Version::parse(&payload.version)
        .map_err(|error| AppError::operation(format!("invalid channel version: {error}")))?;
    let expected_prerelease = expected_channel != "stable";
    if payload.prerelease != expected_prerelease || version.pre.is_empty() == expected_prerelease {
        return Err(AppError::operation(
            "update channel prerelease metadata does not match its channel or version",
        ));
    }
    if payload.source_commit.len() != 40
        || !payload
            .source_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::operation(
            "update source commit must be a 40-character hexadecimal Git object id",
        ));
    }
    let published_at = chrono::DateTime::parse_from_rfc3339(&payload.published_at);
    if !payload.published_at.ends_with('Z')
        || published_at.is_err()
        || published_at
            .as_ref()
            .is_ok_and(|timestamp| timestamp.offset().local_minus_utc() != 0)
    {
        return Err(AppError::operation(
            "update publication time must be a UTC timestamp",
        ));
    }
    let mut targets = BTreeSet::new();
    for artifact in &payload.artifacts {
        if artifact.kind != "archive" || !targets.insert(&artifact.target) {
            return Err(AppError::operation(
                "update channel artifacts must contain one portable archive per target",
            ));
        }
        if !matches!(artifact.format.as_str(), "tar.gz" | "zip") {
            return Err(AppError::operation("unsupported update archive format"));
        }
        let expected_format = SUPPORTED_UPDATE_TARGETS
            .iter()
            .find_map(|(target, format)| (*target == artifact.target.as_str()).then_some(*format));
        if expected_format.is_some_and(|format| artifact.format != format)
            || artifact.top_level_directory
                != format!("vulcan-{}-{}", payload.version, artifact.target)
        {
            return Err(AppError::operation(
                "update artifact format or top-level directory does not match its target",
            ));
        }
        validate_https_url(&artifact.url, "artifact")?;
        if artifact.sha256.len() != 64
            || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || artifact.size == 0
            || artifact.size > UPDATE_ARCHIVE_LIMIT as u64
            || artifact.top_level_directory.is_empty()
            || artifact.top_level_directory.contains(['/', '\\'])
        {
            return Err(AppError::operation(
                "invalid update artifact integrity or layout metadata",
            ));
        }
    }
    Ok(())
}

fn validate_https_url(url: &str, kind: &str) -> Result<(), AppError> {
    if !url.starts_with("https://") || url.contains(['\r', '\n']) {
        return Err(AppError::operation(format!(
            "{kind} URL must use HTTPS without control characters"
        )));
    }
    Ok(())
}

fn verify_signatures(
    payload: &[u8],
    signatures: &[UpdateSignature],
    trusted_keys: &[TrustedUpdateKey],
) -> Result<Option<String>, AppError> {
    let mut matching_signature_failed = false;
    for signature_record in signatures {
        if signature_record.algorithm != "ed25519" {
            continue;
        }
        let Some(trusted) = trusted_keys
            .iter()
            .find(|key| key.key_id == signature_record.key_id)
        else {
            continue;
        };
        let verifying_key = VerifyingKey::from_bytes(&trusted.public_key)
            .map_err(|error| AppError::operation(format!("invalid trusted update key: {error}")))?;
        let signature_bytes = BASE64.decode(&signature_record.signature).ok();
        let signature = signature_bytes
            .as_deref()
            .and_then(|bytes| Signature::from_slice(bytes).ok());
        if signature
            .as_ref()
            .is_some_and(|signature| verifying_key.verify_strict(payload, signature).is_ok())
        {
            return Ok(Some(trusted.key_id.clone()));
        }
        matching_signature_failed = true;
    }
    if matching_signature_failed {
        return Err(AppError::operation(
            "update channel signature verification failed",
        ));
    }
    Ok(None)
}

fn extract_binary(archive: &[u8], artifact: &UpdateArtifact) -> Result<Vec<u8>, AppError> {
    let executable_name = if artifact.target.contains("windows") {
        "vulcan.exe"
    } else {
        "vulcan"
    };
    let expected = format!("{}/{executable_name}", artifact.top_level_directory);
    match artifact.format.as_str() {
        "tar.gz" => extract_tar_binary(archive, &expected),
        "zip" => extract_zip_binary(archive, &expected),
        _ => Err(AppError::operation("unsupported update archive format")),
    }
}

fn extract_tar_binary(archive: &[u8], expected: &str) -> Result<Vec<u8>, AppError> {
    let decoder = GzDecoder::new(Cursor::new(archive)).take(UPDATE_TAR_EXPANDED_LIMIT);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar
        .entries()
        .map_err(|error| AppError::operation(format!("invalid update tar archive: {error}")))?
    {
        let mut entry = entry
            .map_err(|error| AppError::operation(format!("invalid update tar entry: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| AppError::operation(format!("invalid update tar path: {error}")))?;
        if path == Path::new(expected) {
            if !entry.header().entry_type().is_file() {
                return Err(AppError::operation(
                    "update executable archive member is not a regular file",
                ));
            }
            return read_bounded(&mut entry, UPDATE_BINARY_LIMIT);
        }
    }
    Err(AppError::operation(
        "update archive does not contain the expected executable",
    ))
}

fn extract_zip_binary(archive: &[u8], expected: &str) -> Result<Vec<u8>, AppError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|error| AppError::operation(format!("invalid update ZIP archive: {error}")))?;
    let mut entry = zip.by_name(expected).map_err(|_| {
        AppError::operation("update archive does not contain the expected executable")
    })?;
    if entry.is_dir() {
        return Err(AppError::operation(
            "update executable archive member is not a regular file",
        ));
    }
    read_bounded(&mut entry, UPDATE_BINARY_LIMIT)
}

fn read_bounded(reader: &mut impl Read, limit: u64) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(AppError::operation(
            "update executable exceeds the size limit",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn copy_executable_permissions(source: &Path, destination: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(source)?.permissions().mode();
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_executable_permissions(_source: &Path, _destination: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), AppError> {
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(feature = "web")]
#[derive(Debug, Clone)]
pub struct HttpUpdateSource {
    client: reqwest::blocking::Client,
}

#[cfg(feature = "web")]
impl HttpUpdateSource {
    pub fn new() -> Result<Self, AppError> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("vulcan/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many update download redirects")
                } else if attempt.url().scheme() != "https" {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(AppError::operation)?;
        Ok(Self { client })
    }
}

#[cfg(feature = "web")]
impl UpdateSource for HttpUpdateSource {
    fn fetch(&self, url: &str, limit: usize) -> Result<Vec<u8>, AppError> {
        validate_https_url(url, "update download")?;
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|error| AppError::operation(format!("update download failed: {error}")))?;
        if !response.status().is_success() {
            return Err(AppError::operation(format!(
                "update download returned HTTP {}",
                response.status()
            )));
        }
        if response.url().scheme() != "https" {
            return Err(AppError::operation(
                "update download redirected away from HTTPS",
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(AppError::operation(
                "update download exceeds the size limit",
            ));
        }
        let mut bytes = Vec::new();
        response
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(AppError::operation)?;
        if bytes.len() > limit {
            return Err(AppError::operation(
                "update download exceeds the size limit",
            ));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    struct MemorySource(BTreeMap<String, Vec<u8>>);

    impl UpdateSource for MemorySource {
        fn fetch(&self, url: &str, _limit: usize) -> Result<Vec<u8>, AppError> {
            self.0
                .get(url)
                .cloned()
                .ok_or_else(|| AppError::operation("missing fixture URL"))
        }
    }

    fn archive(binary: &[u8]) -> Vec<u8> {
        let output = Vec::new();
        let encoder = GzEncoder::new(output, Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                "vulcan-0.2.0-x86_64-unknown-linux-gnu/vulcan",
                binary,
            )
            .expect("append fixture binary");
        let encoder = builder.into_inner().expect("finish tar");
        encoder.finish().expect("finish gzip")
    }

    fn windows_archive(binary: &[u8]) -> Vec<u8> {
        let output = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(output);
        writer
            .start_file(
                "vulcan-0.2.0-x86_64-pc-windows-msvc/vulcan.exe",
                zip::write::FileOptions::default(),
            )
            .expect("start fixture binary");
        writer.write_all(binary).expect("write fixture binary");
        writer.finish().expect("finish ZIP").into_inner()
    }

    fn fixture(require_signature: bool) -> (MemorySource, UpdateCheckRequest<'static>) {
        let archive = archive(b"new-vulcan");
        let payload = UpdateChannelPayload {
            schema_version: 1,
            product: "vulcan".to_string(),
            channel: "stable".to_string(),
            version: "0.2.0".to_string(),
            source_commit: "a".repeat(40),
            published_at: "2026-08-31T20:00:00Z".to_string(),
            prerelease: false,
            artifacts: SUPPORTED_UPDATE_TARGETS
                .iter()
                .map(|(target, format)| UpdateArtifact {
                    target: (*target).to_string(),
                    kind: "archive".to_string(),
                    format: (*format).to_string(),
                    url: "https://releases.example/vulcan.tar.gz".to_string(),
                    sha256: format!("{:x}", Sha256::digest(&archive)),
                    size: archive.len() as u64,
                    top_level_directory: format!("vulcan-0.2.0-{target}"),
                })
                .collect(),
        };
        let payload = serde_json::to_vec(&payload).expect("serialize payload");
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let envelope = UpdateChannelEnvelope {
            schema_version: 1,
            payload: BASE64.encode(&payload),
            signatures: vec![UpdateSignature {
                algorithm: "ed25519".to_string(),
                key_id: "test-2026".to_string(),
                signature: BASE64.encode(signing_key.sign(&payload).to_bytes()),
            }],
        };
        let channel = serde_json::to_vec(&envelope).expect("serialize envelope");
        let source = MemorySource(BTreeMap::from([
            ("https://channels.example/stable.json".to_string(), channel),
            (
                "https://releases.example/vulcan.tar.gz".to_string(),
                archive,
            ),
        ]));
        let keys = Box::leak(Box::new([TrustedUpdateKey {
            key_id: "test-2026".to_string(),
            public_key: signing_key.verifying_key().to_bytes(),
        }]));
        let request = UpdateCheckRequest {
            channel_url: "https://channels.example/stable.json",
            expected_channel: "stable",
            current_version: "0.1.0",
            target: "x86_64-unknown-linux-gnu",
            require_signature,
            trusted_keys: keys,
        };
        (source, request)
    }

    #[test]
    fn signed_channel_checks_and_extracts_the_exact_archive_binary() {
        let (source, request) = fixture(true);
        let report = check_for_update(&source, &request).expect("check signed update");
        assert!(report.update_available);
        assert!(report.signature_verified);
        assert_eq!(report.verified_key_id.as_deref(), Some("test-2026"));
        let prepared = prepare_update(&source, report, false).expect("prepare update");
        assert_eq!(prepared.binary, b"new-vulcan");
    }

    #[test]
    fn extracts_the_exact_windows_executable() {
        let archive = windows_archive(b"new-vulcan.exe");
        let artifact = UpdateArtifact {
            target: "x86_64-pc-windows-msvc".to_string(),
            kind: "archive".to_string(),
            format: "zip".to_string(),
            url: "https://releases.example/vulcan.zip".to_string(),
            sha256: format!("{:x}", Sha256::digest(&archive)),
            size: archive.len() as u64,
            top_level_directory: "vulcan-0.2.0-x86_64-pc-windows-msvc".to_string(),
        };

        assert_eq!(
            extract_binary(&archive, &artifact).expect("extract Windows binary"),
            b"new-vulcan.exe"
        );
    }

    #[test]
    fn a_valid_rotation_signature_can_follow_an_invalid_matching_record() {
        let signing_key = SigningKey::from_bytes(&[9; 32]);
        let payload = b"exact payload bytes";
        let signatures = [
            UpdateSignature {
                algorithm: "ed25519".to_string(),
                key_id: "rotation".to_string(),
                signature: BASE64.encode([0_u8; 64]),
            },
            UpdateSignature {
                algorithm: "ed25519".to_string(),
                key_id: "rotation".to_string(),
                signature: BASE64.encode(signing_key.sign(payload).to_bytes()),
            },
        ];
        let keys = [TrustedUpdateKey {
            key_id: "rotation".to_string(),
            public_key: signing_key.verifying_key().to_bytes(),
        }];

        assert_eq!(
            verify_signatures(payload, &signatures, &keys).expect("verify rotated signature"),
            Some("rotation".to_string())
        );
    }

    #[test]
    fn payload_validation_rejects_malformed_time_and_prerelease_identity() {
        let (source, _) = fixture(true);
        let envelope: UpdateChannelEnvelope = serde_json::from_slice(
            source
                .0
                .get("https://channels.example/stable.json")
                .expect("channel fixture"),
        )
        .expect("decode envelope");
        let mut payload: UpdateChannelPayload =
            serde_json::from_slice(&BASE64.decode(envelope.payload).expect("decode payload"))
                .expect("parse payload");
        assert!(validate_payload(&payload, "stable").is_ok());

        payload.published_at = "not-a-timeZ".to_string();
        assert!(validate_payload(&payload, "stable").is_err());
        payload.published_at = "2026-08-31T20:00:00Z".to_string();
        payload.prerelease = true;
        assert!(validate_payload(&payload, "stable").is_err());
    }

    #[test]
    fn signature_and_version_fail_closed_without_explicit_overrides() {
        let (mut source, mut request) = fixture(true);
        request.trusted_keys = &[];
        let error = check_for_update(&source, &request).expect_err("unsigned trust must fail");
        assert!(error.to_string().contains("not signed by a trusted key"));

        request.require_signature = false;
        request.current_version = "0.3.0";
        let report = check_for_update(&source, &request).expect("checksum-only check");
        assert!(!report.update_available);
        let error =
            prepare_update(&source, report.clone(), false).expect_err("downgrade must fail");
        assert!(error.to_string().contains("not newer"));
        assert_eq!(
            prepare_update(&source, report, true)
                .expect("explicit downgrade")
                .binary,
            b"new-vulcan"
        );

        source.0.insert(
            "https://releases.example/vulcan.tar.gz".to_string(),
            b"tampered".to_vec(),
        );
        let (_, mut request) = fixture(false);
        request.trusted_keys = &[];
        let report = check_for_update(&source, &request).expect("unsigned metadata check");
        assert!(prepare_update(&source, report, true).is_err());
    }

    #[test]
    fn replacement_is_dry_run_safe_and_atomic_on_success() {
        let (source, request) = fixture(true);
        let check = check_for_update(&source, &request).expect("check update");
        let prepared = prepare_update(&source, check, false).expect("prepare update");
        let temporary = TempDir::new().expect("temporary directory");
        let executable = temporary.path().join("vulcan");
        fs::write(&executable, b"old-vulcan").expect("write old binary");

        let preview = apply_prepared_update(&prepared, &executable, true).expect("preview update");
        assert!(preview.dry_run);
        assert_eq!(
            fs::read(&executable).expect("read old binary"),
            b"old-vulcan"
        );

        let applied = apply_prepared_update(&prepared, &executable, false).expect("apply update");
        assert!(!applied.dry_run);
        assert_eq!(
            fs::read(&executable).expect("read new binary"),
            b"new-vulcan"
        );
        assert!(applied.retained_backup.is_none());
        assert!(temporary.path().join(".vulcan.vulcan-update.lock").exists());
    }

    #[test]
    fn replacement_refuses_a_concurrent_update_lock() {
        let (source, request) = fixture(true);
        let check = check_for_update(&source, &request).expect("check update");
        let prepared = prepare_update(&source, check, false).expect("prepare update");
        let temporary = TempDir::new().expect("temporary directory");
        let executable = temporary.path().join("vulcan");
        fs::write(&executable, b"old-vulcan").expect("write old binary");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(temporary.path().join(".vulcan.vulcan-update.lock"))
            .expect("open update lock");
        lock.lock_exclusive().expect("acquire update lock");

        let error = apply_prepared_update(&prepared, &executable, false)
            .expect_err("concurrent update must fail");
        assert!(error.to_string().contains("self-update lock"));
        assert_eq!(
            fs::read(&executable).expect("read old binary"),
            b"old-vulcan"
        );
    }
}
