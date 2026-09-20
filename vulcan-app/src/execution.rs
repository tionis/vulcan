//! Transport-neutral identity, authority, cancellation, and deadline state.
//!
//! Async adapters may construct this context before entering a synchronous app
//! workflow. The app layer itself does not require an async runtime.

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ulid::Ulid;
use vulcan_core::PermissionGrant;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionVaultIdentity {
    pub canonical_root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<ExecutionRepositoryIdentity>,
}

impl ExecutionVaultIdentity {
    pub fn resolve(
        root: &Path,
        registration_id: Option<String>,
        repository: Option<ExecutionRepositoryIdentity>,
    ) -> Result<Self, ExecutionContextError> {
        let canonical_root = root.canonicalize().map_err(|error| {
            ExecutionContextError::InvalidVaultIdentity(format!(
                "could not canonicalize vault `{}`: {error}",
                root.display()
            ))
        })?;
        if !canonical_root.is_dir() {
            return Err(ExecutionContextError::InvalidVaultIdentity(format!(
                "vault `{}` is not a directory",
                canonical_root.display()
            )));
        }
        if registration_id.as_deref().is_some_and(str::is_empty) {
            return Err(ExecutionContextError::InvalidVaultIdentity(
                "registration identity must not be empty".to_string(),
            ));
        }
        Ok(Self {
            canonical_root,
            registration_id,
            repository,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRepositoryIdentity {
    /// Stable identity derived by the repository layer, not a display path.
    pub key: String,
    pub canonical_git_dir: PathBuf,
}

impl ExecutionRepositoryIdentity {
    pub fn resolve(key: impl Into<String>, git_dir: &Path) -> Result<Self, ExecutionContextError> {
        let key = key.into();
        if key.is_empty() {
            return Err(ExecutionContextError::InvalidVaultIdentity(
                "repository key must not be empty".to_string(),
            ));
        }
        let canonical_git_dir = git_dir.canonicalize().map_err(|error| {
            ExecutionContextError::InvalidVaultIdentity(format!(
                "could not canonicalize Git directory `{}`: {error}",
                git_dir.display()
            ))
        })?;
        Ok(Self {
            key,
            canonical_git_dir,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionAuthority {
    Caller {
        principal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        credential_id: Option<String>,
        permission_ceiling: PermissionGrant,
    },
    BackgroundService {
        service_id: String,
        authority_id: String,
        permission_ceiling: PermissionGrant,
    },
}

impl ExecutionAuthority {
    #[must_use]
    pub fn permission_ceiling(&self) -> &PermissionGrant {
        match self {
            Self::Caller {
                permission_ceiling, ..
            }
            | Self::BackgroundService {
                permission_ceiling, ..
            } => permission_ceiling,
        }
    }

    fn validate(&self) -> Result<(), ExecutionContextError> {
        let (kind, primary, secondary) = match self {
            Self::Caller {
                principal_id,
                credential_id,
                ..
            } => ("principal", principal_id.as_str(), credential_id.as_deref()),
            Self::BackgroundService {
                service_id,
                authority_id,
                ..
            } => ("service", service_id.as_str(), Some(authority_id.as_str())),
        };
        if primary.is_empty() || secondary.is_some_and(str::is_empty) {
            return Err(ExecutionContextError::InvalidAuthority(format!(
                "{kind} execution authority contains an empty identity"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub service_instance_id: String,
    pub request_id: String,
    pub operation_id: String,
}

impl ExecutionIdentity {
    #[must_use]
    pub fn new(service_instance_id: impl Into<String>) -> Self {
        Self {
            service_instance_id: service_instance_id.into(),
            request_id: new_id(),
            operation_id: new_id(),
        }
    }

    pub fn supplied(
        service_instance_id: impl Into<String>,
        request_id: impl Into<String>,
        operation_id: impl Into<String>,
    ) -> Result<Self, ExecutionContextError> {
        let identity = Self {
            service_instance_id: service_instance_id.into(),
            request_id: request_id.into(),
            operation_id: operation_id.into(),
        };
        identity.validate()?;
        Ok(identity)
    }

    fn validate(&self) -> Result<(), ExecutionContextError> {
        if self.service_instance_id.is_empty()
            || self.request_id.is_empty()
            || self.operation_id.is_empty()
        {
            Err(ExecutionContextError::InvalidIdentity(
                "service instance, request, and operation identities must not be empty".to_string(),
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRetryClass {
    ReadOnly,
    Idempotent,
    DurableRecovery,
    IndeterminateAfterDispatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionDeadline {
    pub unix_epoch_ms: u64,
}

impl ExecutionDeadline {
    #[must_use]
    pub fn after(duration: Duration) -> Self {
        Self::at(SystemTime::now() + duration)
    }

    #[must_use]
    pub fn at(time: SystemTime) -> Self {
        let millis = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Self {
            unix_epoch_ms: u64::try_from(millis).unwrap_or(u64::MAX),
        }
    }

    #[must_use]
    pub fn is_expired_at(self, now: SystemTime) -> bool {
        Self::at(now).unix_epoch_ms >= self.unix_epoch_ms
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl ExecutionCancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub vault: ExecutionVaultIdentity,
    pub authority: ExecutionAuthority,
    pub effective_permissions: PermissionGrant,
    pub identity: ExecutionIdentity,
    pub audience: Option<String>,
    pub retry_class: ExecutionRetryClass,
    pub cancellation: ExecutionCancellationToken,
    pub deadline: Option<ExecutionDeadline>,
}

impl ExecutionContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vault: ExecutionVaultIdentity,
        authority: ExecutionAuthority,
        effective_permissions: PermissionGrant,
        identity: ExecutionIdentity,
        audience: Option<String>,
        retry_class: ExecutionRetryClass,
        cancellation: ExecutionCancellationToken,
        deadline: Option<ExecutionDeadline>,
    ) -> Result<Self, ExecutionContextError> {
        authority.validate()?;
        identity.validate()?;
        if !effective_permissions.is_subset_of(authority.permission_ceiling()) {
            return Err(ExecutionContextError::PermissionCeilingExceeded);
        }
        if audience.as_deref().is_some_and(str::is_empty) {
            return Err(ExecutionContextError::InvalidIdentity(
                "execution audience must not be empty when supplied".to_string(),
            ));
        }
        let context = Self {
            vault,
            authority,
            effective_permissions,
            identity,
            audience,
            retry_class,
            cancellation,
            deadline,
        };
        context.checkpoint_at(SystemTime::now())?;
        Ok(context)
    }

    pub fn checkpoint(&self) -> Result<(), ExecutionContextError> {
        self.checkpoint_at(SystemTime::now())
    }

    fn checkpoint_at(&self, now: SystemTime) -> Result<(), ExecutionContextError> {
        if self.cancellation.is_cancelled() {
            return Err(ExecutionContextError::Cancelled);
        }
        if self
            .deadline
            .is_some_and(|deadline| deadline.is_expired_at(now))
        {
            return Err(ExecutionContextError::DeadlineExceeded);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionContextError {
    InvalidVaultIdentity(String),
    InvalidAuthority(String),
    InvalidIdentity(String),
    PermissionCeilingExceeded,
    Cancelled,
    DeadlineExceeded,
}

impl Display for ExecutionContextError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVaultIdentity(message)
            | Self::InvalidAuthority(message)
            | Self::InvalidIdentity(message) => formatter.write_str(message),
            Self::PermissionCeilingExceeded => formatter.write_str(
                "effective permissions exceed the configured execution authority ceiling",
            ),
            Self::Cancelled => formatter.write_str("execution was cancelled"),
            Self::DeadlineExceeded => formatter.write_str("execution deadline was exceeded"),
        }
    }
}

impl std::error::Error for ExecutionContextError {}

fn new_id() -> String {
    Ulid::new().to_string().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::{PathPermission, ResourceLimits, ResourceSpecifier};

    fn grant(write: bool) -> PermissionGrant {
        let all = PathPermission {
            allow: vec![ResourceSpecifier::All],
            deny: Vec::new(),
        };
        PermissionGrant {
            read: all.clone(),
            write: if write {
                all.clone()
            } else {
                PathPermission::default()
            },
            refactor: PathPermission::default(),
            git: false,
            network: false,
            network_domains: Vec::new(),
            index: true,
            config_read: false,
            config_write: false,
            execute: false,
            shell: false,
            limits: ResourceLimits::default(),
        }
    }

    #[test]
    fn vault_identity_resolves_symlinks_to_one_canonical_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("vault");
        std::fs::create_dir(&root).expect("vault");
        #[cfg(unix)]
        {
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&root, &alias).expect("symlink");
            let direct = ExecutionVaultIdentity::resolve(&root, None, None).expect("direct");
            let through_alias = ExecutionVaultIdentity::resolve(&alias, None, None).expect("alias");
            assert_eq!(direct.canonical_root, through_alias.canonical_root);
        }
    }

    #[test]
    fn effective_permissions_cannot_exceed_caller_ceiling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let vault = ExecutionVaultIdentity::resolve(temp.path(), None, None).expect("vault");
        let authority = ExecutionAuthority::Caller {
            principal_id: "user:1".to_string(),
            credential_id: Some("credential:1".to_string()),
            permission_ceiling: grant(false),
        };
        let error = ExecutionContext::new(
            vault,
            authority,
            grant(true),
            ExecutionIdentity::new("test"),
            None,
            ExecutionRetryClass::IndeterminateAfterDispatch,
            ExecutionCancellationToken::default(),
            None,
        )
        .expect_err("broader effective grant");
        assert_eq!(error, ExecutionContextError::PermissionCeilingExceeded);
    }

    #[test]
    fn background_authority_is_explicit_and_not_a_caller_credential() {
        let temp = tempfile::tempdir().expect("tempdir");
        let context = ExecutionContext::new(
            ExecutionVaultIdentity::resolve(temp.path(), None, None).expect("vault"),
            ExecutionAuthority::BackgroundService {
                service_id: "auto-commit".to_string(),
                authority_id: "configured:auto-commit".to_string(),
                permission_ceiling: grant(true),
            },
            grant(false),
            ExecutionIdentity::new("daemon:test"),
            None,
            ExecutionRetryClass::DurableRecovery,
            ExecutionCancellationToken::default(),
            None,
        )
        .expect("background context");
        assert!(matches!(
            context.authority,
            ExecutionAuthority::BackgroundService { .. }
        ));
    }

    #[test]
    fn cancellation_and_deadline_are_checked_cooperatively() {
        let token = ExecutionCancellationToken::default();
        let temp = tempfile::tempdir().expect("tempdir");
        let make = |cancellation, deadline| {
            ExecutionContext::new(
                ExecutionVaultIdentity::resolve(temp.path(), None, None).expect("vault"),
                ExecutionAuthority::Caller {
                    principal_id: "user:1".to_string(),
                    credential_id: None,
                    permission_ceiling: grant(false),
                },
                grant(false),
                ExecutionIdentity::new("test"),
                None,
                ExecutionRetryClass::ReadOnly,
                cancellation,
                deadline,
            )
        };
        let context = make(token.clone(), None).expect("context");
        token.cancel();
        assert_eq!(context.checkpoint(), Err(ExecutionContextError::Cancelled));

        let expired = ExecutionDeadline::at(UNIX_EPOCH);
        assert!(matches!(
            make(ExecutionCancellationToken::default(), Some(expired)),
            Err(ExecutionContextError::DeadlineExceeded)
        ));
    }
}
