//! Device lifecycle across vaults: `device replace` and `devices revoke`.

use crate::cli::DeviceCommand;
use crate::commands::enroll::{login_policy, print_report, CliAuthority};
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;
use vulcan_app::device_config::DeviceConfigStore;
use vulcan_app::device_identity::DeviceIdentityStore;
use vulcan_app::device_replace::{
    replace_device, ReplaceEnvironment, ReplaceReport, ReplaceRequest, ReplaceVault,
};
use vulcan_app::device_revoke::{
    revoke_device_in_vault, ForgeAccess, RevokeStepStatus, VaultRevokeReport,
};
use vulcan_app::sync::GitRemote;
use vulcan_app::sync_forge::ForgeDeployKeyAdapter;
use vulcan_app::sync_state::SyncStateStore;
use vulcan_app::sync_transport::probe_device_key;
use vulcan_app::vault_enroll::ForgeAuthority;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_daemon::registry::WikiRegistry;

/// Every registered, available Git vault on this machine.
fn git_vaults(remote: &GitRemote) -> Result<Vec<ReplaceVault>, CliError> {
    let registrations = WikiRegistry::user_default()
        .map_err(CliError::operation)?
        .list(None)
        .map_err(CliError::operation)?;
    Ok(registrations
        .into_iter()
        .filter(|status| {
            status.available && status.registration.sync_backend.as_deref() == Some("git")
        })
        .map(|status| ReplaceVault {
            wiki: status.registration.id.to_string(),
            paths: VaultPaths::new(&status.registration.path),
            remote: remote.clone(),
            permissions_profile: status.registration.permissions_profile,
        })
        .collect())
}

fn check_git(cli: &Cli, vault: &ReplaceVault) -> Result<(), String> {
    let profile = cli
        .permissions
        .as_deref()
        .or(vault.permissions_profile.as_deref());
    let selection = resolve_permission_profile(&vault.paths, profile).map_err(|e| e.to_string())?;
    ProfilePermissionGuard::new(&vault.paths, selection)
        .check_git()
        .map_err(|e| e.to_string())
}

/// A forge adapter for the vault, or the reason there is none. Never starts a login.
#[cfg(feature = "web")]
fn forge_access(
    cli: &Cli,
    vault: &ReplaceVault,
) -> Result<(Box<dyn ForgeDeployKeyAdapter>, String), String> {
    let (config, adapter) = crate::commands::sync::forge_adapter(
        cli,
        &vault.paths,
        vault.permissions_profile.as_deref(),
        false,
    )
    .map_err(|error| error.to_string())?;
    Ok((Box::new(adapter), config.repo))
}

#[cfg(not(feature = "web"))]
fn forge_access(
    _cli: &Cli,
    _vault: &ReplaceVault,
) -> Result<(Box<dyn ForgeDeployKeyAdapter>, String), String> {
    Err("this build has no forge support; rebuild with the `web` feature".to_owned())
}

#[derive(Serialize)]
struct RevokeEverywhereReport {
    version: u32,
    device_id: String,
    dry_run: bool,
    wikis: Vec<RevokeEntry>,
    complete: usize,
    incomplete: usize,
}

#[derive(Serialize)]
struct RevokeEntry {
    wiki: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<VaultRevokeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(crate) fn handle_devices_revoke(
    cli: &Cli,
    device_id: &str,
    remote: &str,
    dry_run: bool,
) -> Result<(), CliError> {
    let remote = GitRemote::parse(remote).map_err(CliError::operation)?;
    let mut wikis = Vec::new();
    for vault in git_vaults(&remote)? {
        let outcome = check_git(cli, &vault).map(|()| {
            let access = forge_access(cli, &vault);
            let forge = match &access {
                Ok((adapter, repo)) => ForgeAccess::Adapter {
                    adapter: adapter.as_ref(),
                    repo,
                },
                Err(reason) => ForgeAccess::Unavailable(reason.clone()),
            };
            revoke_device_in_vault(
                &vault.paths,
                &vault.wiki,
                &vault.remote,
                device_id,
                &forge,
                dry_run,
            )
        });
        wikis.push(match outcome {
            Ok(report) => RevokeEntry {
                wiki: vault.wiki,
                report: Some(report),
                error: None,
            },
            Err(error) => RevokeEntry {
                wiki: vault.wiki,
                report: None,
                error: Some(error),
            },
        });
    }
    let incomplete = wikis
        .iter()
        .filter(|entry| {
            entry.error.is_some()
                || entry
                    .report
                    .as_ref()
                    .is_some_and(VaultRevokeReport::incomplete)
        })
        .count();
    let report = RevokeEverywhereReport {
        version: 1,
        device_id: device_id.to_owned(),
        dry_run,
        complete: wikis.len() - incomplete,
        incomplete,
        wikis,
    };
    if cli.output == OutputFormat::Json {
        print_json(&report)?;
    } else {
        print_revoke(&report);
    }
    if incomplete > 0 {
        return Err(CliError::operation(format!(
            "{incomplete} of {} vaults need attention; the others were revoked",
            report.wikis.len()
        )));
    }
    Ok(())
}

fn step_label(status: &RevokeStepStatus) -> &'static str {
    match status {
        RevokeStepStatus::Done => "done",
        RevokeStepStatus::Already => "already",
        RevokeStepStatus::NotRegistered => "not registered",
        RevokeStepStatus::Pending => "PENDING",
        RevokeStepStatus::Failed => "FAILED",
    }
}

fn print_revoke_vault(report: &VaultRevokeReport) {
    println!(
        "\nWiki {}{}",
        report.wiki,
        if report.dry_run { " (dry run)" } else { "" }
    );
    for (name, step) in [
        ("registration", &report.registration),
        ("forge key", &report.forge),
    ] {
        match &step.detail {
            Some(detail) => println!("  [{}] {name}: {detail}", step_label(&step.status)),
            None => println!("  [{}] {name}", step_label(&step.status)),
        }
    }
}

fn print_revoke(report: &RevokeEverywhereReport) {
    if report.wikis.is_empty() {
        println!(
            "No Git vaults are registered on this machine, so there is nothing to revoke here."
        );
        println!("For a vault this machine does not register, run `vulcan sync devices revoke {}` inside it, or remove the deploy key in the forge.", report.device_id);
        return;
    }
    for entry in &report.wikis {
        match (&entry.report, &entry.error) {
            (Some(vault), _) => print_revoke_vault(vault),
            (None, Some(error)) => println!("\nWiki {}: FAILED\n  {error}", entry.wiki),
            (None, None) => {}
        }
    }
    println!(
        "\n{} complete, {} need attention. Only vaults registered on this machine were reached.",
        report.complete, report.incomplete
    );
}

pub(crate) fn handle_device_replace(cli: &Cli, command: &DeviceCommand) -> Result<(), CliError> {
    let DeviceCommand::Replace {
        activate_anyway,
        revoke_old,
        login,
        remote,
        dry_run,
    } = command
    else {
        unreachable!("only `device replace` is routed here")
    };
    let (activate_anyway, revoke_old, login, dry_run) =
        (*activate_anyway, *revoke_old, *login, *dry_run);
    let remote = GitRemote::parse(remote).map_err(CliError::operation)?;
    let mut vaults = git_vaults(&remote)?;
    // A vault this run may not use Git on is left out rather than half-handled.
    vaults.retain(|vault| check_git(cli, vault).is_ok());

    let device_config = DeviceConfigStore::user_default()
        .and_then(|store| store.load())
        .map_err(CliError::operation)?;
    let state = SyncStateStore::user_default().map_err(CliError::operation)?;
    let identity = DeviceIdentityStore::user_default().map_err(CliError::operation)?;
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let probe = |store: &DeviceIdentityStore, target: &str, dir: Option<&Path>| {
        probe_device_key(store, target, dir)
    };
    let sleep = |duration: Duration| std::thread::sleep(duration);
    let authority = |vault: &ReplaceVault| -> Box<dyn ForgeAuthority + '_> {
        // The paths are owned by the vault list, which outlives the run.
        Box::new(OwnedAuthority {
            cli,
            paths: vault.paths.clone(),
            profile: vault.permissions_profile.clone(),
        })
    };
    let access = |vault: &ReplaceVault| forge_access(cli, vault);
    let environment = ReplaceEnvironment {
        device_config: &device_config,
        identity: &identity,
        state: &state,
        executable: &executable,
        probe: &probe,
        authority: &authority,
        sleep: &sleep,
        forge_access: &access,
        remote_url_override: None,
    };
    let request = ReplaceRequest {
        login: login_policy(login),
        activate_anyway,
        revoke_old,
        dry_run,
    };
    let report = replace_device(&environment, &vaults, request).map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        print_json(&report)?;
    } else {
        print_replace(&report);
    }
    Ok(())
}

/// `CliAuthority` borrows its vault; this owns it so the per-vault factory can
/// hand out a boxed authority.
struct OwnedAuthority<'a> {
    cli: &'a Cli,
    paths: VaultPaths,
    profile: Option<String>,
}

impl ForgeAuthority for OwnedAuthority<'_> {
    fn authorize(
        &self,
        config: &vulcan_app::sync_forge::ForgeConfig,
        device_id: &str,
        public_key: &str,
        label: Option<&str>,
        login_allowed: bool,
    ) -> Result<
        vulcan_app::sync_forge::ForgeAuthorizeReport,
        vulcan_app::vault_enroll::AuthorityError,
    > {
        CliAuthority {
            cli: self.cli,
            paths: &self.paths,
            registration_profile: self.profile.as_deref(),
        }
        .authorize(config, device_id, public_key, label, login_allowed)
    }
}

fn print_replace(report: &ReplaceReport) {
    if let Some(old) = &report.old_device_id {
        println!("Old device: {old}");
    }
    match &report.new_device_id {
        Some(new) => println!("New device: {new}"),
        None => println!("New device: (a key would be generated)"),
    }
    if let Some(key) = &report.new_public_key {
        if !report.activated {
            println!("New public key (for an administrator to authorize by hand):\n  {key}");
        }
    }
    let affected = report.vaults.iter().filter(|entry| entry.affected).count();
    if affected == 0 {
        println!("\nNo vault is bound to the device key, so no vault needs re-enrolling.");
    } else {
        println!("\n{affected} vault(s) are bound to the old key and will be re-enrolled.");
    }
    for entry in &report.vaults {
        if !entry.affected {
            println!(
                "\nWiki {}: not bound to the device key, unaffected",
                entry.wiki
            );
        }
        if let Some(prepare) = &entry.prepare {
            print_report(prepare);
        }
        if let Some(rebind) = &entry.rebind {
            print_report(rebind);
        }
        if let Some(revoke) = &entry.revoke_old {
            print_revoke_vault(revoke);
        }
        if let Some(reason) = &entry.revoke_skipped {
            println!("\nWiki {}: old device kept: {reason}", entry.wiki);
        }
        if let Some(error) = &entry.error {
            println!("\nWiki {}: FAILED\n  {error}", entry.wiki);
        }
    }
    println!(
        "\n{}",
        match (report.activated, report.dry_run) {
            (true, _) => "The new key is now the active device identity.",
            (false, true) => "Dry run: nothing was changed.",
            (false, false) => "The current key is still active; nothing was switched.",
        }
    );
    for next in &report.next_steps {
        println!("  next: {next}");
    }
}
