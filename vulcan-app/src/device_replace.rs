//! `device replace`: swap this installation's device key without breaking a
//! working vault.
//!
//! The new key is staged beside the active one, authorized and proven in every
//! vault bound to the old key, and only then activated. Vaults are handled
//! independently; one that cannot be proven stays pending and never blocks the
//! others (unless the caller wants all-or-nothing, which is the default).

use crate::device_config::DeviceConfig;
use crate::device_identity::{DeviceIdentityStatus, DeviceIdentityStore};
use crate::device_revoke::{revoke_device_in_vault, ForgeAccess, VaultRevokeReport};
use crate::sync_forge::ForgeDeployKeyAdapter;
use crate::sync_state::SyncStateStore;
use crate::sync_transport::{transport_status_with_store, ProbeOutcome};
use crate::vault_enroll::{
    enroll_vault, prepare_vault, EnrollEnvironment, EnrollReport, EnrollRequest, EnrollState,
    ForgeAuthority, LoginPolicy,
};
use crate::AppError;
use serde::Serialize;
use std::path::Path;
use std::time::Duration;
use vulcan_core::VaultPaths;
use vulcan_sync::GitRemote;

pub const DEVICE_REPLACE_REPORT_VERSION: u32 = 1;

/// One registered Git vault this installation knows about.
pub struct ReplaceVault {
    pub wiki: String,
    pub paths: VaultPaths,
    pub remote: GitRemote,
    /// The registration's permission profile, for the caller's per-vault checks.
    pub permissions_profile: Option<String>,
}

/// The forge authority for one vault; permission checks are per vault.
pub type AuthorityFn<'a> = dyn Fn(&ReplaceVault) -> Box<dyn ForgeAuthority + 'a> + 'a;

/// A forge adapter for one vault, or why none could be built.
pub type ForgeAccessFn<'a> =
    dyn Fn(&ReplaceVault) -> Result<(Box<dyn ForgeDeployKeyAdapter>, String), String> + 'a;
pub type ReplaceProbeFn<'a> =
    dyn Fn(&DeviceIdentityStore, &str, Option<&Path>) -> Result<ProbeOutcome, AppError> + 'a;

pub struct ReplaceEnvironment<'a> {
    pub device_config: &'a DeviceConfig,
    /// The active identity (the one being replaced).
    pub identity: &'a DeviceIdentityStore,
    pub state: &'a SyncStateStore,
    pub executable: &'a Path,
    pub probe: &'a ReplaceProbeFn<'a>,
    pub authority: &'a AuthorityFn<'a>,
    pub sleep: &'a dyn Fn(Duration),
    pub forge_access: &'a ForgeAccessFn<'a>,
    /// Replaces `git remote get-url` (tests talk to local bare remotes).
    pub remote_url_override: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)] // Independent command-line switches.
pub struct ReplaceRequest {
    pub login: LoginPolicy,
    /// Activate even when some vaults could not be proven.
    pub activate_anyway: bool,
    /// Revoke the old device's registrations and deploy keys afterwards.
    pub revoke_old: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplaceVaultReport {
    pub wiki: String,
    /// The vault was bound to the old key and so is affected by the swap.
    pub affected: bool,
    /// The staged key authorized and proven here, before activation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare: Option<EnrollReport>,
    /// Re-enrollment with the new key, after activation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebind: Option<EnrollReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoke_old: Option<VaultRevokeReport>,
    /// Why revoking the old device was skipped here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoke_skipped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplaceReport {
    pub version: u32,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_device_id: Option<String>,
    /// The staged public key, for an administrator to authorize by hand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_public_key: Option<String>,
    pub activated: bool,
    pub vaults: Vec<ReplaceVaultReport>,
    pub next_steps: Vec<String>,
}

fn request_for(vault: &ReplaceVault, request: ReplaceRequest) -> EnrollRequest {
    EnrollRequest {
        wiki: vault.wiki.clone(),
        remote: vault.remote.clone(),
        no_device_key: false,
        login: request.login,
        dry_run: request.dry_run,
    }
}

/// Replaces the device key. See the module documentation.
pub fn replace_device(
    env: &ReplaceEnvironment<'_>,
    vaults: &[ReplaceVault],
    request: ReplaceRequest,
) -> Result<ReplaceReport, AppError> {
    let active = env.identity.inspect();
    let staged_store = env.identity.staged()?;
    let interrupted = active.status == DeviceIdentityStatus::Uninitialized
        && staged_store.inspect().status == DeviceIdentityStatus::Ready;
    if active.status == DeviceIdentityStatus::Uninitialized && !interrupted {
        return Err(AppError::operation(
            "this installation has no device identity to replace; run `vulcan device init`",
        ));
    }
    if matches!(
        active.status,
        DeviceIdentityStatus::Degraded | DeviceIdentityStatus::Invalid
    ) {
        return Err(AppError::operation(active.diagnostic.unwrap_or_else(
            || "the active device identity is not usable; repair it first".to_owned(),
        )));
    }

    let staged = env.identity.stage_replacement(request.dry_run)?;
    let old_device_id = staged.old_device_id.clone();
    let new_device_id = staged.staged.device_id.clone();
    let new_public_key = staged_store.public_key().ok();

    let mut report = ReplaceReport {
        version: DEVICE_REPLACE_REPORT_VERSION,
        dry_run: request.dry_run,
        old_device_id: old_device_id.clone(),
        new_device_id: new_device_id.clone(),
        new_public_key,
        activated: false,
        vaults: Vec::new(),
        next_steps: Vec::new(),
    };

    let mut entries = classify_vaults(
        env,
        vaults,
        old_device_id.as_deref(),
        new_device_id.as_deref(),
    );

    // 1. Authorize and prove the staged key everywhere it will be needed.
    if !interrupted {
        let staged_probe =
            |target: &str, dir: Option<&Path>| (env.probe)(&staged_store, target, dir);
        for (vault, entry) in vaults.iter().zip(&mut entries) {
            if !entry.affected {
                continue;
            }
            let authority = (env.authority)(vault);
            let staged_env = EnrollEnvironment {
                device_config: env.device_config,
                identity: &staged_store,
                state: env.state,
                executable: env.executable,
                probe: &staged_probe,
                authority: authority.as_ref(),
                sleep: env.sleep,
                remote_url_override: env.remote_url_override,
            };
            match prepare_vault(&vault.paths, &staged_env, &request_for(vault, request)) {
                Ok(prepared) => entry.prepare = Some(prepared),
                Err(error) => entry.error = Some(error.to_string()),
            }
        }
    }

    let all_proven = entries.iter().filter(|entry| entry.affected).all(|entry| {
        entry.error.is_none()
            && entry.prepare.as_ref().is_none_or(|prepared| {
                matches!(prepared.state, EnrollState::Ready | EnrollState::Skipped)
            })
    });

    // 2. Activate once every affected vault is proven (or the user accepts less).
    if !request.dry_run && (all_proven || request.activate_anyway) {
        env.identity.activate_staged()?;
        report.activated = true;
    } else if !request.dry_run {
        report.next_steps.push(
            "some vaults could not be proven with the new key; complete the pending steps and re-run `vulcan device replace`, or pass --activate-anyway to switch now (those vaults fail closed until re-enrolled)"
                .to_owned(),
        );
    }

    // 3. Re-enroll with the now-active key, which rebinds each vault.
    if report.activated {
        rebind_affected(env, vaults, &mut entries, request);
    }

    // 4. Retire the old device (see `retire_old_device`).
    if request.revoke_old && (report.activated || request.dry_run) {
        if let Some(old) = &old_device_id {
            retire_old_device(env, vaults, &mut entries, old, request.dry_run);
        }
    } else if !request.revoke_old && report.activated {
        if let Some(old) = &old_device_id {
            report.next_steps.push(format!(
                "retire the old device when ready: vulcan devices revoke {old}"
            ));
        }
    }
    report.vaults = entries;
    Ok(report)
}

fn rebind_affected(
    env: &ReplaceEnvironment<'_>,
    vaults: &[ReplaceVault],
    entries: &mut [ReplaceVaultReport],
    request: ReplaceRequest,
) {
    let active_probe = |target: &str, dir: Option<&Path>| (env.probe)(env.identity, target, dir);
    for (vault, entry) in vaults.iter().zip(entries) {
        if !entry.affected {
            continue;
        }
        let authority = (env.authority)(vault);
        let active_env = EnrollEnvironment {
            device_config: env.device_config,
            identity: env.identity,
            state: env.state,
            executable: env.executable,
            probe: &active_probe,
            authority: authority.as_ref(),
            sleep: env.sleep,
            remote_url_override: env.remote_url_override,
        };
        match enroll_vault(&vault.paths, &active_env, &request_for(vault, request)) {
            Ok(done) => entry.rebind = Some(done),
            Err(error) => entry.error = Some(error.to_string()),
        }
    }
}

/// Affected vaults are those bound to the key being replaced (or, after an
/// interrupted swap, to any key other than the new one).
fn classify_vaults(
    env: &ReplaceEnvironment<'_>,
    vaults: &[ReplaceVault],
    old_device_id: Option<&str>,
    new_device_id: Option<&str>,
) -> Vec<ReplaceVaultReport> {
    vaults
        .iter()
        .map(|vault| {
            let bound = transport_status_with_store(&vault.paths, env.state, env.identity)
                .ok()
                .and_then(|status| status.bound_device_id);
            let affected = bound.is_some_and(|bound| {
                old_device_id == Some(bound.as_str())
                    || (old_device_id.is_none() && new_device_id != Some(bound.as_str()))
            });
            ReplaceVaultReport {
                wiki: vault.wiki.clone(),
                affected,
                prepare: None,
                rebind: None,
                revoke_old: None,
                revoke_skipped: None,
                error: None,
            }
        })
        .collect()
}

/// Revokes the old device in every vault, but never where the new key is not
/// yet proven: removing the old key there would leave neither.
fn retire_old_device(
    env: &ReplaceEnvironment<'_>,
    vaults: &[ReplaceVault],
    entries: &mut [ReplaceVaultReport],
    old: &str,
    dry_run: bool,
) {
    for (vault, entry) in vaults.iter().zip(entries) {
        let proven_here = !entry.affected
            || if dry_run {
                entry
                    .prepare
                    .as_ref()
                    .is_some_and(|prepared| prepared.state != EnrollState::Pending)
            } else {
                entry
                    .rebind
                    .as_ref()
                    .is_some_and(|done| done.state == EnrollState::Bound)
            };
        if !proven_here {
            entry.revoke_skipped = Some(
                "the new key is not proven in this vault yet; revoking the old one now would lock it out"
                    .to_owned(),
            );
            continue;
        }
        let access = (env.forge_access)(vault);
        let forge = match &access {
            Ok((adapter, repo)) => ForgeAccess::Adapter {
                adapter: adapter.as_ref(),
                repo,
            },
            Err(reason) => ForgeAccess::Unavailable(reason.clone()),
        };
        entry.revoke_old = Some(revoke_device_in_vault(
            &vault.paths,
            &vault.wiki,
            &vault.remote,
            old,
            &forge,
            dry_run,
        ));
    }
}

#[cfg(test)]
mod tests;
