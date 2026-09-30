//! Bounded synchronous custody for device-local opaque secrets.
//!
//! This crate contains no vault, OAuth, signing, or authorization semantics.
//! Trusted application workflows choose a concrete provider and logical name;
//! transports and scripts must not expose a raw store to callers.

mod file;
pub use file::{
    inspect_protected_secret_input, read_protected_secret_input, ProtectedFileSecretStore,
};

use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

pub const MAX_SECRET_BYTES: usize = 64 * 1024;

/// Non-serializable, non-cloneable secret memory, cleared when dropped.
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Result<Self, SecretStoreError> {
        Self::from_zeroizing(Zeroizing::new(bytes))
    }

    pub(crate) fn from_zeroizing(bytes: Zeroizing<Vec<u8>>) -> Result<Self, SecretStoreError> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_BYTES {
            return Err(SecretStoreError::Invalid);
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes([REDACTED])")
    }
}

/// A non-secret, bounded logical name, never a path, URI, or shell expression.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SecretName(String);

impl SecretName {
    pub fn parse(name: impl Into<String>) -> Result<Self, SecretStoreError> {
        let name = name.into().to_ascii_lowercase();
        let stem = name.split('.').next().unwrap_or_default();
        let reserved = matches!(stem, "con" | "prn" | "aux" | "nul")
            || (stem.len() == 4
                && (stem.starts_with("com") || stem.starts_with("lpt"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if name.is_empty()
            || name.len() > 128
            || name.starts_with('.')
            || name.ends_with('.')
            || name.contains("..")
            || reserved
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        {
            return Err(SecretStoreError::Invalid);
        }
        Ok(Self(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SecretName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Versioned provider selection. Unknown providers fail closed, never fall back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretProvider {
    #[serde(rename = "file_v1")]
    ProtectedFileV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretReference {
    pub provider: SecretProvider,
    pub name: SecretName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStoreState {
    Available,
    Locked,
    PromptRequired,
    Unavailable,
    Missing,
    Denied,
    Invalid,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SecretStoreCapabilities {
    pub persistent: CapabilitySupport,
    pub device_local: CapabilitySupport,
    pub exportable: CapabilitySupport,
    pub unattended: CapabilitySupport,
}

/// Errors contain neither secret bytes nor raw filesystem/provider locators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStoreError {
    AlreadyExists,
    Locked,
    PromptRequired,
    Missing,
    Denied,
    Invalid,
    Unsupported,
    Unavailable,
    Unknown,
}

impl SecretStoreError {
    #[must_use]
    pub const fn state(self) -> SecretStoreState {
        match self {
            Self::AlreadyExists => SecretStoreState::Available,
            Self::Locked => SecretStoreState::Locked,
            Self::PromptRequired => SecretStoreState::PromptRequired,
            Self::Missing => SecretStoreState::Missing,
            Self::Denied => SecretStoreState::Denied,
            Self::Invalid => SecretStoreState::Invalid,
            Self::Unsupported => SecretStoreState::Unsupported,
            Self::Unavailable => SecretStoreState::Unavailable,
            Self::Unknown => SecretStoreState::Unknown,
        }
    }
}

impl fmt::Display for SecretStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyExists => "secret already exists; replacement refused",
            Self::Locked => "secret custody provider is locked; retry later",
            Self::PromptRequired => "secret custody requires an explicit interactive operation",
            Self::Missing => "secret or store is missing",
            Self::Denied => "secret custody access denied",
            Self::Invalid => "invalid secret name, value, or protected storage",
            Self::Unsupported => "secret custody provider is unsupported",
            Self::Unavailable => "secret custody provider is unavailable",
            Self::Unknown => "secret custody outcome is unknown; inspect before retrying",
        })
    }
}

impl std::error::Error for SecretStoreError {}

/// Explicit low-level custody, not an authority or key-management interface.
/// Inspection must not create files, read secret bytes, or cause a prompt.
pub trait SecretStore: fmt::Debug + Send + Sync {
    fn capabilities(&self) -> SecretStoreCapabilities;
    fn inspect(&self, name: &SecretName) -> SecretStoreState;
    fn create(&self, name: &SecretName, value: &SecretBytes) -> Result<(), SecretStoreError>;
    fn get(&self, name: &SecretName) -> Result<SecretBytes, SecretStoreError>;
    fn delete(&self, name: &SecretName) -> Result<(), SecretStoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_bounded_and_debug_is_redacted() {
        let marker = b"credential-that-must-not-leak".to_vec();
        let value = SecretBytes::new(marker.clone()).unwrap();
        assert_eq!(value.expose(), marker);
        assert_eq!(format!("{value:?}"), "SecretBytes([REDACTED])");
        assert!(SecretBytes::new(Vec::new()).is_err());
        assert!(SecretBytes::new(vec![0; MAX_SECRET_BYTES + 1]).is_err());
        assert!(SecretBytes::new(vec![0; MAX_SECRET_BYTES]).is_ok());
    }

    #[test]
    fn failures_have_structured_states_and_secret_free_messages() {
        for (error, state) in [
            (SecretStoreError::AlreadyExists, SecretStoreState::Available),
            (SecretStoreError::Locked, SecretStoreState::Locked),
            (
                SecretStoreError::PromptRequired,
                SecretStoreState::PromptRequired,
            ),
            (SecretStoreError::Missing, SecretStoreState::Missing),
            (SecretStoreError::Denied, SecretStoreState::Denied),
            (SecretStoreError::Invalid, SecretStoreState::Invalid),
            (SecretStoreError::Unsupported, SecretStoreState::Unsupported),
            (SecretStoreError::Unavailable, SecretStoreState::Unavailable),
            (SecretStoreError::Unknown, SecretStoreState::Unknown),
        ] {
            assert_eq!(error.state(), state);
            assert!(!error.to_string().contains('/'));
            assert!(!error.to_string().is_empty());
            assert!(serde_json::to_value(state).unwrap().is_string());
        }
    }

    #[test]
    fn references_are_versioned_closed_and_reject_untrusted_names() {
        let reference = SecretReference {
            provider: SecretProvider::ProtectedFileV1,
            name: SecretName::parse("mcp-instance-issuer").unwrap(),
        };
        let json = serde_json::to_string(&reference).unwrap();
        assert_eq!(
            serde_json::from_str::<SecretReference>(&json).unwrap(),
            reference
        );
        for json in [
            r#"{"provider":"auto","name":"issuer"}"#,
            r#"{"provider":"file_v2","name":"issuer"}"#,
            r#"{"provider":"file_v1","name":"../issuer"}"#,
            r#"{"provider":"file_v1","name":"issuer","secret":"untrusted"}"#,
        ] {
            assert!(serde_json::from_str::<SecretReference>(json).is_err());
        }
        for name in [
            "", "..", "/secret", "a/b", "a\\b", "a:b", "a\0b", "a..b", "CON", "con.item", "LPT9",
            "COM1", "issuer.",
        ] {
            assert_eq!(SecretName::parse(name), Err(SecretStoreError::Invalid));
        }
        assert!(SecretName::parse("x".repeat(129)).is_err());
        assert_eq!(
            SecretName::parse("MCP-ISSUER").unwrap().as_str(),
            "mcp-issuer"
        );
    }
}
