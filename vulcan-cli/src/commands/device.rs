use crate::cli::DeviceCommand;
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use serde::Serialize;
use vulcan_app::device_identity::{
    DeviceIdentityInitReport, DeviceIdentityReport, DeviceIdentityStatus, DeviceIdentityStore,
};
use vulcan_app::sync_state::SyncStateStore;

#[derive(Serialize)]
struct PublicKeyReport {
    version: u32,
    public_key: String,
}

pub(crate) fn handle_device_command(cli: &Cli, command: &DeviceCommand) -> Result<(), CliError> {
    let store = DeviceIdentityStore::user_default().map_err(CliError::operation)?;
    match command {
        DeviceCommand::Show => {
            let report = inspect_with_legacy_state(&store);
            print_device_show(cli.output, &report)
        }
        DeviceCommand::Init { dry_run } => {
            let mut report = store.initialize(*dry_run).map_err(CliError::operation)?;
            report.identity = inspect_with_legacy_state(&store);
            print_device_init(cli.output, &report)
        }
        DeviceCommand::PublicKey => {
            let public_key = store.public_key().map_err(CliError::operation)?;
            match cli.output {
                OutputFormat::Json => print_json(&PublicKeyReport {
                    version: 1,
                    public_key,
                }),
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!("{public_key}");
                    Ok(())
                }
            }
        }
    }
}

fn inspect_with_legacy_state(store: &DeviceIdentityStore) -> DeviceIdentityReport {
    let legacy =
        SyncStateStore::user_default().and_then(|state| state.load_or_create_device_id(false));
    if let Ok(id) = legacy {
        return store
            .inspect_with_legacy_id(id.as_ref().map(vulcan_app::sync::GitSyncDeviceId::as_str));
    }
    let mut report = store.inspect();
    report.sync_identity_state = "legacy_unavailable".to_string();
    let detail = "legacy sync actor state could not be read or validated; run `vulcan sync doctor`";
    report.diagnostic = Some(match report.diagnostic {
        Some(existing) => format!("{existing}; {detail}"),
        None => detail.to_string(),
    });
    report
}

fn print_device_show(output: OutputFormat, report: &DeviceIdentityReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!("Local device identity: {}", status_label(report.status));
    if let Some(id) = &report.device_id {
        println!("Key identity ID: {id}");
    }
    if let Some(id) = &report.sync_actor_id {
        println!("Current sync actor ID: {id}");
    }
    if let Some(fingerprint) = &report.fingerprint {
        println!("Public fingerprint: {fingerprint}");
    }
    if let Some(provider) = &report.key_provider {
        println!("Key provider: {provider}");
        println!(
            "Private key: {}",
            if report.private_key_available {
                "available"
            } else {
                "unavailable"
            }
        );
    }
    if let Some(diagnostic) = &report.diagnostic {
        println!("{diagnostic}");
    }
    if report.sync_identity_state == "key_pending_rollout" {
        println!("Sync still uses legacy ULID actor IDs; this key is not yet its sync actor.");
    }
    println!("Identity is not a trust or access decision.");
    Ok(())
}

fn print_device_init(
    output: OutputFormat,
    report: &DeviceIdentityInitReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.dry_run {
        println!("Would initialize an Ed25519 device key in Vulcan user data.");
        println!(
            "Current identity state: {}",
            status_label(report.identity.status)
        );
        println!("No key was generated.");
    } else if report.created {
        println!(
            "Created device identity {}.",
            report.identity.device_id.as_deref().unwrap_or("(unknown)")
        );
    } else if report.adopted_interrupted_initialization {
        println!(
            "Adopted an existing complete device keypair as identity {}.",
            report.identity.device_id.as_deref().unwrap_or("(unknown)")
        );
    } else {
        println!(
            "Device identity {} is already initialized; no key was replaced.",
            report.identity.device_id.as_deref().unwrap_or("(unknown)")
        );
    }
    if let Some(id) = &report.identity.sync_actor_id {
        println!("Current sync actor ID: {id}");
    }
    if report.identity.sync_identity_state == "key_pending_rollout" {
        println!("Sync still uses legacy ULID actor IDs; this key is not yet its sync actor.");
    }
    println!("Identity is not a trust or access decision.");
    Ok(())
}

fn status_label(status: DeviceIdentityStatus) -> &'static str {
    match status {
        DeviceIdentityStatus::Uninitialized => "uninitialized",
        DeviceIdentityStatus::Ready => "ready",
        DeviceIdentityStatus::Degraded => "degraded",
        DeviceIdentityStatus::Legacy => "legacy ULID",
        DeviceIdentityStatus::Invalid => "invalid",
    }
}
