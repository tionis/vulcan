//! The device-key enrollment pipeline (Roadmap 12.22.3).
//!
//! One idempotent function takes a vault from "reachable some way" to "bound
//! to this installation's device key", or reports exactly what is missing. It
//! never asks a question and never binds a key the remote has not accepted:
//! binding sets `core.sshCommand` and makes sync fail closed, so a refused key
//! would break plain `git` at once. Design: `docs/specs/device-key-enrollment.md`.
//!
//! Everything that touches the outside world is injected (the probe, the forge
//! authority, the executable path, sleeping), so every branch is testable
//! without a real SSH server or forge.

use crate::device_config::{DeviceConfig, ForgeEntry, LoginMode, TransportPolicy};
use crate::device_identity::{DeviceIdentityStatus, DeviceIdentityStore};
use crate::sync_forge::{
    derive_forge_target, set_forge_config_with, ForgeAuthorizeReport, ForgeConfig, ForgeTarget,
};
use crate::sync_registration::{register_self, SelfRegistrationOutcome};
use crate::sync_state::SyncStateStore;
use crate::sync_transport::{
    bind_transport_for_url, is_ssh_url, remote_url, GitConfigMode, ProbeOutcome,
};
use crate::AppError;
use serde::Serialize;
use std::path::Path;
use std::time::Duration;
use vulcan_core::VaultPaths;
use vulcan_sync::GitRemote;

pub const VAULT_ENROLL_REPORT_VERSION: u32 = 1;

/// Asks a remote name (inside the directory) or URL whether it accepts the
/// device key alone.
pub type ProbeFn<'a> = dyn Fn(&str, Option<&Path>) -> Result<ProbeOutcome, AppError> + 'a;
const REPROBE_ATTEMPTS: u32 = 3;
const REPROBE_DELAY: Duration = Duration::from_secs(1);

/// Why a forge could not authorize the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    /// A login is required and none was possible.
    NeedsLogin,
    /// No usable credential is configured.
    NoCredential(String),
    /// The forge refused or failed.
    Failed(String),
}

/// Whatever lets this device add its own key to a forge: a credential plus the
/// forge API. Injected so tests need no network.
pub trait ForgeAuthority {
    fn authorize(
        &self,
        config: &ForgeConfig,
        device_id: &str,
        public_key: &str,
        label: Option<&str>,
        login_allowed: bool,
    ) -> Result<ForgeAuthorizeReport, AuthorityError>;
}

/// When an interactive OAuth login may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoginPolicy {
    #[default]
    Never,
    /// Only if the forge entry allows it (`login = "auto"`); the caller sets
    /// this when attached to a terminal.
    Interactive,
    /// The user asked for it explicitly (`--login`).
    Forced,
}

#[derive(Debug, Clone)]
pub struct EnrollRequest {
    /// The wiki's ID, used only in the messages and commands the report prints.
    pub wiki: String,
    pub remote: GitRemote,
    /// Treat the policy as `ambient` for this run only.
    pub no_device_key: bool,
    pub login: LoginPolicy,
    pub dry_run: bool,
}

pub struct EnrollEnvironment<'a> {
    pub device_config: &'a DeviceConfig,
    pub identity: &'a DeviceIdentityStore,
    pub state: &'a SyncStateStore,
    /// The `vulcan` executable a bound `core.sshCommand` should name.
    pub executable: &'a Path,
    pub probe: &'a ProbeFn<'a>,
    pub authority: &'a dyn ForgeAuthority,
    pub sleep: &'a dyn Fn(Duration),
    /// Replaces `git remote get-url`; tests use it to derive a forge from a
    /// URL while talking to a local bare remote.
    pub remote_url_override: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Done,
    Already,
    Skipped,
    /// Waiting on something outside this machine's authority.
    Pending,
    Failed,
    /// A dry run: what would happen.
    Planned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnrollStep {
    pub name: &'static str,
    pub status: StepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollState {
    /// Authorized, bound, and plain `git` configured.
    Bound,
    /// Something outside this machine's authority is missing; see `next_steps`.
    Pending,
    /// Policy or the remote kind says there is nothing to do.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnrollReport {
    pub version: u32,
    pub wiki: String,
    pub dry_run: bool,
    pub remote: GitRemote,
    pub state: EnrollState,
    pub policy: TransportPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    pub steps: Vec<EnrollStep>,
    /// Exact commands to run next, when the state is `pending`.
    pub next_steps: Vec<String>,
}

impl EnrollReport {
    fn push(&mut self, name: &'static str, status: StepStatus, detail: Option<String>) {
        self.steps.push(EnrollStep {
            name,
            status,
            detail,
        });
    }

    fn step(&mut self, name: &'static str, status: StepStatus, detail: impl Into<String>) {
        self.push(name, status, Some(detail.into()));
    }

    fn step_bare(&mut self, name: &'static str, status: StepStatus) {
        self.push(name, status, None);
    }
}

/// Enrolls one vault.
pub fn enroll_vault(
    paths: &VaultPaths,
    env: &EnrollEnvironment<'_>,
    request: &EnrollRequest,
) -> Result<EnrollReport, AppError> {
    let mut report = EnrollReport {
        version: VAULT_ENROLL_REPORT_VERSION,
        wiki: request.wiki.clone(),
        dry_run: request.dry_run,
        remote: request.remote.clone(),
        state: EnrollState::Skipped,
        policy: TransportPolicy::DeviceKey,
        device_id: None,
        steps: Vec::new(),
        next_steps: Vec::new(),
    };

    let url = match env.remote_url_override {
        Some(url) => Ok(url.to_owned()),
        None => remote_url(paths.vault_root(), request.remote.as_str()),
    };
    let Ok(url) = url else {
        report.step(
            "remote",
            StepStatus::Skipped,
            format!("there is no remote named `{}`", request.remote),
        );
        return Ok(report);
    };
    let derived = derive_forge_target(&url).ok();
    let host = derived.as_ref().map(|target| target.host.as_str());
    report.policy = if request.no_device_key {
        TransportPolicy::Ambient
    } else {
        env.device_config.policy_for(host)
    };
    if report.policy == TransportPolicy::Ambient {
        report.step(
            "policy",
            StepStatus::Skipped,
            "transport is ambient: authentication is left to your own SSH setup",
        );
        return Ok(report);
    }
    if !is_ssh_url(&url) {
        report.step(
            "remote",
            StepStatus::Skipped,
            "the remote is not an SSH URL, so the device key cannot authenticate it",
        );
        return Ok(report);
    }
    report.step("policy", StepStatus::Done, "device-key");

    let Some((device_id, public_key)) = ensure_identity(env, request, &mut report)? else {
        report.state = EnrollState::Pending;
        report
            .next_steps
            .push(format!("vulcan vault enroll {}", request.wiki));
        return Ok(report);
    };
    report.device_id = Some(device_id.clone());

    let mut outcome = (env.probe)(request.remote.as_str(), Some(paths.vault_root()))?;
    if outcome == ProbeOutcome::Accepted {
        report.step(
            "probe",
            StepStatus::Already,
            "the remote accepts the device key",
        );
    } else {
        report.step("probe", StepStatus::Pending, probe_detail(&outcome));
        if matches!(outcome, ProbeOutcome::Denied(_)) {
            outcome = authorize_and_reprobe(
                paths,
                env,
                request,
                derived.as_ref(),
                (&device_id, &public_key),
                &mut report,
            )?;
        } else {
            report
                .next_steps
                .push("check the network, then re-run this command".to_owned());
        }
    }

    if outcome == ProbeOutcome::Accepted {
        bind_step(paths, env, request, &url, &device_id, &mut report)?;
        register_step(paths, env, request, &mut report);
        report.state = EnrollState::Bound;
    } else {
        // Not authorized yet: still make this device visible to an administrator
        // using whatever credential works today.
        register_step(paths, env, request, &mut report);
        report.state = EnrollState::Pending;
        if report.next_steps.is_empty() {
            report.next_steps = unauthorized_next_steps(request);
        }
        report
            .next_steps
            .push(format!("vulcan vault enroll {}", request.wiki));
    }
    Ok(report)
}

fn probe_detail(outcome: &ProbeOutcome) -> String {
    match outcome {
        ProbeOutcome::Accepted => "the remote accepts the device key".to_owned(),
        ProbeOutcome::Denied(message) => format!("the remote refused the device key: {message}"),
        ProbeOutcome::Unreachable(message) => format!("the remote could not be reached: {message}"),
    }
}

fn unauthorized_next_steps(request: &EnrollRequest) -> Vec<String> {
    vec![
        "vulcan device public-key".to_owned(),
        "on a machine that can administer the repository: vulcan sync devices register --public-key <that key file> && vulcan sync forge sync (or add the key as a deploy key in the forge's settings)".to_owned(),
        format!("then, here: vulcan vault enroll {}", request.wiki),
    ]
}

/// Returns the device ID and public key, initializing the identity unless this
/// is a dry run. `None` means a dry run found no identity yet.
fn ensure_identity(
    env: &EnrollEnvironment<'_>,
    request: &EnrollRequest,
    report: &mut EnrollReport,
) -> Result<Option<(String, String)>, AppError> {
    let inspected = env.identity.inspect();
    match inspected.status {
        DeviceIdentityStatus::Ready => {
            report.step_bare("identity", StepStatus::Already);
        }
        DeviceIdentityStatus::Uninitialized => {
            if request.dry_run {
                report.step(
                    "identity",
                    StepStatus::Planned,
                    "would create the device identity",
                );
                report.step(
                    "probe",
                    StepStatus::Skipped,
                    "no identity yet, so nothing to probe with",
                );
                return Ok(None);
            }
            env.identity.ensure_device_id()?;
            report.step("identity", StepStatus::Done, "created the device identity");
        }
        DeviceIdentityStatus::Degraded | DeviceIdentityStatus::Invalid => {
            return Err(AppError::operation(
                inspected
                    .diagnostic
                    .unwrap_or_else(|| "the device identity is not usable".to_owned()),
            ));
        }
    }
    let device_id = env
        .identity
        .device_id()?
        .ok_or_else(|| AppError::operation("the device identity has no ID"))?;
    Ok(Some((device_id, env.identity.public_key()?)))
}

/// The forge settings for this vault: its own, else the device-level entry for
/// its host combined with what the remote implies.
fn forge_settings(
    paths: &VaultPaths,
    env: &EnrollEnvironment<'_>,
    derived: Option<&ForgeTarget>,
    request: &EnrollRequest,
) -> Result<Option<(ForgeConfig, Option<ForgeEntry>)>, AppError> {
    let entry = derived
        .and_then(|target| env.device_config.forge(&target.host))
        .cloned();
    if let Some(config) = crate::sync_forge::load_config(paths, env.state)? {
        return Ok(Some((config, entry)));
    }
    let (Some(target), Some(entry)) = (derived, entry) else {
        return Ok(None);
    };
    let Some(kind) = entry.kind else {
        return Ok(None);
    };
    if entry.oauth_client_id.is_none() && entry.token_env.is_none() {
        return Ok(None);
    }
    let config = ForgeConfig::new(
        kind,
        &target.url,
        &target.repo,
        entry.token_env.as_deref(),
        entry.oauth_client_id.as_deref(),
    )?;
    if !request.dry_run {
        set_forge_config_with(paths, env.state, &config, false)?;
    }
    Ok(Some((config, Some(entry))))
}

fn login_allowed(policy: LoginPolicy, entry: Option<&ForgeEntry>) -> bool {
    match policy {
        LoginPolicy::Never => false,
        LoginPolicy::Forced => true,
        LoginPolicy::Interactive => entry.is_none_or(|entry| entry.login == LoginMode::Auto),
    }
}

/// Authorizes the device through the forge, then probes again. Returns the
/// final probe outcome and records every step.
fn authorize_and_reprobe(
    paths: &VaultPaths,
    env: &EnrollEnvironment<'_>,
    request: &EnrollRequest,
    derived: Option<&ForgeTarget>,
    (device_id, public_key): (&str, &str),
    report: &mut EnrollReport,
) -> Result<ProbeOutcome, AppError> {
    let denied = ProbeOutcome::Denied("the device key is not authorized".to_owned());
    let Some((config, entry)) = forge_settings(paths, env, derived, request)? else {
        report.step(
            "authority",
            StepStatus::Pending,
            "no forge is configured for this host, so this machine cannot authorize its own key",
        );
        return Ok(denied);
    };
    if request.dry_run {
        report.step(
            "authority",
            StepStatus::Planned,
            format!(
                "would authorize this device on {} ({})",
                config.repo, config.url
            ),
        );
        return Ok(denied);
    }
    let allowed = login_allowed(request.login, entry.as_ref());
    match env
        .authority
        .authorize(&config, device_id, public_key, None, allowed)
    {
        Ok(authorized) => {
            report.step(
                "authority",
                StepStatus::Done,
                format!(
                    "{:?} the deploy key on {}",
                    authorized.action, authorized.repo
                ),
            );
        }
        Err(AuthorityError::NeedsLogin) => {
            report.step(
                "authority",
                StepStatus::Pending,
                "a forge login is required",
            );
            report.next_steps.push(format!(
                "vulcan sync forge login --wiki {}   (then: vulcan vault enroll {})",
                request.wiki, request.wiki
            ));
            return Ok(denied);
        }
        Err(AuthorityError::NoCredential(message)) => {
            report.step("authority", StepStatus::Pending, message);
            return Ok(denied);
        }
        Err(AuthorityError::Failed(message)) => {
            report.step("authority", StepStatus::Failed, message);
            return Ok(denied);
        }
    }
    let mut outcome = denied;
    for attempt in 0..REPROBE_ATTEMPTS {
        if attempt > 0 {
            (env.sleep)(REPROBE_DELAY);
        }
        outcome = (env.probe)(request.remote.as_str(), Some(paths.vault_root()))?;
        if outcome == ProbeOutcome::Accepted {
            break;
        }
    }
    if outcome == ProbeOutcome::Accepted {
        report.step(
            "probe",
            StepStatus::Done,
            "the remote now accepts the device key",
        );
    } else {
        report.step(
            "probe",
            StepStatus::Pending,
            "the key was added but the remote does not accept it yet; re-run in a moment",
        );
    }
    Ok(outcome)
}

fn bind_step(
    paths: &VaultPaths,
    env: &EnrollEnvironment<'_>,
    request: &EnrollRequest,
    url: &str,
    device_id: &str,
    report: &mut EnrollReport,
) -> Result<(), AppError> {
    let bound = bind_transport_for_url(
        paths,
        env.state,
        env.identity,
        url,
        GitConfigMode::Auto,
        request.dry_run,
        env.executable,
    )?;
    let detail = bound.git_config_skipped.clone();
    let status = match (request.dry_run, bound.changed) {
        (_, false) => StepStatus::Already,
        (true, true) => StepStatus::Planned,
        (false, true) => StepStatus::Done,
    };
    report.push("bind", status, detail);
    debug_assert_eq!(bound.device_id, device_id);
    Ok(())
}

/// Publishes this device's registration. Best effort: a failure is reported but
/// never stops enrollment.
fn register_step(
    paths: &VaultPaths,
    env: &EnrollEnvironment<'_>,
    request: &EnrollRequest,
    report: &mut EnrollReport,
) {
    if request.dry_run {
        report.step(
            "registration",
            StepStatus::Planned,
            "would publish this device's registration",
        );
        return;
    }
    match register_self(paths, &request.remote, env.identity) {
        Ok(Some(SelfRegistrationOutcome::Created | SelfRegistrationOutcome::Claimed)) => {
            report.step_bare("registration", StepStatus::Done);
        }
        Ok(Some(SelfRegistrationOutcome::AlreadyRegistered)) => {
            report.step_bare("registration", StepStatus::Already);
        }
        Ok(Some(SelfRegistrationOutcome::Revoked)) => report.step(
            "registration",
            StepStatus::Pending,
            "an administrator revoked this device; ask them to unregister it first",
        ),
        Ok(None) => report.step("registration", StepStatus::Skipped, "no device identity"),
        Err(error) => report.step(
            "registration",
            StepStatus::Failed,
            format!("could not publish the registration: {error}"),
        ),
    }
}

#[cfg(test)]
mod tests;
