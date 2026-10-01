//! `vulcan vault enroll`: presentation and wiring for the enrollment pipeline.

use crate::cli::VaultCommand;
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use serde::Serialize;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;
use vulcan_app::device_config::DeviceConfigStore;
use vulcan_app::device_identity::DeviceIdentityStore;
use vulcan_app::sync::GitRemote;
use vulcan_app::sync_forge::{ForgeAuthorizeReport, ForgeConfig};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_app::sync_transport::probe_device_key;
use vulcan_app::vault_enroll::{
    enroll_vault, AuthorityError, EnrollEnvironment, EnrollReport, EnrollRequest, EnrollState,
    ForgeAuthority, LoginPolicy, StepStatus,
};
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_daemon::registry::{WikiId, WikiRegistration, WikiRegistry};

/// Applies the vault's permission profile, then runs the credential chain and
/// the forge API on behalf of the pipeline.
struct CliAuthority<'a> {
    cli: &'a Cli,
    paths: &'a VaultPaths,
    registration_profile: Option<&'a str>,
}

impl ForgeAuthority for CliAuthority<'_> {
    #[cfg(feature = "web")]
    fn authorize(
        &self,
        config: &ForgeConfig,
        device_id: &str,
        public_key: &str,
        label: Option<&str>,
        login_allowed: bool,
    ) -> Result<ForgeAuthorizeReport, AuthorityError> {
        use vulcan_app::sync_forge::{
            authorize_device, forge_login, forge_oauth_status, open_in_browser,
            resolve_forge_credential, ForgeKind, ForgejoDeployKeys, DEFAULT_LOGIN_TIMEOUT,
        };
        let failed = |error: &dyn std::fmt::Display| AuthorityError::Failed(error.to_string());
        let profile = self
            .cli
            .permissions
            .as_deref()
            .or(self.registration_profile);
        let selection = resolve_permission_profile(self.paths, profile).map_err(|e| failed(&e))?;
        ProfilePermissionGuard::new(self.paths, selection)
            .check_network(&config.url)
            .map_err(|e| failed(&e))?;

        let env = |name: &str| std::env::var(name).ok();
        let mut credential = resolve_forge_credential(config, &env);
        if credential.is_err() && config.oauth_client_id.is_some() {
            let logged_in = forge_oauth_status(config)
                .ok()
                .flatten()
                .is_some_and(|status| status.logged_in);
            if !logged_in {
                if !login_allowed {
                    return Err(AuthorityError::NeedsLogin);
                }
                let announce = |url: &str| {
                    eprintln!(
                        "Approve the login in your browser. If it does not open, visit:\n  {url}\nWaiting for the redirect to 127.0.0.1 ..."
                    );
                    let _ = open_in_browser(url);
                };
                forge_login(config, DEFAULT_LOGIN_TIMEOUT, &announce).map_err(|e| failed(&e))?;
                credential = resolve_forge_credential(config, &env);
            }
        }
        let credential =
            credential.map_err(|error| AuthorityError::NoCredential(error.to_string()))?;
        let adapter = match config.kind {
            ForgeKind::Forgejo => {
                ForgejoDeployKeys::new(config, credential.token.as_str(), Duration::from_secs(30))
                    .map_err(|e| failed(&e))?
            }
        };
        authorize_device(&adapter, &config.repo, device_id, public_key, label, false)
            .map_err(|e| failed(&e))
    }

    #[cfg(not(feature = "web"))]
    fn authorize(
        &self,
        _config: &ForgeConfig,
        _device_id: &str,
        _public_key: &str,
        _label: Option<&str>,
        _login_allowed: bool,
    ) -> Result<ForgeAuthorizeReport, AuthorityError> {
        let _ = (self.cli, self.paths, self.registration_profile);
        Err(AuthorityError::NoCredential(
            "this build has no forge support; rebuild with the `web` feature".to_owned(),
        ))
    }
}

#[derive(Serialize)]
struct EnrollAllEntry {
    wiki: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<EnrollReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct EnrollAllReport {
    version: u32,
    dry_run: bool,
    wikis: Vec<EnrollAllEntry>,
    bound: usize,
    pending: usize,
    skipped: usize,
    failed: usize,
}

pub(crate) fn handle_vault_enroll(
    cli: &Cli,
    registry: &WikiRegistry,
    command: &VaultCommand,
) -> Result<(), CliError> {
    let VaultCommand::Enroll {
        id,
        all_wikis,
        login,
        no_device_key,
        remote,
        dry_run,
    } = command
    else {
        unreachable!("only `vault enroll` is routed here")
    };
    let remote = GitRemote::parse(remote).map_err(CliError::operation)?;
    let policy = if *login {
        LoginPolicy::Forced
    } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        LoginPolicy::Interactive
    } else {
        LoginPolicy::Never
    };
    let template = |wiki: &str| EnrollRequest {
        wiki: wiki.to_owned(),
        remote: remote.clone(),
        no_device_key: *no_device_key,
        login: policy,
        dry_run: *dry_run,
    };

    if !*all_wikis {
        let id = id
            .as_deref()
            .ok_or_else(|| CliError::operation("name a registered wiki, or pass --all-wikis"))?;
        let status = registry
            .show(&WikiId::parse(id).map_err(CliError::operation)?)
            .map_err(CliError::operation)?;
        let report = enroll_one(cli, id, &status.registration, &template(id))?;
        return print_one(cli.output, &report);
    }

    let mut entries = Vec::new();
    for status in registry.list(None).map_err(CliError::operation)? {
        let registration = status.registration;
        if !status.available || registration.sync_backend.as_deref() != Some("git") {
            continue;
        }
        let wiki = registration.id.to_string();
        match enroll_one(cli, &wiki, &registration, &template(&wiki)) {
            Ok(report) => entries.push(EnrollAllEntry {
                wiki,
                report: Some(report),
                error: None,
            }),
            Err(error) => entries.push(EnrollAllEntry {
                wiki,
                report: None,
                error: Some(error.to_string()),
            }),
        }
    }
    let count = |state: EnrollState| {
        entries
            .iter()
            .filter(|entry| {
                entry
                    .report
                    .as_ref()
                    .is_some_and(|report| report.state == state)
            })
            .count()
    };
    let summary = EnrollAllReport {
        version: 1,
        dry_run: *dry_run,
        bound: count(EnrollState::Bound),
        pending: count(EnrollState::Pending),
        skipped: count(EnrollState::Skipped),
        failed: entries.iter().filter(|entry| entry.error.is_some()).count(),
        wikis: entries,
    };
    if cli.output == OutputFormat::Json {
        print_json(&summary)?;
    } else {
        for entry in &summary.wikis {
            match (&entry.report, &entry.error) {
                (Some(report), _) => print_report(report),
                (None, Some(error)) => println!("\nWiki {}: failed\n  {error}", entry.wiki),
                (None, None) => {}
            }
        }
        println!(
            "\n{} bound, {} pending, {} skipped, {} failed",
            summary.bound, summary.pending, summary.skipped, summary.failed
        );
    }
    if summary.failed > 0 {
        return Err(CliError::operation(format!(
            "{} of {} vaults failed; the others were not affected",
            summary.failed,
            summary.wikis.len()
        )));
    }
    Ok(())
}

fn enroll_one(
    cli: &Cli,
    id: &str,
    registration: &WikiRegistration,
    request: &EnrollRequest,
) -> Result<EnrollReport, CliError> {
    let paths = VaultPaths::new(&registration.path);
    let profile = cli
        .permissions
        .as_deref()
        .or(registration.permissions_profile.as_deref());
    let selection = resolve_permission_profile(&paths, profile).map_err(CliError::operation)?;
    ProfilePermissionGuard::new(&paths, selection)
        .check_git()
        .map_err(CliError::operation)?;

    let device_config = DeviceConfigStore::user_default()
        .and_then(|store| store.load())
        .map_err(CliError::operation)?;
    let state = SyncStateStore::user_default().map_err(CliError::operation)?;
    let identity = DeviceIdentityStore::user_default().map_err(CliError::operation)?;
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let authority = CliAuthority {
        cli,
        paths: &paths,
        registration_profile: registration.permissions_profile.as_deref(),
    };
    let probe =
        |target: &str, directory: Option<&Path>| probe_device_key(&identity, target, directory);
    let sleep = |duration: Duration| std::thread::sleep(duration);
    let environment = EnrollEnvironment {
        device_config: &device_config,
        identity: &identity,
        state: &state,
        executable: &executable,
        probe: &probe,
        authority: &authority,
        sleep: &sleep,
        remote_url_override: None,
    };
    let _ = id;
    enroll_vault(&paths, &environment, request).map_err(CliError::operation)
}

fn print_one(output: OutputFormat, report: &EnrollReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    print_report(report);
    Ok(())
}

fn print_report(report: &EnrollReport) {
    println!(
        "\nWiki {}: {}{}",
        report.wiki,
        match report.state {
            EnrollState::Bound => "bound to the device key",
            EnrollState::Pending => "pending",
            EnrollState::Skipped => "skipped",
        },
        if report.dry_run { " (dry run)" } else { "" }
    );
    if let Some(device_id) = &report.device_id {
        println!("  device: {device_id}");
    }
    for step in &report.steps {
        let label = match step.status {
            StepStatus::Done => "done",
            StepStatus::Already => "already",
            StepStatus::Skipped => "skipped",
            StepStatus::Pending => "pending",
            StepStatus::Failed => "FAILED",
            StepStatus::Planned => "would",
        };
        match &step.detail {
            Some(detail) => println!("  [{label}] {}: {detail}", step.name),
            None => println!("  [{label}] {}", step.name),
        }
    }
    if !report.next_steps.is_empty() {
        println!("  next:");
        for next in &report.next_steps {
            println!("    {next}");
        }
    }
}
