//! Installation-wide Ed25519 device identity and protected file custody.
//!
//! The identity manifest is public metadata. The private key is never returned
//! by this module's inspection APIs and is loaded only while initializing or
//! verifying the file provider.

use crate::{durable_file, AppError};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use zeroize::Zeroizing;

const IDENTITY_FILE: &str = "identity.json";
const PRIVATE_KEY_FILE: &str = "id_ed25519";
const PUBLIC_KEY_FILE: &str = "id_ed25519.pub";
const INIT_LOCK_FILE: &str = ".identity-init.lock";
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
const SCHEME: &str = "ssh-ed25519-sha256-v1";
const KEY_PROVIDER: &str = "file_v1";

/// State of the local installation's cryptographic device identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceIdentityStatus {
    Uninitialized,
    Ready,
    Degraded,
    Invalid,
}

/// Public, secret-free view of the local device identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceIdentityReport {
    pub status: DeviceIdentityStatus,
    /// Cryptographic device ID, absent until local key initialization.
    pub device_id: Option<String>,
    pub scheme: Option<String>,
    pub fingerprint: Option<String>,
    pub key_provider: Option<String>,
    pub private_key_available: bool,
    pub diagnostic: Option<String>,
}

/// Result from explicit identity initialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceIdentityInitReport {
    pub identity: DeviceIdentityReport,
    pub dry_run: bool,
    pub created: bool,
    pub adopted_interrupted_initialization: bool,
}

/// Result from tightening over-permissive identity storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceIdentityRepairReport {
    pub dry_run: bool,
    /// Logical artifact names whose access was (or would be) restricted; never paths.
    pub repaired: Vec<String>,
    pub identity: DeviceIdentityReport,
}

/// Installation-global storage for the local device identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentityStore {
    directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityManifest {
    version: u32,
    scheme: String,
    device_id: String,
    public_key: String,
    key_provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedPublicIdentity {
    manifest: IdentityManifest,
    fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactState {
    Absent,
    Present,
}

impl DeviceIdentityStore {
    /// Resolve the platform Vulcan data directory without creating anything.
    pub fn user_default() -> Result<Self, AppError> {
        let directory = vulcan_core::vulcan_user_data_dir()
            .ok_or_else(|| AppError::operation("cannot determine the Vulcan user data directory"))?
            .join("device");
        Ok(Self::at(directory))
    }

    /// Construct a store at an explicit identity directory, primarily useful
    /// for tests and platform adapters.
    #[must_use]
    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Return a read-only identity view. This never creates directories/files.
    #[must_use]
    pub fn inspect(&self) -> DeviceIdentityReport {
        match self.inspect_inner() {
            Ok(report) => report,
            Err(error) if error.code() == Some("device_identity_unavailable") => {
                unavailable_report("identity storage is temporarily inaccessible")
            }
            Err(_) => invalid_report("identity files are malformed or unsafe"),
        }
    }

    /// Return the public device ID from the manifest without creating anything or requiring the
    /// private key, which ordinary sync naming and provenance never need.
    pub fn device_id(&self) -> Result<Option<String>, AppError> {
        match fs::symlink_metadata(self.directory.join(IDENTITY_FILE)) {
            Ok(_) => Ok(Some(self.load_manifest()?.manifest.device_id)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(identity_io_error(error)),
        }
    }

    /// Return the device ID, initializing the identity when no identity artifacts exist. This is
    /// the frictionless first-mutation path; an existing manifest is never regenerated.
    pub fn ensure_device_id(&self) -> Result<String, AppError> {
        if let Some(id) = self.device_id()? {
            return Ok(id);
        }
        self.initialize(false)?
            .identity
            .device_id
            .ok_or_else(|| AppError::operation("device identity initialization produced no ID"))
    }

    /// Explicitly initialize the file-backed Ed25519 identity.
    ///
    /// Dry-run is state-free and reports no predicted key ID.
    pub fn initialize(&self, dry_run: bool) -> Result<DeviceIdentityInitReport, AppError> {
        self.initialize_inner(dry_run, None)
    }

    /// Return canonical public-key text for explicit export.
    /// Path of the `file_v1` private key for explicit transport use. Callers
    /// must have checked identity status; the path is never reported or logged.
    pub(crate) fn private_key_path(&self) -> Result<PathBuf, AppError> {
        let path = self.directory.join(PRIVATE_KEY_FILE);
        let identity = self.load_manifest()?;
        self.private_key_matches(&identity)?;
        Ok(path)
    }

    pub fn public_key(&self) -> Result<String, AppError> {
        let validated = self.load_manifest()?;
        Ok(validated.manifest.public_key)
    }

    /// Restrict an existing identity directory and its known files to the current user.
    ///
    /// This only tightens access: it refuses symlinks/reparse points and objects owned by
    /// another account, never creates, replaces, or rewrites key material, and leaves
    /// malformed or mismatched keys for manual repair (the returned report re-inspects them).
    pub fn repair_permissions(
        &self,
        dry_run: bool,
    ) -> Result<DeviceIdentityRepairReport, AppError> {
        let metadata = match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(AppError::operation(
                    "no device identity directory exists; nothing to repair",
                ));
            }
            Err(error) => return Err(identity_io_error(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(AppError::operation(
                "identity directory is not a safe directory",
            ));
        }
        let mut targets = vec![(
            "identity directory".to_string(),
            self.directory.clone(),
            true,
        )];
        for name in [
            IDENTITY_FILE,
            PRIVATE_KEY_FILE,
            PUBLIC_KEY_FILE,
            INIT_LOCK_FILE,
        ] {
            let path = self.directory.join(name);
            if artifact_state(&path)? == ArtifactState::Present {
                targets.push((name.to_string(), path, false));
            }
        }
        let mut repaired = Vec::new();
        for (name, path, directory) in targets {
            if needs_repair(&path, directory)? {
                if !dry_run {
                    repair_private_path(&path, directory)?;
                }
                repaired.push(name);
            }
        }
        Ok(DeviceIdentityRepairReport {
            dry_run,
            repaired,
            identity: self.inspect(),
        })
    }

    fn inspect_inner(&self) -> Result<DeviceIdentityReport, AppError> {
        let directory = match fs::symlink_metadata(&self.directory) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(AppError::operation(
                        "identity directory is not a safe directory",
                    ));
                }
                validate_private_directory(&self.directory, &metadata)?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(identity_io_error(error)),
        };
        if !directory {
            return Ok(uninitialized_report());
        }

        match fs::symlink_metadata(self.directory.join(IDENTITY_FILE)) {
            Ok(_) => {
                let identity = self.load_manifest()?;
                let public_state = artifact_state(&self.directory.join(PUBLIC_KEY_FILE))?;
                let private_state = artifact_state(&self.directory.join(PRIVATE_KEY_FILE))?;
                if public_state == ArtifactState::Absent || private_state == ArtifactState::Absent {
                    return Ok(identity_report(
                        &identity,
                        DeviceIdentityStatus::Degraded,
                        false,
                        Some("public identity is available, but a key file is missing".into()),
                    ));
                }
                match self.verify_keypair_files(&identity) {
                    Ok(()) => Ok(identity_report(
                        &identity,
                        DeviceIdentityStatus::Ready,
                        true,
                        None,
                    )),
                    Err(error) if error.code() == Some("device_identity_unavailable") => {
                        Ok(identity_report(
                            &identity,
                            DeviceIdentityStatus::Degraded,
                            false,
                            Some("identity storage is temporarily inaccessible".into()),
                        ))
                    }
                    Err(_) => Ok(identity_report(
                        &identity,
                        DeviceIdentityStatus::Invalid,
                        false,
                        Some(
                            "stored key material is unsafe or does not match the identity manifest"
                                .into(),
                        ),
                    )),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(identity) = self.inspect_unmanifested_keypair()? {
                    Ok(identity_report(
                        &identity,
                        DeviceIdentityStatus::Degraded,
                        true,
                        Some(
                            "complete keypair is available for safe initialization adoption".into(),
                        ),
                    ))
                } else {
                    let artifacts = self.artifact_states()?;
                    if artifacts == [ArtifactState::Absent, ArtifactState::Absent] {
                        Ok(uninitialized_report())
                    } else {
                        Ok(invalid_report(
                            "incomplete identity files require manual repair; no files were changed",
                        ))
                    }
                }
            }
            Err(error) => Err(AppError::operation(error)),
        }
    }

    fn initialize_inner(
        &self,
        dry_run: bool,
        failpoint: Option<InitializationFailpoint>,
    ) -> Result<DeviceIdentityInitReport, AppError> {
        if dry_run {
            let identity = self.inspect();
            return Ok(DeviceIdentityInitReport {
                identity,
                dry_run: true,
                created: false,
                adopted_interrupted_initialization: false,
            });
        }

        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(AppError::operation(
                    "identity directory is not a safe directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = self.directory.parent().ok_or_else(|| {
                    AppError::operation("identity directory has no parent directory")
                })?;
                fs::create_dir_all(parent).map_err(AppError::operation)?;
                create_private_directory(&self.directory)?;
                sync_directory(parent)?;
            }
            Err(error) => return Err(AppError::operation(error)),
        }
        let directory_metadata =
            fs::symlink_metadata(&self.directory).map_err(AppError::operation)?;
        if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
            return Err(AppError::operation(
                "identity directory is not a safe directory",
            ));
        }
        validate_private_directory(&self.directory, &directory_metadata)?;

        let lock = self.acquire_lock()?;
        let _unlock = UnlockOnDrop(lock);
        self.initialize_locked(failpoint)
    }

    fn initialize_locked(
        &self,
        failpoint: Option<InitializationFailpoint>,
    ) -> Result<DeviceIdentityInitReport, AppError> {
        if self.manifest_state()? == ArtifactState::Present {
            let identity = self.load_manifest()?;
            self.verify_keypair_files(&identity)?;
            return Ok(DeviceIdentityInitReport {
                identity: identity_report(&identity, DeviceIdentityStatus::Ready, true, None),
                dry_run: false,
                created: false,
                adopted_interrupted_initialization: false,
            });
        }

        let artifacts = self.artifact_states()?;
        let adopted = if artifacts == [ArtifactState::Present, ArtifactState::Present] {
            let identity = self.inspect_unmanifested_keypair()?.ok_or_else(|| {
                AppError::operation(
                    "existing identity key files do not match; no files were changed",
                )
            })?;
            Some(identity)
        } else if artifacts == [ArtifactState::Absent, ArtifactState::Absent] {
            None
        } else {
            return Err(AppError::operation(
                "incomplete identity key files require manual repair; no files were changed",
            ));
        };

        if adopted.is_none() {
            let private = PrivateKey::random(&mut ssh_key::rand_core::OsRng, Algorithm::Ed25519)
                .map_err(|_| AppError::operation("could not generate a device identity key"))?;
            let mut public = private.public_key().clone();
            public.set_comment("");
            let public_text = public
                .to_openssh()
                .map_err(|_| AppError::operation("could not encode the device public key"))?;
            let private_text = private
                .to_openssh(LineEnding::LF)
                .map_err(|_| AppError::operation("could not encode the device private key"))?;
            let identity = build_identity(&public_text)?;
            write_new_private(
                &self.directory.join(PRIVATE_KEY_FILE),
                private_text.as_bytes(),
            )?;
            if fail_if_requested(failpoint, InitializationFailpoint::Private) {
                return Err(AppError::operation("simulated initialization interruption"));
            }
            write_new_private(
                &self.directory.join(PUBLIC_KEY_FILE),
                identity.manifest.public_key.as_bytes(),
            )?;
            if fail_if_requested(failpoint, InitializationFailpoint::Public) {
                return Err(AppError::operation("simulated initialization interruption"));
            }
            self.verify_keypair_files(&identity)?;
            self.publish_manifest(&identity)?;
            if fail_if_requested(failpoint, InitializationFailpoint::Manifest) {
                return Err(AppError::operation("simulated initialization interruption"));
            }
            Ok(DeviceIdentityInitReport {
                identity: identity_report(&identity, DeviceIdentityStatus::Ready, true, None),
                dry_run: false,
                created: true,
                adopted_interrupted_initialization: false,
            })
        } else {
            let identity = adopted.expect("checked Some above");
            self.verify_keypair_files(&identity)?;
            self.publish_manifest(&identity)?;
            Ok(DeviceIdentityInitReport {
                identity: identity_report(&identity, DeviceIdentityStatus::Ready, true, None),
                dry_run: false,
                created: false,
                adopted_interrupted_initialization: true,
            })
        }
    }

    fn acquire_lock(&self) -> Result<File, AppError> {
        let path = self.directory.join(INIT_LOCK_FILE);
        reject_unsafe_file(&path, true)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options.open(path).map_err(AppError::operation)?;
        let metadata = file.metadata().map_err(AppError::operation)?;
        validate_regular_file(&metadata)?;
        validate_private_file(&file)?;
        file.lock_exclusive().map_err(AppError::operation)?;
        Ok(file)
    }

    fn manifest_state(&self) -> Result<ArtifactState, AppError> {
        artifact_state(&self.directory.join(IDENTITY_FILE))
    }

    fn publish_manifest(&self, identity: &ValidatedPublicIdentity) -> Result<(), AppError> {
        let bytes = manifest_bytes(&identity.manifest)?;
        match durable_file::create(&self.directory.join(IDENTITY_FILE), &bytes)? {
            durable_file::DurableCreate::Created => Ok(()),
            durable_file::DurableCreate::AlreadyExists => {
                let existing = self.load_manifest()?;
                self.verify_keypair_files(&existing)?;
                if existing.manifest == identity.manifest {
                    Ok(())
                } else {
                    Err(AppError::operation(
                        "another identity manifest was created concurrently; no files were changed",
                    ))
                }
            }
        }
    }

    fn artifact_states(&self) -> Result<[ArtifactState; 2], AppError> {
        Ok([
            artifact_state(&self.directory.join(PRIVATE_KEY_FILE))?,
            artifact_state(&self.directory.join(PUBLIC_KEY_FILE))?,
        ])
    }

    fn inspect_unmanifested_keypair(&self) -> Result<Option<ValidatedPublicIdentity>, AppError> {
        if self.artifact_states()? != [ArtifactState::Present, ArtifactState::Present] {
            return Ok(None);
        }
        let public = self.read_public_key()?;
        let canonical = canonical_public_key(&public)?;
        let identity = build_identity(&canonical)?;
        self.verify_keypair_files(&identity)?;
        Ok(Some(identity))
    }

    fn load_manifest(&self) -> Result<ValidatedPublicIdentity, AppError> {
        let path = self.directory.join(IDENTITY_FILE);
        let file = open_regular_file(&path)?;
        let metadata = file.metadata().map_err(AppError::operation)?;
        validate_regular_file(&metadata)?;
        if metadata.len() > MAX_MANIFEST_BYTES {
            return Err(AppError::operation(
                "identity manifest exceeds its size limit",
            ));
        }
        let capacity = usize::try_from(metadata.len())
            .map_err(|_| AppError::operation("identity manifest exceeds platform limits"))?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(identity_io_error)?;
        if bytes.len() > 16 * 1024 {
            return Err(AppError::operation(
                "identity manifest exceeds its size limit",
            ));
        }
        let manifest: IdentityManifest = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::operation("identity manifest is malformed"))?;
        validate_manifest(manifest)
    }

    fn read_public_key(&self) -> Result<PublicKey, AppError> {
        let path = self.directory.join(PUBLIC_KEY_FILE);
        let file = open_regular_file(&path)?;
        let metadata = file.metadata().map_err(AppError::operation)?;
        validate_regular_file(&metadata)?;
        if metadata.len() > 4096 {
            return Err(AppError::operation(
                "public key file exceeds its size limit",
            ));
        }
        let capacity = usize::try_from(metadata.len())
            .map_err(|_| AppError::operation("public key exceeds platform limits"))?;
        let mut bytes = Vec::with_capacity(capacity);
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(identity_io_error)?;
        if bytes.len() > 4096 {
            return Err(AppError::operation(
                "public key file exceeds its size limit",
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| AppError::operation("device public key is not UTF-8"))?;
        parse_device_public_key(&text)
    }

    fn private_key_matches(&self, identity: &ValidatedPublicIdentity) -> Result<(), AppError> {
        let path = self.directory.join(PRIVATE_KEY_FILE);
        let file = open_regular_file(&path)?;
        let metadata = file.metadata().map_err(AppError::operation)?;
        validate_regular_file(&metadata)?;
        validate_private_file(&file)?;
        let capacity = usize::try_from(metadata.len())
            .map_err(|_| AppError::operation("private key exceeds platform limits"))?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
        file.take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(identity_io_error)?;
        if bytes.len() > 16 * 1024 {
            return Err(AppError::operation(
                "private key file exceeds its size limit",
            ));
        }
        let private = PrivateKey::from_openssh(bytes.as_slice())
            .map_err(|_| AppError::operation("device private key is malformed"))?;
        if private.algorithm() != Algorithm::Ed25519 || private.is_encrypted() {
            return Err(AppError::operation(
                "device private key has an unsupported format",
            ));
        }
        let public_bytes = private
            .public_key()
            .to_bytes()
            .map_err(|_| AppError::operation("device private key public half is invalid"))?;
        let manifest_public = PublicKey::from_openssh(&identity.manifest.public_key)
            .map_err(|_| AppError::operation("identity manifest public key is invalid"))?;
        let expected = manifest_public
            .to_bytes()
            .map_err(|_| AppError::operation("identity manifest public key is invalid"))?;
        if public_bytes != expected {
            return Err(AppError::operation(
                "device private key does not match the public identity",
            ));
        }
        Ok(())
    }

    fn verify_keypair_files(&self, identity: &ValidatedPublicIdentity) -> Result<(), AppError> {
        let public = self.read_public_key()?;
        if canonical_public_key(&public)? != identity.manifest.public_key {
            return Err(AppError::operation(
                "device public key does not match identity",
            ));
        }
        self.private_key_matches(identity)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitializationFailpoint {
    Private,
    Public,
    Manifest,
}

struct UnlockOnDrop(File);

impl Drop for UnlockOnDrop {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn fail_if_requested(
    failpoint: Option<InitializationFailpoint>,
    stage: InitializationFailpoint,
) -> bool {
    #[cfg(test)]
    return failpoint == Some(stage);
    #[cfg(not(test))]
    {
        let _ = (failpoint, stage);
        false
    }
}

fn build_identity(public_key: &str) -> Result<ValidatedPublicIdentity, AppError> {
    let public = parse_device_public_key(public_key)?;
    let canonical = canonical_public_key(&public)?;
    if canonical != public_key {
        return Err(AppError::operation("device public key is not canonical"));
    }
    let public_blob = public
        .to_bytes()
        .map_err(|_| AppError::operation("device public key is malformed"))?;
    let digest = Sha256::digest(public_blob);
    let device_id = format!("vdev1_{}", base32_lower(&digest));
    let manifest = IdentityManifest {
        version: 1,
        scheme: SCHEME.to_string(),
        device_id,
        public_key: canonical,
        key_provider: KEY_PROVIDER.to_string(),
    };
    validate_manifest(manifest)
}

fn validate_manifest(manifest: IdentityManifest) -> Result<ValidatedPublicIdentity, AppError> {
    if manifest.version != 1 || manifest.scheme != SCHEME || manifest.key_provider != KEY_PROVIDER {
        return Err(AppError::operation(
            "identity manifest has an unsupported version or provider",
        ));
    }
    let public = parse_device_public_key(&manifest.public_key)?;
    if canonical_public_key(&public)? != manifest.public_key {
        return Err(AppError::operation("identity public key is not canonical"));
    }
    let public_blob = public
        .to_bytes()
        .map_err(|_| AppError::operation("identity public key is malformed"))?;
    let expected_id = format!("vdev1_{}", base32_lower(&Sha256::digest(public_blob)));
    if manifest.device_id != expected_id {
        return Err(AppError::operation(
            "identity ID does not match its public key",
        ));
    }
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
    Ok(ValidatedPublicIdentity {
        manifest,
        fingerprint,
    })
}

/// Identity facts derived from one public key alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKeyIdentity {
    pub device_id: String,
    /// Canonical `ssh-ed25519 <base64>` text without a comment.
    pub public_key: String,
    pub fingerprint: String,
}

/// Validates an Ed25519 OpenSSH public key and derives its device ID.
///
/// With `lenient` input (a pasted `id_ed25519.pub`), surrounding whitespace and
/// the comment are dropped. Strict input must already be canonical, which is
/// what stored records require.
pub fn identity_from_public_key(text: &str, lenient: bool) -> Result<PublicKeyIdentity, AppError> {
    if text.len() > 4096 {
        return Err(AppError::operation("public key exceeds its size limit"));
    }
    let public = if lenient {
        let public = PublicKey::from_openssh(text.trim())
            .map_err(|_| AppError::operation("public key is malformed"))?;
        if public.algorithm() != Algorithm::Ed25519 || public.key_data().ed25519().is_none() {
            return Err(AppError::operation("public key is not Ed25519"));
        }
        public
    } else {
        parse_device_public_key(text)?
    };
    let canonical = canonical_public_key(&public)?;
    if !lenient && canonical != text {
        return Err(AppError::operation("public key is not canonical"));
    }
    let blob = public
        .to_bytes()
        .map_err(|_| AppError::operation("public key is malformed"))?;
    Ok(PublicKeyIdentity {
        device_id: format!("vdev1_{}", base32_lower(&Sha256::digest(blob))),
        public_key: canonical,
        fingerprint: public.fingerprint(HashAlg::Sha256).to_string(),
    })
}

fn parse_device_public_key(text: &str) -> Result<PublicKey, AppError> {
    if text.len() > 4096 {
        return Err(AppError::operation(
            "device public key exceeds its size limit",
        ));
    }
    let public = PublicKey::from_openssh(text)
        .map_err(|_| AppError::operation("device public key is malformed"))?;
    if public.algorithm() != Algorithm::Ed25519 || public.key_data().ed25519().is_none() {
        return Err(AppError::operation("device public key is not Ed25519"));
    }
    if public.comment().is_empty() && text.trim() == text {
        Ok(public)
    } else {
        Err(AppError::operation(
            "device public key must be canonical and have no comment",
        ))
    }
}

fn canonical_public_key(public: &PublicKey) -> Result<String, AppError> {
    let mut canonical = public.clone();
    canonical.set_comment("");
    canonical
        .to_openssh()
        .map_err(|_| AppError::operation("could not encode the device public key"))
}

fn manifest_bytes(manifest: &IdentityManifest) -> Result<Vec<u8>, AppError> {
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|_| AppError::operation("could not encode the device identity manifest"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn identity_report(
    identity: &ValidatedPublicIdentity,
    status: DeviceIdentityStatus,
    private_key_available: bool,
    diagnostic: Option<String>,
) -> DeviceIdentityReport {
    DeviceIdentityReport {
        status,
        device_id: Some(identity.manifest.device_id.clone()),
        scheme: Some(identity.manifest.scheme.clone()),
        fingerprint: Some(identity.fingerprint.clone()),
        key_provider: Some(identity.manifest.key_provider.clone()),
        private_key_available,
        diagnostic,
    }
}

fn uninitialized_report() -> DeviceIdentityReport {
    DeviceIdentityReport {
        status: DeviceIdentityStatus::Uninitialized,
        device_id: None,
        scheme: None,
        fingerprint: None,
        key_provider: None,
        private_key_available: false,
        diagnostic: None,
    }
}

fn invalid_report(diagnostic: &str) -> DeviceIdentityReport {
    DeviceIdentityReport {
        status: DeviceIdentityStatus::Invalid,
        device_id: None,
        scheme: None,
        fingerprint: None,
        key_provider: None,
        private_key_available: false,
        diagnostic: Some(diagnostic.to_string()),
    }
}

fn unavailable_report(diagnostic: &str) -> DeviceIdentityReport {
    DeviceIdentityReport {
        status: DeviceIdentityStatus::Degraded,
        device_id: None,
        scheme: None,
        fingerprint: None,
        key_provider: Some(KEY_PROVIDER.to_string()),
        private_key_available: false,
        diagnostic: Some(diagnostic.to_string()),
    }
}

#[cfg(unix)]
fn needs_repair(path: &Path, _directory: bool) -> Result<bool, AppError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path).map_err(identity_io_error)?;
    Ok(metadata.permissions().mode() & 0o077 != 0)
}

#[cfg(windows)]
fn needs_repair(path: &Path, _directory: bool) -> Result<bool, AppError> {
    match crate::windows_acl::verify_private_path(path) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(true),
        Err(error) => Err(identity_io_error(error)),
    }
}

#[cfg(not(any(unix, windows)))]
fn needs_repair(_path: &Path, _directory: bool) -> Result<bool, AppError> {
    Err(AppError::operation(
        "permission repair is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn repair_private_path(path: &Path, directory: bool) -> Result<(), AppError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(identity_io_error)?;
    let metadata = handle.metadata().map_err(AppError::operation)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(AppError::operation(
            "identity storage is owned by another account; no permissions were changed",
        ));
    }
    if directory != metadata.is_dir() {
        return Err(AppError::operation(
            "identity artifact has an unexpected type",
        ));
    }
    let mode = if directory { 0o700 } else { 0o600 };
    handle
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(AppError::operation)
}

#[cfg(windows)]
fn repair_private_path(path: &Path, directory: bool) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path).map_err(identity_io_error)?;
    if directory {
        validate_reparse_free(&metadata)?;
    } else {
        validate_regular_file(&metadata)?;
    }
    crate::windows_acl::repair_private_path(path, directory).map_err(acl_error)
}

#[cfg(windows)]
fn validate_reparse_free(metadata: &fs::Metadata) -> Result<(), AppError> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    if metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(AppError::operation("identity directory is a reparse point"));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn repair_private_path(_path: &Path, _directory: bool) -> Result<(), AppError> {
    Err(AppError::operation(
        "permission repair is unavailable on this platform",
    ))
}

fn artifact_state(path: &Path) -> Result<ArtifactState, AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_regular_file(&metadata)?;
            Ok(ArtifactState::Present)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ArtifactState::Absent),
        Err(error) => Err(identity_io_error(error)),
    }
}

fn reject_unsafe_file(path: &Path, allow_missing: bool) -> Result<(), AppError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_regular_file(&metadata),
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::operation(error)),
    }
}

fn validate_regular_file(metadata: &fs::Metadata) -> Result<(), AppError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::operation(
            "identity artifact is not a regular file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::operation("identity artifact is a reparse point"));
        }
    }
    Ok(())
}

fn validate_private_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AppError::operation(
                "device identity directory is accessible by group or other users",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::operation("identity directory is a reparse point"));
        }
        crate::windows_acl::verify_private_path(path).map_err(acl_error)?;
    }
    #[cfg(not(windows))]
    let _ = path;
    #[cfg(not(any(unix, windows)))]
    let _ = metadata;
    Ok(())
}

#[cfg(unix)]
fn validate_private_file(file: &File) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = file.metadata().map_err(AppError::operation)?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(AppError::operation(
            "device private key is accessible by group or other users",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_file(file: &File) -> Result<(), AppError> {
    crate::windows_acl::verify_private_file(file).map_err(acl_error)
}

#[cfg(not(any(unix, windows)))]
fn validate_private_file(_file: &File) -> Result<(), AppError> {
    Err(AppError::operation(
        "private key access verification is unavailable on this platform",
    ))
}

/// Keep ACL diagnostics free of paths and account names.
#[cfg(windows)]
fn acl_error(error: std::io::Error) -> AppError {
    if error.kind() == std::io::ErrorKind::PermissionDenied && error.raw_os_error().is_none() {
        AppError::operation(format!("device identity storage {error}"))
    } else {
        identity_io_error(error)
    }
}

fn create_private_directory(path: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(AppError::operation(error)),
        }
    }
    #[cfg(windows)]
    {
        match crate::windows_acl::create_private_directory(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(AppError::operation(error)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        match fs::create_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(AppError::operation(error)),
        }
    }
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::operation("identity key path has no parent directory"))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::operation)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(AppError::operation)?;
    }
    temporary.write_all(bytes).map_err(AppError::operation)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(AppError::operation)?;
    let metadata = temporary
        .as_file()
        .metadata()
        .map_err(AppError::operation)?;
    validate_private_file(temporary.as_file()).and_then(|()| validate_regular_file(&metadata))?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| AppError::operation(error.error))?;
    sync_directory(parent)
}

fn open_regular_file(path: &Path) -> Result<File, AppError> {
    let metadata = fs::symlink_metadata(path).map_err(identity_io_error)?;
    validate_regular_file(&metadata)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(identity_io_error)?;
    let opened_metadata = file.metadata().map_err(identity_io_error)?;
    validate_regular_file(&opened_metadata)?;
    Ok(file)
}

fn identity_io_error(error: std::io::Error) -> AppError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    ) {
        AppError::operation_with_code(
            "device_identity_unavailable",
            "identity storage is temporarily inaccessible",
        )
    } else {
        AppError::operation(error)
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), AppError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(AppError::operation)
}

/// NTFS cannot open a directory for a portable flush; files are already synced and moved
/// atomically, matching `durable_file`.
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep one fallible cross-platform durability contract.
fn sync_directory(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut output = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut accumulator = 0_u16;
    let mut bits = 0_u8;
    for byte in bytes {
        accumulator = (accumulator << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            output.push(char::from(
                ALPHABET[((accumulator >> bits) & 0x1f) as usize],
            ));
        }
    }
    if bits != 0 {
        output.push(char::from(
            ALPHABET[((accumulator << (5 - bits)) & 0x1f) as usize],
        ));
    }
    output
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::json;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::tempdir;

    fn set_private_mode(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private mode");
        }
    }

    fn generate_private_text() -> String {
        PrivateKey::random(&mut ssh_key::rand_core::OsRng, Algorithm::Ed25519)
            .expect("key generation")
            .to_openssh(LineEnding::LF)
            .expect("private key encoding")
            .to_string()
    }

    #[test]
    fn inspect_and_dry_run_are_state_free() {
        let temporary = tempdir().expect("tempdir");
        let directory = temporary.path().join("missing/device");
        let store = DeviceIdentityStore::at(&directory);

        assert_eq!(store.inspect().status, DeviceIdentityStatus::Uninitialized);
        let report = store.initialize(true).expect("dry-run");
        assert!(report.dry_run);
        assert!(!report.created);
        assert_eq!(report.identity.device_id, None);
        assert!(!directory.exists());
    }

    #[test]
    fn repair_tightens_loose_permissions_without_touching_key_material() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("vulcan/device"));
        let id = store
            .initialize(false)
            .expect("initialize")
            .identity
            .device_id
            .expect("device ID");
        let private = store.directory.join(PRIVATE_KEY_FILE);
        let before = fs::read(&private).expect("private key bytes");
        fs::set_permissions(&private, fs::Permissions::from_mode(0o644)).expect("loosen key");
        fs::set_permissions(&store.directory, fs::Permissions::from_mode(0o755))
            .expect("loosen directory");
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Invalid);

        let preview = store.repair_permissions(true).expect("preview");
        assert_eq!(preview.repaired, ["identity directory", PRIVATE_KEY_FILE]);
        assert_eq!(
            fs::metadata(&private).expect("key").permissions().mode() & 0o777,
            0o644,
            "dry-run changes nothing"
        );

        let repaired = store.repair_permissions(false).expect("repair");
        assert_eq!(repaired.repaired, preview.repaired);
        assert_eq!(repaired.identity.status, DeviceIdentityStatus::Ready);
        assert_eq!(repaired.identity.device_id.as_deref(), Some(id.as_str()));
        assert_eq!(fs::read(&private).expect("private key bytes"), before);
        assert!(store
            .repair_permissions(false)
            .expect("idempotent")
            .repaired
            .is_empty());
    }

    #[test]
    fn repair_refuses_missing_directories_and_symlinks() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("device"));
        assert!(store.repair_permissions(false).is_err());
        let target = temporary.path().join("elsewhere");
        fs::create_dir(&target).expect("target");
        std::os::unix::fs::symlink(&target, temporary.path().join("device")).expect("symlink");
        assert!(store.repair_permissions(false).is_err());
    }

    #[test]
    fn initialization_derives_full_id_from_canonical_openssh_key() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("vulcan/device"));
        let report = store.initialize(false).expect("initialize");
        let id = report.identity.device_id.expect("device ID");
        let public_text = store.public_key().expect("public key");
        let public = PublicKey::from_openssh(&public_text).expect("SSH public parser");
        assert_eq!(public.algorithm(), Algorithm::Ed25519);
        assert_eq!(public.comment(), "");
        assert!(id.starts_with("vdev1_"));
        assert_eq!(id.len(), 58);
        assert!(report.identity.fingerprint.unwrap().starts_with("SHA256:"));
        assert_eq!(report.identity.status, DeviceIdentityStatus::Ready);
        assert_eq!(store.inspect().device_id.as_deref(), Some(id.as_str()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&store.directory)
                    .expect("identity directory")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(store.directory.join(PRIVATE_KEY_FILE))
                    .expect("private key")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let private_bytes =
            Zeroizing::new(fs::read(store.directory.join(PRIVATE_KEY_FILE)).expect("private file"));
        let private =
            PrivateKey::from_openssh(private_bytes.as_slice()).expect("private key parses");
        assert_eq!(
            private
                .public_key()
                .to_bytes()
                .expect("private public blob"),
            public.to_bytes().expect("public blob")
        );
    }

    #[test]
    fn device_id_is_read_only_and_ensure_initializes_once() {
        let temporary = tempdir().expect("tempdir");
        let directory = temporary.path().join("device");
        let store = DeviceIdentityStore::at(&directory);
        assert_eq!(store.device_id().expect("absent"), None);
        assert!(!directory.exists());

        let id = store.ensure_device_id().expect("ensure");
        assert!(id.starts_with("vdev1_"));
        assert_eq!(store.device_id().expect("present"), Some(id.clone()));
        assert_eq!(store.ensure_device_id().expect("stable"), id);

        // The public manifest alone is enough for naming, even without the private key.
        fs::remove_file(directory.join(PRIVATE_KEY_FILE)).expect("remove private key");
        assert_eq!(store.ensure_device_id().expect("degraded"), id);
    }

    #[test]
    fn ssh_blob_id_vector_and_noncanonical_inputs_are_checked() {
        let public_text =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";
        let identity = build_identity(public_text).expect("known SSH key");
        assert_eq!(
            identity.manifest.device_id,
            "vdev1_kasselv6z6hm64aukjgayhelqhg43lwxktpy4dubim4oobspocca"
        );

        assert!(parse_device_public_key(
            "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti"
        )
        .is_err());
        assert!(parse_device_public_key(&format!("{public_text} user@host")).is_err());
        assert!(parse_device_public_key(&format!("{public_text} ")).is_err());
        assert!(parse_device_public_key("ssh-ed25519 !!!").is_err());

        let mut blob = PublicKey::from_openssh(public_text)
            .expect("fixture key")
            .to_bytes()
            .expect("wire blob");
        blob.extend_from_slice(&[0, 0, 0, 1, 0xff]);
        let trailing = format!(
            "ssh-ed25519 {}",
            base64::engine::general_purpose::STANDARD.encode(blob)
        );
        assert!(parse_device_public_key(&trailing).is_err());
    }

    #[test]
    fn concurrent_initialization_is_serialized_and_no_clobber() {
        let temporary = tempdir().expect("tempdir");
        let store = Arc::new(DeviceIdentityStore::at(temporary.path().join("device")));
        let barrier = Arc::new(Barrier::new(5));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store.initialize(false).expect("racing initialization")
                })
            })
            .collect();
        barrier.wait();
        let reports: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("thread"))
            .collect();
        let id = reports[0].identity.device_id.as_deref().expect("ID");
        assert!(reports.iter().all(|report| {
            report.identity.device_id.as_deref() == Some(id)
                && report.identity.status == DeviceIdentityStatus::Ready
        }));
        assert!(reports.iter().filter(|report| report.created).count() == 1);
        let before = fs::read(store.directory.join(PRIVATE_KEY_FILE)).expect("key before");
        let repeated = store.initialize(false).expect("repeated init");
        let after = fs::read(store.directory.join(PRIVATE_KEY_FILE)).expect("key after");
        assert!(!repeated.created);
        assert_eq!(before, after);
    }

    #[test]
    fn complete_pair_after_interruption_is_adopted_but_partial_pair_is_not() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("adopt"));
        let interrupted = store.initialize_inner(false, Some(InitializationFailpoint::Public));
        assert!(interrupted.is_err());
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Degraded);
        let adopted = store.initialize(false).expect("adopt complete pair");
        assert!(adopted.adopted_interrupted_initialization);
        assert_eq!(adopted.identity.status, DeviceIdentityStatus::Ready);

        let partial = DeviceIdentityStore::at(temporary.path().join("partial"));
        assert!(partial
            .initialize_inner(false, Some(InitializationFailpoint::Private))
            .is_err());
        assert_eq!(partial.inspect().status, DeviceIdentityStatus::Invalid);
        assert!(partial.initialize(false).is_err());
        assert!(!partial.directory.join(IDENTITY_FILE).exists());
    }

    #[test]
    fn unsafe_permissions_symlinks_and_mismatched_keys_fail_closed() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("unsafe"));
        store.initialize(false).expect("initialize");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                store.directory.join(PRIVATE_KEY_FILE),
                fs::Permissions::from_mode(0o644),
            )
            .expect("make key readable by others");
            assert_eq!(store.inspect().status, DeviceIdentityStatus::Invalid);
            assert!(store.initialize(false).is_err());
            set_private_mode(&store.directory.join(PRIVATE_KEY_FILE));
        }

        let mismatched_private = generate_private_text();
        fs::write(store.directory.join(PRIVATE_KEY_FILE), mismatched_private)
            .expect("replace with unrelated key");
        set_private_mode(&store.directory.join(PRIVATE_KEY_FILE));
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Invalid);
        assert!(store.initialize(false).is_err());

        #[cfg(unix)]
        {
            let symlink_store = DeviceIdentityStore::at(temporary.path().join("symlink"));
            fs::create_dir(&symlink_store.directory).expect("directory");
            std::os::unix::fs::symlink(
                store.directory.join(IDENTITY_FILE),
                symlink_store.directory.join(IDENTITY_FILE),
            )
            .expect("manifest symlink");
            assert_eq!(
                symlink_store.inspect().status,
                DeviceIdentityStatus::Invalid
            );
            assert!(symlink_store.initialize(false).is_err());
        }
    }

    #[test]
    fn missing_public_file_is_degraded_and_corrupt_public_file_is_invalid() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("public"));
        store.initialize(false).expect("initialize");
        fs::remove_file(store.directory.join(PUBLIC_KEY_FILE)).expect("remove public key");
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Degraded);
        let report = store.initialize(false).expect_err("no silent repair");
        assert!(!report.to_string().contains("OPENSSH PRIVATE KEY"));

        store
            .initialize_inner(false, None)
            .expect_err("still missing public");
        fs::write(
            store.directory.join(PUBLIC_KEY_FILE),
            "ssh-ed25519 invalid\n",
        )
        .expect("corrupt public file");
        set_private_mode(&store.directory.join(PUBLIC_KEY_FILE));
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Invalid);

        let unrelated = PrivateKey::random(&mut ssh_key::rand_core::OsRng, Algorithm::Ed25519)
            .expect("unrelated key");
        let unrelated_public = canonical_public_key(unrelated.public_key()).expect("public text");
        fs::write(store.directory.join(PUBLIC_KEY_FILE), unrelated_public)
            .expect("mismatched public file");
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Invalid);
    }

    #[test]
    fn malformed_private_bytes_are_redacted_from_reports_and_errors() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("redacted"));
        store.initialize(false).expect("initialize");
        let marker = "PRIVATE-KEY-SECRET-MARKER";
        fs::write(store.directory.join(PRIVATE_KEY_FILE), marker).expect("replace private file");
        set_private_mode(&store.directory.join(PRIVATE_KEY_FILE));
        let report = store.inspect();
        assert_eq!(report.status, DeviceIdentityStatus::Invalid);
        assert!(!format!("{report:?}").contains(marker));
        assert!(!serde_json::to_string(&report)
            .expect("report JSON")
            .contains(marker));
        let error = store.initialize(false).expect_err("invalid key");
        assert!(!error.to_string().contains(marker));
    }

    #[test]
    fn identity_manifest_is_closed_and_never_contains_private_material() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("manifest"));
        store.initialize(false).expect("initialize");
        let text = fs::read_to_string(store.directory.join(IDENTITY_FILE)).expect("manifest");
        let value: serde_json::Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(
            value,
            json!({
                "version": 1,
                "scheme": SCHEME,
                "device_id": store.inspect().device_id,
                "public_key": store.public_key().expect("public key"),
                "key_provider": KEY_PROVIDER,
            })
        );
        assert!(!text.contains("OPENSSH PRIVATE KEY"));
    }

    #[test]
    fn manifest_collision_is_verified_before_success_is_reported() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("collision"));
        let initialized = store.initialize(false).expect("initialize");
        let other_public = PrivateKey::random(&mut ssh_key::rand_core::OsRng, Algorithm::Ed25519)
            .expect("other key")
            .public_key()
            .clone();
        let other = build_identity(&canonical_public_key(&other_public).expect("public text"))
            .expect("other identity");
        assert!(store.publish_manifest(&other).is_err());
        assert_eq!(store.inspect().device_id, initialized.identity.device_id);
    }

    #[test]
    fn staged_replacement_leaves_the_active_identity_until_activated() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("device"));
        let old = store.ensure_device_id().expect("old identity");

        let dry = store.stage_replacement(true).expect("dry run");
        assert!(dry.dry_run && !dry.reused);
        assert_eq!(dry.staged.status, DeviceIdentityStatus::Uninitialized);

        let staged = store.stage_replacement(false).expect("stage");
        let new = staged.staged.device_id.clone().expect("staged id");
        assert_ne!(new, old);
        assert_eq!(staged.old_device_id.as_deref(), Some(old.as_str()));
        assert_eq!(store.device_id().expect("active"), Some(old.clone()));

        let again = store.stage_replacement(false).expect("restage");
        assert!(again.reused);
        assert_eq!(again.staged.device_id.as_deref(), Some(new.as_str()));

        let activated = store.activate_staged().expect("activate");
        assert_eq!(activated.old_device_id.as_deref(), Some(old.as_str()));
        assert_eq!(activated.new_device_id, new);
        assert!(activated.archived);
        assert_eq!(store.device_id().expect("active"), Some(new));
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Ready);
        let retired = DeviceIdentityStore::at(store.retired_directory(&old).expect("retired"));
        assert_eq!(retired.device_id().expect("retired id"), Some(old));
        assert!(store.activate_staged().is_err(), "nothing left to activate");
    }

    #[test]
    fn interrupted_activation_is_finished_by_running_it_again() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("device"));
        let old = store.ensure_device_id().expect("old identity");
        let new = store
            .stage_replacement(false)
            .expect("stage")
            .staged
            .device_id
            .expect("id");
        // Simulate dying between the two renames.
        let retired = store.retired_directory(&old).expect("retired");
        fs::create_dir_all(retired.parent().expect("parent")).expect("retired root");
        fs::rename(temporary.path().join("device"), retired).expect("first rename");
        assert_eq!(store.inspect().status, DeviceIdentityStatus::Uninitialized);

        let report = store.activate_staged().expect("finish");
        assert!(report.completed_interrupted_activation && !report.archived);
        assert_eq!(store.device_id().expect("active"), Some(new));
    }

    #[test]
    fn activation_refuses_without_a_ready_staged_identity() {
        let temporary = tempdir().expect("tempdir");
        let store = DeviceIdentityStore::at(temporary.path().join("device"));
        store.ensure_device_id().expect("identity");
        assert!(store.activate_staged().is_err());
    }
}
/// A replacement identity staged beside the active one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagedReplacementReport {
    pub dry_run: bool,
    /// The identity currently active, absent when there is none (or it is unreadable).
    pub old_device_id: Option<String>,
    /// The staged identity; absent on a dry run that would generate one.
    pub staged: DeviceIdentityReport,
    /// A staged identity already existed and was kept.
    pub reused: bool,
}

/// Result of making the staged identity the active one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivationReport {
    pub old_device_id: Option<String>,
    pub new_device_id: String,
    /// The old identity was archived (inactive, never used as a fallback).
    pub archived: bool,
    /// The staged identity was already moved into place by an interrupted run.
    pub completed_interrupted_activation: bool,
}

const STAGED_DIRECTORY: &str = "device-staged";
const RETIRED_DIRECTORY: &str = "device-retired";

impl DeviceIdentityStore {
    fn sibling(&self, name: &str) -> Result<PathBuf, AppError> {
        self.directory
            .parent()
            .map(|parent| parent.join(name))
            .ok_or_else(|| AppError::operation("identity directory has no parent directory"))
    }

    /// The store holding a staged replacement identity.
    pub fn staged(&self) -> Result<Self, AppError> {
        Ok(Self::at(self.sibling(STAGED_DIRECTORY)?))
    }

    /// Where an archived identity for `device_id` lives once it is retired.
    pub fn retired_directory(&self, device_id: &str) -> Result<PathBuf, AppError> {
        Ok(self.sibling(RETIRED_DIRECTORY)?.join(device_id))
    }

    /// Generates a replacement identity in a separate directory, leaving the
    /// active identity untouched. Re-running keeps the staged one.
    pub fn stage_replacement(&self, dry_run: bool) -> Result<StagedReplacementReport, AppError> {
        let staged = self.staged()?;
        let old_device_id = self.device_id().ok().flatten();
        let existing = staged.inspect();
        let reused = existing.status != DeviceIdentityStatus::Uninitialized;
        if reused && existing.status != DeviceIdentityStatus::Ready {
            return Err(AppError::operation(
                "a staged replacement identity exists but is not usable; remove the device-staged directory and retry",
            ));
        }
        if dry_run || reused {
            return Ok(StagedReplacementReport {
                dry_run,
                old_device_id,
                staged: existing,
                reused,
            });
        }
        let report = staged.initialize(false)?;
        Ok(StagedReplacementReport {
            dry_run,
            old_device_id,
            staged: report.identity,
            reused: false,
        })
    }

    /// Makes the staged identity the active one, archiving the old identity
    /// under `device-retired/<old-id>`. Two atomic renames; an interruption
    /// between them leaves no active identity (everything fails closed), and
    /// running this again finishes the swap.
    pub fn activate_staged(&self) -> Result<ActivationReport, AppError> {
        let staged = self.staged()?;
        let staged_report = staged.inspect();
        let staged_directory = staged.directory.clone();
        if staged_report.status != DeviceIdentityStatus::Ready {
            return Err(AppError::operation(
                "there is no usable staged identity; run `vulcan device replace` first",
            ));
        }
        let new_device_id = staged_report
            .device_id
            .ok_or_else(|| AppError::operation("the staged identity has no device ID"))?;
        let active_present = match fs::symlink_metadata(&self.directory) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(identity_io_error(error)),
        };
        let old_device_id = if active_present {
            self.device_id()?
        } else {
            None
        };
        if old_device_id.as_deref() == Some(new_device_id.as_str()) {
            return Err(AppError::operation(
                "the staged identity is already the active one",
            ));
        }
        let mut archived = false;
        if active_present {
            let id = old_device_id.as_deref().ok_or_else(|| {
                AppError::operation(
                    "the active identity has no readable manifest; repair it before replacing",
                )
            })?;
            let retired = self.retired_directory(id)?;
            if fs::symlink_metadata(&retired).is_ok() {
                return Err(AppError::operation(
                    "this identity was already retired; remove the old archive to continue",
                ));
            }
            let retired_root = self.sibling(RETIRED_DIRECTORY)?;
            create_private_directory(&retired_root)?;
            fs::rename(&self.directory, &retired).map_err(AppError::operation)?;
            sync_directory(&retired_root)?;
            archived = true;
        }
        fs::rename(&staged_directory, &self.directory).map_err(AppError::operation)?;
        if let Some(parent) = self.directory.parent() {
            sync_directory(parent)?;
        }
        Ok(ActivationReport {
            old_device_id,
            new_device_id,
            archived,
            completed_interrupted_activation: !active_present,
        })
    }
}
