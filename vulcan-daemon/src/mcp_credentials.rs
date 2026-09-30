//! Typed device-local credential custody for named MCP instances.
//! No client request may choose the provider, logical names, or legacy sources.

use crate::mcp_oauth_clients::{
    OAuthClientRegistry, OAuthClientRegistryError, OAuthClientSecretCustody,
    OAuthClientSecretMigration,
};
use crate::mcp_remote::McpRemoteDefinition;
use base64::prelude::{Engine, BASE64_URL_SAFE_NO_PAD};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use ulid::Ulid;
use vulcan_secrets::{
    inspect_protected_secret_input, read_protected_secret_input, ProtectedFileSecretStore,
    SecretBytes, SecretName, SecretProvider, SecretReference, SecretStore, SecretStoreError,
    SecretStoreState,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpRemoteCredentialReferences {
    pub issuer: SecretReference,
    pub signing: SecretReference,
    pub client_namespace: SecretName,
}

impl McpRemoteCredentialReferences {
    #[must_use]
    pub fn for_instance(instance_id: Ulid) -> Self {
        let namespace = format!("mcp-{instance_id}");
        let reference = |kind| SecretReference {
            provider: SecretProvider::ProtectedFileV1,
            name: SecretName::parse(format!("{namespace}.{kind}"))
                .expect("bounded instance credential name"),
        };
        Self {
            issuer: reference("issuer"),
            signing: reference("signing"),
            client_namespace: SecretName::parse(namespace).expect("bounded instance namespace"),
        }
    }
}

#[derive(Clone)]
pub struct McpRemoteCredentials {
    store: Arc<dyn SecretStore>,
    references: McpRemoteCredentialReferences,
    legacy_directory: PathBuf,
}

impl std::fmt::Debug for McpRemoteCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpRemoteCredentials")
            .field("references", &self.references)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct McpIssuerCredentialMigration {
    pub reference: SecretReference,
    pub source_state: SecretStoreState,
    pub destination_state: SecretStoreState,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpCredentialMigrationReport {
    pub dry_run: bool,
    pub issuer_credentials: Vec<McpIssuerCredentialMigration>,
    pub clients: OAuthClientSecretMigration,
}

#[derive(Debug)]
pub enum McpCredentialError {
    Custody(SecretStoreError),
    Clients(OAuthClientRegistryError),
    MigrationRequired,
    Invalid,
    DifferentCredential,
    RandomUnavailable,
}

impl std::fmt::Display for McpCredentialError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Custody(error) => write!(formatter, "MCP credential custody: {error}"),
            Self::Clients(error) => std::fmt::Display::fmt(error, formatter),
            Self::MigrationRequired => formatter.write_str("named MCP credentials require explicit migration: preview `vulcan mcp remote migrate-credentials <name> --dry-run`, then apply without --dry-run while the instance is stopped"),
            Self::Invalid => formatter.write_str("MCP credential is invalid; no replacement was generated"),
            Self::DifferentCredential => formatter.write_str("existing MCP credential differs from its migration source; replacement refused"),
            Self::RandomUnavailable => formatter.write_str("MCP credential randomness is unavailable"),
        }
    }
}
impl std::error::Error for McpCredentialError {}
impl From<SecretStoreError> for McpCredentialError {
    fn from(error: SecretStoreError) -> Self {
        Self::Custody(error)
    }
}
impl From<OAuthClientRegistryError> for McpCredentialError {
    fn from(error: OAuthClientRegistryError) -> Self {
        Self::Clients(error)
    }
}

impl McpRemoteCredentials {
    #[must_use]
    pub fn at(state_root: &Path, remote: &McpRemoteDefinition) -> Self {
        Self {
            store: Arc::new(ProtectedFileSecretStore::at(state_root.join("secrets"))),
            references: McpRemoteCredentialReferences::for_instance(remote.instance_id),
            legacy_directory: state_root.join("mcp-remotes").join(remote.id.as_str()),
        }
    }

    pub fn client_custody(&self) -> Result<OAuthClientSecretCustody, McpCredentialError> {
        Ok(OAuthClientSecretCustody::new(
            Arc::clone(&self.store),
            self.references.client_namespace.clone(),
        )?)
    }

    fn sources(&self) -> [(&SecretReference, PathBuf); 2] {
        [
            (
                &self.references.issuer,
                self.legacy_directory.join("oauth-issuer-secret"),
            ),
            (
                &self.references.signing,
                self.legacy_directory.join("oauth-signing-key"),
            ),
        ]
    }

    /// Fail before creating any new issuer credential when a legacy source
    /// requires migration. Existing references are authoritative, never files.
    fn preflight(&self) -> Result<(), McpCredentialError> {
        for (reference, source) in self.sources() {
            match self.store.inspect(&reference.name) {
                SecretStoreState::Available => {}
                SecretStoreState::Missing => match inspect_protected_secret_input(&source) {
                    SecretStoreState::Missing => {}
                    SecretStoreState::Available => {
                        return Err(McpCredentialError::MigrationRequired)
                    }
                    _ => return Err(McpCredentialError::Invalid),
                },
                _ => {
                    self.store.get(&reference.name)?;
                }
            }
        }
        Ok(())
    }

    pub fn issuer_secret(&self) -> Result<String, McpCredentialError> {
        self.load_or_create(&self.references.issuer)
    }
    pub fn signing_key(&self) -> Result<String, McpCredentialError> {
        self.load_or_create(&self.references.signing)
    }

    fn load_or_create(&self, reference: &SecretReference) -> Result<String, McpCredentialError> {
        self.preflight()?;
        match self.store.get(&reference.name) {
            Ok(value) => secret_text(&value),
            Err(SecretStoreError::Missing) => {
                let mut random = [0_u8; 32];
                getrandom::fill(&mut random).map_err(|_| McpCredentialError::RandomUnavailable)?;
                let value = SecretBytes::new(BASE64_URL_SAFE_NO_PAD.encode(random).into_bytes())?;
                match self.store.create(&reference.name, &value) {
                    Ok(()) => secret_text(&value),
                    Err(SecretStoreError::AlreadyExists) => {
                        secret_text(&self.store.get(&reference.name)?)
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The trusted management caller must hold the remote's runtime lock on
    /// apply. Source keys remain protected inactive recovery material.
    pub fn migrate(
        &self,
        dry_run: bool,
    ) -> Result<McpCredentialMigrationReport, McpCredentialError> {
        let custody = self.client_custody()?;
        let path = self.legacy_directory.join("oauth-clients.json");
        let mut report = McpCredentialMigrationReport {
            dry_run,
            issuer_credentials: self
                .sources()
                .iter()
                .map(|(reference, source)| McpIssuerCredentialMigration {
                    reference: (*reference).clone(),
                    source_state: inspect_protected_secret_input(source),
                    destination_state: self.store.inspect(&reference.name),
                })
                .collect(),
            clients: OAuthClientRegistry::migrate_secrets(&path, &custody, true)?,
        };
        if dry_run {
            return Ok(report);
        }
        // Validate both source values before publishing either one. Retained
        // sources and immutable destination names make interrupted copies resumable.
        let planned = self
            .sources()
            .iter()
            .map(|(reference, source)| {
                let value = match read_protected_secret_input(source) {
                    Ok(value) => Some(SecretBytes::new(secret_text(&value)?.into_bytes())?),
                    Err(SecretStoreError::Missing) => None,
                    Err(error) => return Err(McpCredentialError::Custody(error)),
                };
                if let Some(value) = value.as_ref() {
                    match self.store.get(&reference.name) {
                        Ok(existing) => check_same_secret(&existing, value)?,
                        Err(SecretStoreError::Missing) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(((*reference).clone(), value))
            })
            .collect::<Result<Vec<_>, McpCredentialError>>()?;
        for (reference, value) in planned {
            if let Some(value) = value {
                match self.store.create(&reference.name, &value) {
                    Ok(()) | Err(SecretStoreError::AlreadyExists) => {
                        check_same_secret(&self.store.get(&reference.name)?, &value)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        report.clients = OAuthClientRegistry::migrate_secrets(&path, &custody, false)?;
        for item in &mut report.issuer_credentials {
            item.destination_state = self.store.inspect(&item.reference.name);
        }
        Ok(report)
    }
}

fn secret_text(value: &SecretBytes) -> Result<String, McpCredentialError> {
    let text = std::str::from_utf8(value.expose())
        .map_err(|_| McpCredentialError::Invalid)?
        .trim();
    if text.is_empty() {
        return Err(McpCredentialError::Invalid);
    }
    Ok(text.to_owned())
}

fn check_same_secret(
    existing: &SecretBytes,
    expected: &SecretBytes,
) -> Result<(), McpCredentialError> {
    if bool::from(existing.expose().ct_eq(expected.expose())) {
        Ok(())
    } else {
        Err(McpCredentialError::DifferentCredential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_remote::{
        AddMcpRemoteRequest, McpRemoteAuthentication, McpRemoteId, McpRemoteVault,
    };
    use crate::registry::WikiId;
    use std::fs;

    fn remote() -> McpRemoteDefinition {
        AddMcpRemoteRequest {
            id: McpRemoteId::parse("personal").unwrap(),
            bind: "127.0.0.1:8765".into(),
            public_url: "https://mcp.example.test/personal".into(),
            authentication: McpRemoteAuthentication::IndieAuth {
                identity: "https://identity.example.test/me".into(),
            },
            vaults: vec![McpRemoteVault {
                wiki_id: WikiId::parse("personal").unwrap(),
                ceiling_profile: "readonly".into(),
                default_profile: "readonly".into(),
                tool_packs: vec!["notes-read".into()],
            }],
        }
        .into_definition()
        .unwrap()
    }

    fn legacy_files(credentials: &McpRemoteCredentials, issuer: &[u8], signing: &[u8]) {
        fs::create_dir_all(&credentials.legacy_directory).unwrap();
        #[cfg(windows)]
        vulcan_app::windows_acl::repair_private_path(&credentials.legacy_directory, true).unwrap();
        for ((_, path), value) in credentials.sources().into_iter().zip([issuer, signing]) {
            fs::write(&path, value).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }

    #[test]
    fn new_credentials_are_device_local_instance_bound_and_restart_stable() {
        let temporary = tempfile::tempdir().unwrap();
        let definition = remote();
        let credentials = McpRemoteCredentials::at(temporary.path(), &definition);
        let preview = credentials.migrate(true).unwrap();
        assert!(preview
            .issuer_credentials
            .iter()
            .all(|item| item.destination_state == SecretStoreState::Missing));
        assert!(!temporary.path().join("secrets").exists());
        assert!(!temporary.path().join("mcp-remotes").exists());
        let issuer = credentials.issuer_secret().unwrap();
        let signing = credentials.signing_key().unwrap();
        assert_ne!(issuer, signing);
        assert_eq!(issuer.len(), 43);
        let restarted = McpRemoteCredentials::at(temporary.path(), &definition);
        assert_eq!(restarted.issuer_secret().unwrap(), issuer);
        assert_eq!(restarted.signing_key().unwrap(), signing);
        let other = McpRemoteCredentials::at(temporary.path(), &remote());
        assert_ne!(other.references, credentials.references);
        assert_ne!(other.signing_key().unwrap(), signing);
        for rendered in [
            format!("{credentials:?}"),
            serde_json::to_string(&credentials.migrate(true).unwrap()).unwrap(),
        ] {
            assert!(!rendered.contains(&issuer));
            assert!(!rendered.contains(&signing));
            assert!(!rendered.contains(&temporary.path().display().to_string()));
        }
    }

    #[test]
    fn legacy_keys_require_explicit_migration_and_partial_copies_resume_without_rotation() {
        let temporary = tempfile::tempdir().unwrap();
        let definition = remote();
        let credentials = McpRemoteCredentials::at(temporary.path(), &definition);
        legacy_files(
            &credentials,
            b"legacy issuer marker\n",
            b"legacy signing marker\n",
        );
        assert!(matches!(
            credentials.issuer_secret(),
            Err(McpCredentialError::MigrationRequired)
        ));
        assert!(matches!(
            credentials.signing_key(),
            Err(McpCredentialError::MigrationRequired)
        ));
        let preview = credentials.migrate(true).unwrap();
        assert!(preview.dry_run);
        assert!(!temporary.path().join("secrets").exists());
        // State after interruption immediately following the first immutable copy.
        credentials
            .store
            .create(
                &credentials.references.issuer.name,
                &SecretBytes::new(b"legacy issuer marker".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            credentials.issuer_secret(),
            Err(McpCredentialError::MigrationRequired)
        ));
        let restarted = McpRemoteCredentials::at(temporary.path(), &definition);
        let applied = restarted.migrate(false).unwrap();
        assert!(applied
            .issuer_credentials
            .iter()
            .all(|item| item.destination_state == SecretStoreState::Available));
        assert_eq!(restarted.issuer_secret().unwrap(), "legacy issuer marker");
        assert_eq!(restarted.signing_key().unwrap(), "legacy signing marker");
        assert_eq!(
            fs::read(credentials.sources()[1].1.clone()).unwrap(),
            b"legacy signing marker\n"
        );
        restarted.migrate(false).unwrap();
        assert!(!serde_json::to_string(&applied)
            .unwrap()
            .contains("legacy signing marker"));
    }

    #[test]
    fn invalid_or_different_keys_never_generate_replacements() {
        let temporary = tempfile::tempdir().unwrap();
        let credentials = McpRemoteCredentials::at(temporary.path(), &remote());
        legacy_files(&credentials, b"issuer", b"\xff");
        assert!(credentials.migrate(false).is_err());
        assert!(!temporary.path().join("secrets").exists());
        legacy_files(&credentials, b"issuer", b"signing");
        credentials
            .store
            .create(
                &credentials.references.signing.name,
                &SecretBytes::new(b"different signing marker".to_vec()).unwrap(),
            )
            .unwrap();
        let error = credentials.migrate(false).unwrap_err();
        assert!(matches!(error, McpCredentialError::DifferentCredential));
        assert!(!format!("{error:?}").contains("different signing marker"));
        assert_eq!(
            credentials
                .store
                .inspect(&credentials.references.issuer.name),
            SecretStoreState::Missing
        );
        assert_eq!(
            credentials
                .store
                .get(&credentials.references.signing.name)
                .unwrap()
                .expose(),
            b"different signing marker"
        );
    }
}
