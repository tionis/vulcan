//! Retiring a device in one vault: tombstone its registration and remove its
//! Vulcan-marked deploy key, each step reported on its own.

use crate::sync_forge::{remove_device_keys, ForgeDeployKeyAdapter, ForgeRemoveReport};
use crate::sync_registration::{revoke_registration, RegistrationAction};
use serde::Serialize;
use vulcan_core::VaultPaths;
use vulcan_sync::GitRemote;

pub const DEVICE_REVOKE_REPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevokeStepStatus {
    /// Changed now (or would be, on a dry run).
    Done,
    /// Nothing to do: already in the wanted state.
    Already,
    /// The vault has no registration for the device.
    NotRegistered,
    /// Could not run; see the detail.
    Pending,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RevokeStep {
    pub status: RevokeStepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl RevokeStep {
    fn new(status: RevokeStepStatus, detail: impl Into<Option<String>>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultRevokeReport {
    pub version: u32,
    pub wiki: String,
    pub device_id: String,
    pub dry_run: bool,
    pub registration: RevokeStep,
    pub forge: RevokeStep,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forge_removed: Option<ForgeRemoveReport>,
}

impl VaultRevokeReport {
    /// True when something still needs attention in this vault.
    #[must_use]
    pub fn incomplete(&self) -> bool {
        [&self.registration, &self.forge].iter().any(|step| {
            matches!(
                step.status,
                RevokeStepStatus::Pending | RevokeStepStatus::Failed
            )
        })
    }
}

/// Where the forge half comes from: an adapter, or the reason there is none.
pub enum ForgeAccess<'a> {
    Adapter {
        adapter: &'a dyn ForgeDeployKeyAdapter,
        repo: &'a str,
    },
    Unavailable(String),
}

/// Revokes one device in one vault. The two halves are independent: a failed
/// registration write does not stop the key removal, and neither failure is
/// raised as an error, so the caller can carry on with other vaults.
#[must_use]
pub fn revoke_device_in_vault(
    paths: &VaultPaths,
    wiki: &str,
    remote: &GitRemote,
    device_id: &str,
    forge: &ForgeAccess<'_>,
    dry_run: bool,
) -> VaultRevokeReport {
    let registration = match revoke_registration(paths, remote, device_id, dry_run) {
        Ok(report) if report.action == RegistrationAction::Unchanged => {
            RevokeStep::new(RevokeStepStatus::Already, None)
        }
        Ok(_) => RevokeStep::new(RevokeStepStatus::Done, None),
        Err(error)
            if error
                .to_string()
                .contains("no registration for that device") =>
        {
            RevokeStep::new(RevokeStepStatus::NotRegistered, None)
        }
        Err(error) => RevokeStep::new(RevokeStepStatus::Failed, error.to_string()),
    };
    let (forge_step, forge_removed) = match forge {
        ForgeAccess::Unavailable(reason) => (
            RevokeStep::new(RevokeStepStatus::Pending, reason.clone()),
            None,
        ),
        ForgeAccess::Adapter { adapter, repo } => {
            match remove_device_keys(*adapter, repo, device_id, dry_run) {
                Ok(report) if report.removed.is_empty() => (
                    RevokeStep::new(RevokeStepStatus::Already, None),
                    Some(report),
                ),
                Ok(report) => (RevokeStep::new(RevokeStepStatus::Done, None), Some(report)),
                Err(error) => (
                    RevokeStep::new(RevokeStepStatus::Failed, error.to_string()),
                    None,
                ),
            }
        }
    };
    VaultRevokeReport {
        version: DEVICE_REVOKE_REPORT_VERSION,
        wiki: wiki.to_owned(),
        device_id: device_id.to_owned(),
        dry_run,
        registration,
        forge: forge_step,
        forge_removed,
    }
}
