//! Installation-wide device inventory presentation.

use crate::cli::DevicesCommand;
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use serde::Serialize;
use std::path::PathBuf;
use vulcan_app::device_identity::{DeviceIdentityReport, DeviceIdentityStore};
use vulcan_app::sync::{GitRefName, GitRemote};
use vulcan_app::sync_devices::{
    list_sync_device_backups_with_observation, SyncDeviceListReport, SyncDeviceOptions,
};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::permissions::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard,
};
use vulcan_core::VaultPaths;
use vulcan_daemon::registry::WikiRegistry;

#[derive(Debug, Serialize)]
struct InstallationDeviceInventory {
    version: u32,
    identity: InventoryField<DeviceIdentityReport>,
    sync_actor: InventoryField<Option<String>>,
    vaults: Vec<VaultInventory>,
}

#[derive(Debug, Serialize)]
struct InventoryField<T> {
    source: &'static str,
    scope: String,
    freshness: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    value: T,
}

#[derive(Debug, Serialize)]
struct VaultInventory {
    wiki_id: String,
    registration_id: String,
    path: PathBuf,
    registration_source: &'static str,
    registration_scope: String,
    local_state: &'static str,
    local_source: &'static str,
    local_freshness: &'static str,
    remote_source: &'static str,
    remote_target_source: &'static str,
    remote_scope: Option<String>,
    remote_freshness: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_inventory: Option<SyncDeviceListReport>,
}

pub(crate) fn handle_devices_command(cli: &Cli, command: &DevicesCommand) -> Result<(), CliError> {
    match command {
        DevicesCommand::List { offline } => {
            let report = build_inventory(cli, *offline)?;
            print_inventory(cli.output, &report)
        }
    }
}

fn build_inventory(cli: &Cli, offline: bool) -> Result<InstallationDeviceInventory, CliError> {
    let identity_store = DeviceIdentityStore::user_default().map_err(CliError::operation)?;
    let sync_actor_result = SyncStateStore::user_default()
        .map_err(CliError::operation)?
        .load_or_create_device_id(false)
        .map(|id| id.map(|id| id.as_str().to_string()));
    let sync_actor = sync_actor_result.as_ref().ok().and_then(Clone::clone);
    let identity = identity_store.inspect_with_legacy_id(sync_actor.as_deref());
    let registrations = WikiRegistry::user_default()
        .map_err(CliError::operation)?
        .list(None)
        .map_err(CliError::operation)?;

    let vaults = registrations
        .into_iter()
        .map(|status| inspect_registered_vault(cli, status, offline))
        .collect();

    Ok(InstallationDeviceInventory {
        version: 1,
        identity: InventoryField {
            source: "installation_device_identity_store",
            scope: "user_data/device".to_string(),
            freshness: "current_local_read",
            error: None,
            value: identity,
        },
        sync_actor: InventoryField {
            source: "installation_sync_state_store",
            scope: "user_state/sync".to_string(),
            freshness: if sync_actor_result.is_ok() {
                "current_local_read"
            } else {
                "current_local_read_failed"
            },
            error: sync_actor_result
                .is_err()
                .then_some("legacy sync actor metadata could not be read"),
            value: sync_actor,
        },
        vaults,
    })
}

fn inspect_registered_vault(
    cli: &Cli,
    status: vulcan_daemon::registry::WikiRegistrationStatus,
    offline: bool,
) -> VaultInventory {
    let registration = status.registration;
    let mut entry = new_vault_inventory(&registration);
    if !status.available {
        entry.local_state = "vault_unavailable";
        entry.detail = Some("registered vault path is unavailable".to_string());
        return entry;
    }
    if registration
        .sync_backend
        .as_deref()
        .is_some_and(|backend| backend != "git")
    {
        entry.local_state = "unsupported_sync_backend";
        entry.detail = Some("this inventory currently supports Git sync registrations".to_string());
        return entry;
    }
    if registration.sync_backend.as_deref() != Some("git") {
        entry.local_state = "not_configured";
        entry.detail = Some("registration does not declare the Git sync backend".to_string());
        return entry;
    }

    let paths = VaultPaths::new(registration.path);
    let permission_profile = cli
        .permissions
        .as_deref()
        .or(registration.permissions_profile.as_deref());
    let Ok(selection) = resolve_permission_profile(&paths, permission_profile) else {
        entry.local_state = "permission_profile_error";
        entry.detail = Some("the registered permission profile could not be resolved".to_string());
        return entry;
    };
    let guard = ProfilePermissionGuard::new(&paths, selection);
    if guard.check_git().is_err() {
        entry.local_state = "permission_denied";
        entry.detail =
            Some("Git inspection was denied by the active permission profile".to_string());
        return entry;
    }
    if guard.check_read_path(".vulcan/device-names").is_err() {
        entry.local_state = "permission_denied";
        entry.detail =
            Some("local sync inventory was denied by the active path permissions".to_string());
        return entry;
    }
    match list_sync_device_backups_with_observation(
        &paths,
        &default_sync_device_options(),
        !offline,
    ) {
        Ok(report) => {
            entry.local_state = "observed";
            entry.local_freshness = "current_local_read";
            entry.remote_scope = Some(format!("{}|{}", report.remote, report.live_ref));
            entry.remote_freshness = if offline {
                "not_requested"
            } else {
                "current_remote_observation_attempt"
            };
            entry.sync_inventory = Some(report);
        }
        Err(error) => {
            entry.local_state = "error";
            entry.local_freshness = "current_local_read_failed";
            entry.detail = Some(sanitize_inventory_error(&error));
        }
    }
    entry
}

fn new_vault_inventory(registration: &vulcan_daemon::registry::WikiRegistration) -> VaultInventory {
    VaultInventory {
        wiki_id: registration.id.to_string(),
        registration_id: registration.registration_id.to_string(),
        path: registration.path.clone(),
        registration_source: "user_registry",
        registration_scope: format!("wiki:{}:{}", registration.id, registration.registration_id),
        local_state: "not_observed",
        local_source: "vault_local_refs_and_shared_labels",
        local_freshness: "not_observed",
        remote_source: "remote_git_refs",
        remote_target_source: "sync_cli_defaults",
        remote_scope: None,
        remote_freshness: "not_observed",
        detail: None,
        sync_inventory: None,
    }
}

fn default_sync_device_options() -> SyncDeviceOptions {
    SyncDeviceOptions {
        remote: GitRemote::parse("origin").expect("origin is a valid Git remote name"),
        live_ref: GitRefName::parse("refs/heads/__vulcan-sync/live")
            .expect("the default sync live ref is valid"),
    }
}

fn sanitize_inventory_error(error: impl std::fmt::Display) -> String {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("permission") || message.contains("denied") {
        "Git inspection was denied by the active permission profile".to_string()
    } else if message.contains("repository") || message.contains("git") {
        "local Git repository inspection failed".to_string()
    } else {
        "local device inventory could not be read".to_string()
    }
}

fn print_inventory(
    output: OutputFormat,
    report: &InstallationDeviceInventory,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let identity = &report.identity.value;
    println!("Installation device inventory");
    println!("Identity status: {:?}", identity.status);
    if let Some(id) = &identity.device_id {
        println!("Identity ID: {id}");
    }
    println!("Identity is local metadata; it does not establish trust or authorization.");
    if let Some(actor) = &report.sync_actor.value {
        println!("Current sync actor (legacy sync state): {actor}");
    }
    for vault in &report.vaults {
        println!("\nWiki {} ({})", vault.wiki_id, vault.local_state);
        if let Some(detail) = &vault.detail {
            println!("  {detail}");
        }
        if let Some(inventory) = &vault.sync_inventory {
            println!(
                "  Legacy sync actor: {}",
                inventory.current_device_id.as_deref().unwrap_or("unknown")
            );
            if inventory.remote_observation.state
                == vulcan_app::sync_devices::SyncDeviceRemoteObservationState::Available
            {
                println!("  Remote safety backups:");
                for backup in &inventory.backups {
                    println!(
                        "    {}{} [{}]",
                        backup
                            .name
                            .as_deref()
                            .map(|name| format!("{name} — "))
                            .unwrap_or_default(),
                        backup.device_id,
                        backup.identity_kind.as_str()
                    );
                }
                if inventory.backups.is_empty() {
                    println!("    (none observed)");
                }
            } else if inventory.remote_observation.state
                == vulcan_app::sync_devices::SyncDeviceRemoteObservationState::NotRequested
            {
                println!("  Remote safety backups: not requested (--offline); unknown");
            } else {
                println!("  Remote safety backups: unknown (remote observation unavailable)");
                if let Some(error) = &inventory.remote_observation.error {
                    println!("    {error}");
                }
            }
            if !inventory.retained_recovery.is_empty() {
                println!("  Retained local recovery copies:");
                for recovery in &inventory.retained_recovery {
                    println!(
                        "    {}{} [{}]",
                        recovery
                            .name
                            .as_deref()
                            .map(|name| format!("{name} — "))
                            .unwrap_or_default(),
                        recovery.device_id,
                        recovery.identity_kind.as_str()
                    );
                }
            }
            let named_without_recovery = if inventory.remote_observation.state
                == vulcan_app::sync_devices::SyncDeviceRemoteObservationState::NotRequested
            {
                &inventory.named_without_local_recovery
            } else {
                &inventory.named_without_backup
            };
            if !named_without_recovery.is_empty() {
                println!("  Named devices without an observed local recovery copy:");
                for named in named_without_recovery {
                    println!(
                        "    {} — {} [{}]",
                        named.name,
                        named.device_id,
                        named.identity_kind.as_str()
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_uses_named_local_and_remote_provenance() {
        let report = InventoryField {
            source: "vault_local_refs_and_shared_labels",
            scope: "wiki:notes:registration".to_string(),
            freshness: "current_local_read",
            error: None,
            value: "observed",
        };
        let value = serde_json::to_value(report).expect("serialize provenance");
        assert_eq!(value["source"], "vault_local_refs_and_shared_labels");
        assert_eq!(value["scope"], "wiki:notes:registration");
        assert_eq!(value["freshness"], "current_local_read");
    }
}
