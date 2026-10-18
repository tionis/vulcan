//! Device-key custody: which provider holds this installation's private key,
//! what it can do, and the one way trusted workflows sign with it.
//!
//! See `docs/specs/device-key-custody.md`. Custody is separate from identity
//! (the public key and its `vdev1_` ID), usage, and trust. Only `file_v1` is
//! implemented; a manifest naming another reserved provider keeps its public
//! identity, reports `unsupported`, and never falls back to a key file.
//! Callers never receive private bytes: a provider signs, or, for file
//! custody only, names the exact OpenSSH `IdentityFile` a transport binds.

use crate::device_identity::{
    DeviceIdentityStatus, DeviceIdentityStore, KEY_FILE_MISSING, STORAGE_INACCESSIBLE,
};
use crate::AppError;
use serde::Serialize;
use std::path::PathBuf;

pub use vulcan_secrets::{CapabilitySupport, SecretStoreState as KeyProviderState};

/// The closed set of custody providers a version-1 identity manifest names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum KeyProviderKind {
    /// The protected OpenSSH key file beside the manifest.
    #[serde(rename = "file_v1")]
    File,
    /// A native system secret store (reserved).
    #[serde(rename = "system_secret_v1")]
    SystemSecret,
    /// An SSH agent, for interactive signing only (reserved).
    #[serde(rename = "ssh_agent_v1")]
    SshAgent,
}

impl KeyProviderKind {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "file_v1" => Some(Self::File),
            "system_secret_v1" => Some(Self::SystemSecret),
            "ssh_agent_v1" => Some(Self::SshAgent),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file_v1",
            Self::SystemSecret => "system_secret_v1",
            Self::SshAgent => "ssh_agent_v1",
        }
    }
}

/// What a provider can do, each as supported, unsupported, or unknown.
/// `unattended` means prompting can be suppressed by contract, never that a
/// past attempt happened to succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct KeyCapabilities {
    pub persistent: CapabilitySupport,
    pub device_local: CapabilitySupport,
    pub exportable: CapabilitySupport,
    pub non_exporting_signer: CapabilitySupport,
    pub interactive: CapabilitySupport,
    pub unattended: CapabilitySupport,
    pub ssh_agent: CapabilitySupport,
}

impl KeyCapabilities {
    const UNKNOWN: Self = Self {
        persistent: CapabilitySupport::Unknown,
        device_local: CapabilitySupport::Unknown,
        exportable: CapabilitySupport::Unknown,
        non_exporting_signer: CapabilitySupport::Unknown,
        interactive: CapabilitySupport::Unknown,
        unattended: CapabilitySupport::Unknown,
        ssh_agent: CapabilitySupport::Unknown,
    };
}

/// One exact device key under one custody provider.
pub trait KeyProvider: std::fmt::Debug + Send + Sync {
    fn kind(&self) -> KeyProviderKind;
    /// The canonical `ssh-ed25519` public key this provider must hold.
    fn public_key(&self) -> &str;
    /// Current availability. Never signs, unlocks, or prompts.
    fn state(&self) -> KeyProviderState;
    fn capabilities(&self) -> KeyCapabilities;
    /// An armored SSH signature over `message` in `namespace`.
    fn sign(&self, namespace: &str, message: &[u8]) -> Result<String, AppError>;
    /// The OpenSSH `IdentityFile` a transport may bind, for file custody
    /// only; other providers need their own reviewed transport path.
    fn identity_file(&self) -> Option<PathBuf> {
        None
    }
}

/// `file_v1`: the owner-only OpenSSH key file. Portable and unattended; not
/// encrypted, keychain-backed, or hardware protected.
#[derive(Debug)]
struct FileKeyProvider {
    store: DeviceIdentityStore,
    public_key: String,
}

impl KeyProvider for FileKeyProvider {
    fn kind(&self) -> KeyProviderKind {
        KeyProviderKind::File
    }

    fn public_key(&self) -> &str {
        &self.public_key
    }

    fn state(&self) -> KeyProviderState {
        file_state(&self.store)
    }

    fn capabilities(&self) -> KeyCapabilities {
        KeyCapabilities {
            persistent: CapabilitySupport::Supported,
            device_local: CapabilitySupport::Supported,
            exportable: CapabilitySupport::Supported,
            non_exporting_signer: CapabilitySupport::Unsupported,
            interactive: CapabilitySupport::Unsupported,
            unattended: CapabilitySupport::Supported,
            ssh_agent: CapabilitySupport::Unsupported,
        }
    }

    fn sign(&self, namespace: &str, message: &[u8]) -> Result<String, AppError> {
        self.store.sign(namespace, message)
    }

    fn identity_file(&self) -> Option<PathBuf> {
        self.store.private_key_path().ok()
    }
}

/// A reserved provider this build does not implement.
#[derive(Debug)]
struct UnsupportedKeyProvider {
    kind: KeyProviderKind,
    public_key: String,
}

impl KeyProvider for UnsupportedKeyProvider {
    fn kind(&self) -> KeyProviderKind {
        self.kind
    }

    fn public_key(&self) -> &str {
        &self.public_key
    }

    fn state(&self) -> KeyProviderState {
        KeyProviderState::Unsupported
    }

    fn capabilities(&self) -> KeyCapabilities {
        KeyCapabilities::UNKNOWN
    }

    fn sign(&self, _namespace: &str, _message: &[u8]) -> Result<String, AppError> {
        Err(unsupported(self.kind))
    }
}

fn unsupported(kind: KeyProviderKind) -> AppError {
    AppError::operation_with_code(
        "device_key_provider_unsupported",
        format!(
            "the device key is held by provider `{}`, which this build does not support",
            kind.as_str()
        ),
    )
}

fn file_state(store: &DeviceIdentityStore) -> KeyProviderState {
    let report = store.inspect();
    match report.status {
        DeviceIdentityStatus::Ready => KeyProviderState::Available,
        DeviceIdentityStatus::Uninitialized => KeyProviderState::Missing,
        DeviceIdentityStatus::Invalid => KeyProviderState::Invalid,
        DeviceIdentityStatus::Degraded => match report.diagnostic.as_deref() {
            Some(KEY_FILE_MISSING) => KeyProviderState::Missing,
            Some(STORAGE_INACCESSIBLE) => KeyProviderState::Unavailable,
            _ => KeyProviderState::Unknown,
        },
    }
}

/// The provider holding this installation's device key, chosen by the
/// identity manifest alone. Fails when there is no valid manifest.
pub fn open_device_key(store: &DeviceIdentityStore) -> Result<Box<dyn KeyProvider>, AppError> {
    let provider = store.key_provider()?;
    let kind = KeyProviderKind::parse(&provider)
        .ok_or_else(|| AppError::operation(format!("unknown device key provider `{provider}`")))?;
    let public_key = store.public_key()?;
    Ok(match kind {
        KeyProviderKind::File => Box::new(FileKeyProvider {
            store: store.clone(),
            public_key,
        }),
        KeyProviderKind::SystemSecret | KeyProviderKind::SshAgent => {
            Box::new(UnsupportedKeyProvider { kind, public_key })
        }
    })
}

/// Secret-free custody report for `vulcan device key status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceKeyStatusReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<KeyProviderKind>,
    pub state: KeyProviderState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<KeyCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

/// Reports custody without creating anything, signing, or prompting.
#[must_use]
pub fn device_key_status(store: &DeviceIdentityStore) -> DeviceKeyStatusReport {
    let identity = store.inspect();
    if identity.status == DeviceIdentityStatus::Uninitialized {
        return DeviceKeyStatusReport {
            device_id: None,
            provider: None,
            state: KeyProviderState::Missing,
            capabilities: None,
            diagnostic: Some("no device identity yet; `vulcan device init` creates one".into()),
        };
    }
    match open_device_key(store) {
        Ok(provider) => DeviceKeyStatusReport {
            device_id: identity.device_id,
            provider: Some(provider.kind()),
            state: provider.state(),
            capabilities: Some(provider.capabilities()),
            diagnostic: identity.diagnostic,
        },
        Err(_) => DeviceKeyStatusReport {
            device_id: identity.device_id,
            provider: None,
            state: match identity.status {
                DeviceIdentityStatus::Invalid => KeyProviderState::Invalid,
                _ => KeyProviderState::Unknown,
            },
            capabilities: None,
            diagnostic: identity.diagnostic,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn store() -> (tempfile::TempDir, DeviceIdentityStore) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = DeviceIdentityStore::at(temporary.path().join("device"));
        (temporary, store)
    }

    #[test]
    fn file_custody_reports_its_state_and_signs() {
        let (_temporary, store) = store();
        let status = device_key_status(&store);
        assert_eq!(status.state, KeyProviderState::Missing);
        assert!(status.provider.is_none());
        assert!(
            !store.directory().exists(),
            "status must not create anything"
        );

        store.initialize(false).expect("initialize");
        let status = device_key_status(&store);
        assert_eq!(status.provider, Some(KeyProviderKind::File));
        assert_eq!(status.state, KeyProviderState::Available);
        let capabilities = status.capabilities.expect("capabilities");
        assert_eq!(capabilities.unattended, CapabilitySupport::Supported);
        assert_eq!(capabilities.exportable, CapabilitySupport::Supported);
        assert_eq!(capabilities.interactive, CapabilitySupport::Unsupported);
        let json = serde_json::to_value(&status).expect("status JSON");
        assert_eq!(json["provider"], "file_v1");
        assert_eq!(json["state"], "available");
        assert!(!json.to_string().contains("id_ed25519"));

        let provider = open_device_key(&store).expect("provider");
        assert_eq!(
            provider.public_key(),
            store.public_key().expect("public key")
        );
        let signature = provider.sign("vulcan-test", b"message").expect("signature");
        assert!(signature.starts_with("-----BEGIN SSH SIGNATURE-----"));
        crate::device_identity::verify_signature(
            provider.public_key(),
            "vulcan-test",
            b"message",
            &signature,
        )
        .expect("signature verifies");
        assert!(provider.identity_file().is_some());

        fs::remove_file(store.directory().join("id_ed25519")).expect("remove key");
        assert_eq!(device_key_status(&store).state, KeyProviderState::Missing);
        assert!(provider.sign("vulcan-test", b"message").is_err());
        assert!(provider.identity_file().is_none());
    }

    #[test]
    fn a_reserved_provider_keeps_the_identity_and_never_falls_back_to_the_file() {
        let (_temporary, store) = store();
        store.initialize(false).expect("initialize");
        let manifest_path = store.directory().join("identity.json");
        let manifest = fs::read_to_string(&manifest_path).expect("manifest");
        for (name, kind) in [
            ("system_secret_v1", KeyProviderKind::SystemSecret),
            ("ssh_agent_v1", KeyProviderKind::SshAgent),
        ] {
            fs::write(&manifest_path, manifest.replace("file_v1", name)).expect("manifest");
            let status = device_key_status(&store);
            assert_eq!(status.provider, Some(kind));
            assert_eq!(status.state, KeyProviderState::Unsupported);
            assert_eq!(status.capabilities, Some(KeyCapabilities::UNKNOWN));
            assert!(
                status.device_id.is_some(),
                "the public identity stays usable"
            );
            assert!(store.device_id().expect("device ID").is_some());
            // The key file still exists, but is not used.
            let provider = open_device_key(&store).expect("provider");
            let error = provider
                .sign("vulcan-test", b"message")
                .expect_err("refused");
            assert_eq!(error.code(), Some("device_key_provider_unsupported"));
            assert!(provider.identity_file().is_none());
            assert!(store.sign("vulcan-test", b"message").is_err());
            assert!(store.private_key_path().is_err());
        }
        fs::write(&manifest_path, manifest.replace("file_v1", "auto")).expect("manifest");
        assert!(open_device_key(&store).is_err());
        assert_eq!(device_key_status(&store).state, KeyProviderState::Invalid);
    }
}
