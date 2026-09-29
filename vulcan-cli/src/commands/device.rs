use crate::cli::DeviceCommand;
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use serde::Serialize;
use vulcan_app::device_identity::{
    DeviceIdentityInitReport, DeviceIdentityRepairReport, DeviceIdentityReport,
    DeviceIdentityStatus, DeviceIdentityStore,
};

#[derive(Serialize)]
struct PublicKeyReport {
    version: u32,
    public_key: String,
}

pub(crate) fn handle_device_command(cli: &Cli, command: &DeviceCommand) -> Result<(), CliError> {
    let store = DeviceIdentityStore::user_default().map_err(CliError::operation)?;
    match command {
        DeviceCommand::Show => {
            let report = store.inspect();
            print_device_show(cli.output, &report)
        }
        DeviceCommand::Init { dry_run } => {
            let report = store.initialize(*dry_run).map_err(CliError::operation)?;
            print_device_init(cli.output, &report)
        }
        DeviceCommand::RepairPermissions { dry_run } => {
            let report = store
                .repair_permissions(*dry_run)
                .map_err(CliError::operation)?;
            print_device_repair(cli.output, &report)
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

fn print_device_show(output: OutputFormat, report: &DeviceIdentityReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!("Local device identity: {}", status_label(report.status));
    if let Some(id) = &report.device_id {
        println!("Device ID: {id}");
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
    println!("Identity is not a trust or access decision.");
    Ok(())
}

fn print_device_repair(
    output: OutputFormat,
    report: &DeviceIdentityRepairReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.repaired.is_empty() {
        println!("Device identity storage is already private to the current user.");
    } else {
        let verb = if report.dry_run {
            "Would restrict"
        } else {
            "Restricted"
        };
        println!(
            "{verb} access to the current user for: {}",
            report.repaired.join(", ")
        );
    }
    println!(
        "Local device identity: {}",
        status_label(report.identity.status)
    );
    if let Some(diagnostic) = &report.identity.diagnostic {
        println!("{diagnostic}");
    }
    Ok(())
}

fn status_label(status: DeviceIdentityStatus) -> &'static str {
    match status {
        DeviceIdentityStatus::Uninitialized => "uninitialized",
        DeviceIdentityStatus::Ready => "ready",
        DeviceIdentityStatus::Degraded => "degraded",
        DeviceIdentityStatus::Invalid => "invalid",
    }
}
