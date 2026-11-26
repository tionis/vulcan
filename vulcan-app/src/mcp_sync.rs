//! Permission-aware MCP sync reports and triggers shared across host transports.

use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, RwLock};
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths};

use crate::mcp_protocol::{
    McpMethodError, McpSyncConflictsArgs, McpSyncDoctorArgs, McpSyncTargetArgs,
};
use crate::sync::{
    doctor_git_vault_for_platform, sync_git_vault, GitPlatformProfile, GitRemote, GitSyncOptions,
};
use crate::sync_conflicts::{get_sync_conflict, list_sync_conflicts};

pub fn sync_preview(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpSyncTargetArgs,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let full = crate::mcp_paging::wants_full_detail("sync_status", args.detail.as_deref())?;
    let options = git_sync_options(args)?;
    let report =
        sync_git_vault(paths, &options).map_err(|error| McpMethodError::tool(error.to_string()))?;
    let value = report_value(report)?;
    Ok(if full {
        value
    } else {
        concise_sync_status(&value)
    })
}

/// The parts of a sync preview an agent acts on; `detail: "full"` returns
/// the whole report with installation, preflight, and timing data.
fn concise_sync_status(report: &Value) -> Value {
    let mut concise = serde_json::Map::new();
    for key in [
        "outcome",
        "dry_run",
        "remote",
        "actions",
        "head_before",
        "head_ref_before",
        "retries",
        "safety",
    ] {
        if let Some(value) = report.get(key) {
            if !value.is_null() {
                concise.insert(key.to_string(), value.clone());
            }
        }
    }
    if let Some(compatible) = report
        .get("local_platform_preflight")
        .and_then(|preflight| preflight.get("compatible"))
    {
        concise.insert("platform_compatible".to_string(), compatible.clone());
    }
    Value::Object(concise)
}

pub fn sync_doctor(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpSyncDoctorArgs,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    let target = McpSyncTargetArgs {
        remote: args.remote.clone(),
        detail: None,
    };
    let options = git_sync_options(&target)?;
    let platform = args
        .platform
        .as_deref()
        .map(GitPlatformProfile::parse)
        .transpose()
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))?
        .unwrap_or_else(GitPlatformProfile::native);
    report_value(doctor_git_vault_for_platform(paths, &options, platform))
}

pub fn sync_conflicts(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    args: &McpSyncConflictsArgs,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    if let Some(conflict_id) = args.conflict_id.as_deref() {
        report_value(
            get_sync_conflict(paths, conflict_id)
                .map_err(|error| McpMethodError::tool(error.to_string()))?,
        )
    } else {
        report_value(
            list_sync_conflicts(paths).map_err(|error| McpMethodError::tool(error.to_string()))?,
        )
    }
}

/// What starts or resumes a registered vault's synchronization for MCP
/// tools. The daemon installs one that queues jobs on its own supervisor;
/// `vulcan mcp` installs one that asks the running daemon, or syncs directly
/// when none runs. Registration state lives outside this crate, hence the hook.
/// What a running `sync_refresh` may report and how it learns to stop.
pub struct McpSyncHooks<'a> {
    /// Called with each phase the cycle enters, such as "fetching".
    pub on_phase: &'a mut dyn FnMut(&str),
    /// Whether the call was asked to stop.
    pub cancelled: &'a dyn Fn() -> bool,
}

pub trait McpSyncHost: Send + Sync {
    /// Starts a synchronization of the registered vault at `vault_root` now.
    fn sync_now(&self, vault_root: &Path) -> Result<Value, String>;
    /// [`Self::sync_now`] for a call that reports its phases and can be
    /// stopped. A host that only queues the work answers at once and needs
    /// neither.
    fn sync_now_with(
        &self,
        vault_root: &Path,
        _hooks: &mut McpSyncHooks<'_>,
    ) -> Result<Value, String> {
        self.sync_now(vault_root)
    }
    /// Clears the registration's pause and starts a synchronization.
    fn resume(&self, vault_root: &Path) -> Result<Value, String>;
}

static SYNC_HOST: RwLock<Option<Arc<dyn McpSyncHost>>> = RwLock::new(None);

/// Keeps a sync host installed; dropping it uninstalls the host, so a
/// stopped daemon releases its supervisor and a later one can install its own.
#[must_use = "the host is uninstalled when the registration is dropped"]
pub struct SyncHostRegistration(Arc<dyn McpSyncHost>);

impl Drop for SyncHostRegistration {
    fn drop(&mut self) {
        let mut slot = SYNC_HOST
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot
            .as_ref()
            .is_some_and(|installed| Arc::ptr_eq(installed, &self.0))
        {
            *slot = None;
        }
    }
}

/// Installs this process's sync host unless one is already installed (the
/// daemon's own host wins over a `vulcan mcp` server it runs). The host stays
/// installed while the returned registration lives.
pub fn install_sync_host(host: Box<dyn McpSyncHost>) -> Option<SyncHostRegistration> {
    let mut slot = SYNC_HOST
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_some() {
        return None;
    }
    let host: Arc<dyn McpSyncHost> = Arc::from(host);
    *slot = Some(Arc::clone(&host));
    Some(SyncHostRegistration(host))
}

fn sync_host() -> Result<Arc<dyn McpSyncHost>, McpMethodError> {
    SYNC_HOST
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            McpMethodError::tool(
                "this MCP server cannot start synchronization; run `vulcan sync refresh` instead",
            )
        })
}

pub fn sync_refresh(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    hooks: &mut McpSyncHooks<'_>,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    sync_host()?
        .sync_now_with(paths.vault_root(), hooks)
        .map_err(McpMethodError::tool)
}

pub fn sync_resume(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<Value, McpMethodError> {
    guard
        .check_git()
        .map_err(|error| McpMethodError::tool(error.to_string()))?;
    sync_host()?
        .resume(paths.vault_root())
        .map_err(McpMethodError::tool)
}

fn git_sync_options(args: &McpSyncTargetArgs) -> Result<GitSyncOptions, McpMethodError> {
    let mut options = GitSyncOptions::default();
    if let Some(remote) = args.remote.as_deref() {
        options.remote = GitRemote::parse(remote)
            .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
    }
    options.dry_run = true;
    Ok(options)
}

fn report_value(report: impl Serialize) -> Result<Value, McpMethodError> {
    serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::resolve_permission_profile;

    struct FixedHost(&'static str);

    impl McpSyncHost for FixedHost {
        fn sync_now(&self, _vault_root: &Path) -> Result<Value, String> {
            Ok(Value::from(self.0))
        }
        fn resume(&self, _vault_root: &Path) -> Result<Value, String> {
            Ok(Value::from(self.0))
        }
    }

    // One test owns the process-wide slot, so nothing races it.
    #[test]
    fn a_sync_host_serves_while_registered_and_the_first_one_wins() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("unrestricted")).unwrap(),
        );
        let error = sync_refresh(
            &paths,
            &guard,
            &mut McpSyncHooks {
                on_phase: &mut |_| {},
                cancelled: &|| false,
            },
        )
        .unwrap_err();
        assert!(
            format!("{error:?}").contains("vulcan sync refresh"),
            "{error:?}"
        );

        let first = install_sync_host(Box::new(FixedHost("first"))).expect("installed");
        assert!(install_sync_host(Box::new(FixedHost("second"))).is_none());
        assert_eq!(
            sync_host().unwrap().sync_now(temporary.path()),
            Ok(Value::from("first"))
        );

        // Dropping the registration releases the host and frees the slot.
        drop(first);
        assert!(sync_host().is_err());
        let next = install_sync_host(Box::new(FixedHost("next"))).expect("installed again");
        assert_eq!(
            sync_host().unwrap().sync_now(temporary.path()),
            Ok(Value::from("next"))
        );
        drop(next);
    }

    #[test]
    fn sync_options_are_dry_run_by_default_and_accept_valid_overrides() {
        let default = git_sync_options(&McpSyncTargetArgs {
            remote: None,
            detail: None,
        })
        .expect("default sync options");
        assert!(default.dry_run);
        let explicit = git_sync_options(&McpSyncTargetArgs {
            remote: Some("backup".to_string()),
            detail: None,
        })
        .expect("explicit sync options");
        assert!(explicit.dry_run);
        assert_eq!(explicit.remote.as_str(), "backup");
    }

    #[test]
    fn invalid_sync_remote_is_an_invalid_mcp_parameter() {
        assert!(matches!(
            git_sync_options(&McpSyncTargetArgs {
                remote: Some("-unsafe".to_string()),
                detail: None,
            }),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
    }

    #[test]
    fn every_sync_report_denies_without_git_authority_before_accessing_a_vault() {
        let temporary = tempfile::tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).expect("readonly profile"),
        );
        assert!(matches!(
            sync_preview(&paths, &guard, &McpSyncTargetArgs::default()),
            Err(McpMethodError::Tool { .. })
        ));
        assert!(matches!(
            sync_doctor(&paths, &guard, &McpSyncDoctorArgs::default()),
            Err(McpMethodError::Tool { .. })
        ));
        assert!(matches!(
            sync_conflicts(&paths, &guard, &McpSyncConflictsArgs::default()),
            Err(McpMethodError::Tool { .. })
        ));
        assert!(!temporary.path().join(".git").exists());
        assert!(!temporary.path().join(".vulcan").exists());
    }
}
