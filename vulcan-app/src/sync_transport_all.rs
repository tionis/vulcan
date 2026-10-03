//! `sync transport bind --all-wikis`: bind every registered Git vault to the
//! device key, each vault independently.
//!
//! Binding a key the remote refuses would break plain `git` in that vault, and
//! a bulk operation must not do that quietly. So each vault is probed first and
//! only bound when its remote accepts the device key; every other vault is
//! reported as skipped with the reason and the command that fixes it.

use crate::device_config::{DeviceConfig, TransportPolicy};
use crate::device_identity::DeviceIdentityStore;
use crate::sync_forge::derive_forge_target;
use crate::sync_state::SyncStateStore;
use crate::sync_transport::{
    bind_transport_for_url, eligible_key, is_ssh_url, remote_url, GitConfigMode,
    GitTransportBindReport, ProbeOutcome,
};
use crate::AppError;
use serde::Serialize;
use std::path::Path;
use vulcan_core::VaultPaths;
use vulcan_sync::GitRemote;

pub const BIND_ALL_REPORT_VERSION: u32 = 1;

/// One registered Git vault.
pub struct BindVault {
    pub wiki: String,
    pub paths: VaultPaths,
    /// Why this vault may not be touched (for example its permission profile
    /// forbids Git); it is reported as skipped without being probed.
    pub blocked: Option<String>,
}

pub struct BindAllEnvironment<'a> {
    pub device_config: &'a DeviceConfig,
    pub identity: &'a DeviceIdentityStore,
    pub state: &'a SyncStateStore,
    pub executable: &'a Path,
    /// Asks a remote name (inside the directory) whether it accepts the key.
    pub probe: &'a dyn Fn(&str, &Path) -> Result<ProbeOutcome, AppError>,
    /// Replaces `git remote get-url` (tests use local bare remotes).
    pub remote_url_override: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindOutcome {
    Bound,
    Already,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct BindAllEntry {
    pub wiki: String,
    pub outcome: BindOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<GitTransportBindReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BindAllReport {
    pub version: u32,
    pub dry_run: bool,
    pub wikis: Vec<BindAllEntry>,
    pub bound: usize,
    pub already: usize,
    pub skipped: usize,
    pub failed: usize,
}

fn entry(
    wiki: &str,
    outcome: BindOutcome,
    reason: impl Into<Option<String>>,
    report: Option<GitTransportBindReport>,
) -> BindAllEntry {
    BindAllEntry {
        wiki: wiki.to_owned(),
        outcome,
        reason: reason.into(),
        report,
    }
}

fn bind_one(
    env: &BindAllEnvironment<'_>,
    vault: &BindVault,
    remote: &GitRemote,
    mode: GitConfigMode,
    dry_run: bool,
) -> BindAllEntry {
    let wiki = vault.wiki.as_str();
    if let Some(reason) = &vault.blocked {
        return entry(wiki, BindOutcome::Skipped, reason.clone(), None);
    }
    let url = match env.remote_url_override {
        Some(url) => url.to_owned(),
        None => match remote_url(vault.paths.vault_root(), remote.as_str()) {
            Ok(url) => url,
            Err(_) => {
                return entry(
                    wiki,
                    BindOutcome::Skipped,
                    format!("there is no remote named `{remote}`"),
                    None,
                )
            }
        },
    };
    if !is_ssh_url(&url) {
        return entry(
            wiki,
            BindOutcome::Skipped,
            "the remote is not an SSH URL, so the device key cannot authenticate it".to_owned(),
            None,
        );
    }
    let host = derive_forge_target(&url).ok().map(|target| target.host);
    if env.device_config.policy_for(host.as_deref()) == TransportPolicy::Ambient {
        return entry(
            wiki,
            BindOutcome::Skipped,
            "the transport policy for this host is ambient".to_owned(),
            None,
        );
    }
    match (env.probe)(remote.as_str(), vault.paths.vault_root()) {
        Ok(ProbeOutcome::Accepted) => {}
        Ok(ProbeOutcome::Denied(message)) => {
            return entry(
                wiki,
                BindOutcome::Skipped,
                format!(
                    "the remote refused the device key ({message}); authorize it with `vulcan vault enroll {wiki}`"
                ),
                None,
            );
        }
        Ok(ProbeOutcome::Unreachable(message)) => {
            return entry(
                wiki,
                BindOutcome::Skipped,
                format!("the remote could not be reached ({message}); run this again when online"),
                None,
            );
        }
        Err(error) => return entry(wiki, BindOutcome::Failed, error.to_string(), None),
    }
    match bind_transport_for_url(
        &vault.paths,
        env.state,
        env.identity,
        &url,
        mode,
        dry_run,
        env.executable,
    ) {
        Ok(report) => {
            let outcome = if report.changed {
                BindOutcome::Bound
            } else {
                BindOutcome::Already
            };
            entry(
                wiki,
                outcome,
                report.git_config_skipped.clone(),
                Some(report),
            )
        }
        Err(error) => entry(wiki, BindOutcome::Failed, error.to_string(), None),
    }
}

/// Binds every vault whose remote accepts the device key. One vault's outcome
/// never affects another's. A missing or unusable device key is a precondition
/// of the whole run, reported once rather than once per vault.
pub fn bind_all(
    env: &BindAllEnvironment<'_>,
    vaults: &[BindVault],
    remote: &GitRemote,
    mode: GitConfigMode,
    dry_run: bool,
) -> Result<BindAllReport, AppError> {
    eligible_key(env.identity).map_err(AppError::operation)?;
    let wikis = vaults
        .iter()
        .map(|vault| bind_one(env, vault, remote, mode, dry_run))
        .collect::<Vec<_>>();
    let count = |outcome: BindOutcome| wikis.iter().filter(|e| e.outcome == outcome).count();
    Ok(BindAllReport {
        version: BIND_ALL_REPORT_VERSION,
        dry_run,
        bound: count(BindOutcome::Bound),
        already: count(BindOutcome::Already),
        skipped: count(BindOutcome::Skipped),
        failed: count(BindOutcome::Failed),
        wikis,
    })
}

#[cfg(test)]
mod tests;
