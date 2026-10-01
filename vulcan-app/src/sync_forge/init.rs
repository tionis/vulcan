//! `sync forge init`: generic, derive-first setup of forge settings.

use super::derive::{derive_forge_target, url_host, ForgeTarget};
use super::shared::{publish_shared_forge, read_shared_forge, SharedForge, SharedForgeView};
use super::{load_config, set_forge_config_with, ForgeConfig, ForgeKind};
use crate::sync_state::SyncStateStore;
use crate::AppError;
use serde::Serialize;
use vulcan_core::VaultPaths;
use vulcan_sync::GitRemote;

#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // One field per command-line switch.
pub struct ForgeInitRequest {
    /// With a kind, settings come from flags plus what the Git remote implies.
    /// Without one, the remote's shared settings are looked up instead.
    pub kind: Option<ForgeKind>,
    pub url: Option<String>,
    pub repo: Option<String>,
    pub token_env: Option<String>,
    pub oauth_client_id: Option<String>,
    /// Save shared settings found on the remote. Without it they are only shown.
    pub adopt: bool,
    /// Publish these settings so other administrators can adopt them.
    pub publish: bool,
    /// Permit an API URL on a different host than the Git remote.
    pub allow_other_host: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeInitReport {
    pub version: u32,
    pub dry_run: bool,
    pub remote: GitRemote,
    /// What the Git remote URL implies; absent when it is not an SSH or HTTPS
    /// URL (a local path, for example).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived: Option<ForgeTarget>,
    /// Settings published on the remote, if the command looked them up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<SharedForgeView>,
    /// The device-local settings now in effect, or that would be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ForgeConfig>,
    pub saved: bool,
    pub published: bool,
    /// Why nothing was saved, when that is not obvious.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Settings resolved before the host check: kind, API URL, and OAuth client ID.
struct Resolved {
    kind: ForgeKind,
    url: String,
    oauth_client_id: Option<String>,
}

/// Resolves the settings from flags, or from the remote's shared proposal.
/// Returns `None` when the proposal was only displayed.
fn resolve_settings(
    paths: &VaultPaths,
    remote: &GitRemote,
    request: &ForgeInitRequest,
    derived: Option<&ForgeTarget>,
    report: &mut ForgeInitReport,
) -> Result<Option<Resolved>, AppError> {
    let default_url = || {
        derived
            .map(|target| target.url.clone())
            .ok_or_else(|| AppError::operation("the Git remote does not imply a forge; pass --url"))
    };
    if let Some(kind) = request.kind {
        let url = match &request.url {
            Some(url) => url.clone(),
            None => default_url()?,
        };
        return Ok(Some(Resolved {
            kind,
            url,
            oauth_client_id: request.oauth_client_id.clone(),
        }));
    }
    let Some(shared) = read_shared_forge(paths, remote)? else {
        return Err(AppError::operation(
            "the remote has no shared forge settings; pass --kind (and --oauth-client-id or --token-env)",
        ));
    };
    let settings = shared.settings.clone();
    report.shared = Some(shared);
    if !request.adopt {
        report.note = Some(
            "these settings were published on the remote by someone who can push; review them and re-run with --adopt to save them locally"
                .to_owned(),
        );
        return Ok(None);
    }
    let url = match request.url.clone().or(settings.api_url) {
        Some(url) => url,
        None => default_url()?,
    };
    Ok(Some(Resolved {
        kind: settings.kind,
        url,
        oauth_client_id: request.oauth_client_id.clone().or(settings.oauth_client_id),
    }))
}

/// The credential only ever goes to the Git remote's own host unless the
/// administrator confirms otherwise on this machine.
fn check_host(
    url: &str,
    derived: Option<&ForgeTarget>,
    allow_other_host: bool,
) -> Result<(), AppError> {
    match derived {
        Some(target) => {
            if !allow_other_host && url_host(url).as_deref() != Some(target.host.as_str()) {
                return Err(AppError::operation(format!(
                    "the forge URL `{url}` is not on the Git remote's host `{}`; pass --allow-other-host to use it anyway",
                    target.host
                )));
            }
        }
        None if !allow_other_host => {
            return Err(AppError::operation(format!(
                "the Git remote's host is unknown, so the forge URL `{url}` cannot be checked against it; pass --allow-other-host to confirm it"
            )));
        }
        None => {}
    }
    Ok(())
}

pub fn forge_init(
    paths: &VaultPaths,
    remote: &GitRemote,
    request: &ForgeInitRequest,
) -> Result<ForgeInitReport, AppError> {
    forge_init_with(
        paths,
        &SyncStateStore::user_default()?,
        remote,
        request,
        None,
    )
}

/// `remote_url` overrides what `git remote get-url` reports; tests use it to
/// derive from a forge URL while talking to a local bare remote.
pub(crate) fn forge_init_with(
    paths: &VaultPaths,
    state: &SyncStateStore,
    remote: &GitRemote,
    request: &ForgeInitRequest,
    remote_url: Option<&str>,
) -> Result<ForgeInitReport, AppError> {
    let remote_url = match remote_url {
        Some(url) => url.to_owned(),
        None => crate::sync_transport::remote_url(paths.vault_root(), remote.as_str())?,
    };
    let derived = derive_forge_target(&remote_url).ok();
    let mut report = ForgeInitReport {
        version: super::SYNC_FORGE_REPORT_VERSION,
        dry_run: request.dry_run,
        remote: remote.clone(),
        derived: derived.clone(),
        shared: None,
        config: None,
        saved: false,
        published: false,
        note: None,
    };

    let Some(Resolved {
        kind,
        url,
        oauth_client_id,
    }) = resolve_settings(paths, remote, request, derived.as_ref(), &mut report)?
    else {
        return Ok(report);
    };
    check_host(&url, derived.as_ref(), request.allow_other_host)?;
    let repo = match (&request.repo, &derived) {
        (Some(repo), _) => repo.clone(),
        (None, Some(target)) => target.repo.clone(),
        (None, None) => {
            return Err(AppError::operation(
                "the Git remote does not imply a repository; pass --repo",
            ))
        }
    };
    let config = ForgeConfig::new(
        kind,
        &url,
        &repo,
        request.token_env.as_deref(),
        oauth_client_id.as_deref(),
    )?;
    let changed = load_config(paths, state)?.as_ref() != Some(&config);
    if changed && !request.dry_run {
        set_forge_config_with(paths, state, &config, false)?;
    }
    report.saved = changed && !request.dry_run;
    report.config = Some(config.clone());

    if request.publish {
        let settings = SharedForge::new(
            config.kind,
            (derived.as_ref().map(|target| target.url.as_str()) != Some(config.url.as_str()))
                .then(|| config.url.clone()),
            config.oauth_client_id.clone(),
        );
        if request.dry_run {
            report.published = false;
            report.note = Some("a dry run publishes nothing".to_owned());
        } else {
            report.published = publish_shared_forge(paths, remote, &settings)?.is_some();
        }
    }
    Ok(report)
}
