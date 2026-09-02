use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat, SyncNotificationsCommand};
use serde::Serialize;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use vulcan_daemon::notifications::{
    NotificationMutationAction, NotificationMutationReport, NotificationStore,
    NotificationSubscription, NotificationSubscriptionId,
};
use vulcan_daemon::registry::{WikiId, WikiRegistry};
use vulcan_event_relay::SubscriptionBundle;

const DEFAULT_LIVE_REF: &str = "refs/heads/__vulcan-sync/live";
const MAX_BUNDLE_BYTES: u64 = 512 * 1024;

pub(crate) fn handle_notifications_command(
    cli: &Cli,
    command: &SyncNotificationsCommand,
) -> Result<(), CliError> {
    let store = NotificationStore::user_default().map_err(CliError::operation)?;
    match command {
        SyncNotificationsCommand::Import {
            wiki,
            bundle,
            source,
            refs,
            dry_run,
        } => {
            let wiki = WikiId::parse(wiki).map_err(CliError::operation)?;
            let registration = WikiRegistry::user_default()
                .map_err(CliError::operation)?
                .show(&wiki)
                .map_err(CliError::operation)?
                .registration;
            let bundle = read_bundle(bundle)?;
            let refs = if refs.is_empty() {
                vec![DEFAULT_LIVE_REF.to_string()]
            } else {
                refs.clone()
            };
            let report = store
                .import(&registration, source.clone(), refs, bundle, *dry_run)
                .map_err(CliError::operation)?;
            print_mutation(cli.output, &report)
        }
        SyncNotificationsCommand::List { wiki, all: _ } => {
            let wiki = wiki
                .as_deref()
                .map(WikiId::parse)
                .transpose()
                .map_err(CliError::operation)?;
            let subscriptions = store.list(wiki.as_ref()).map_err(CliError::operation)?;
            print_list(cli.output, &subscriptions)
        }
        SyncNotificationsCommand::Show { subscription } => {
            let subscription = parse_id(subscription)?;
            let subscription = store.show(subscription).map_err(CliError::operation)?;
            print_subscription(cli.output, &subscription)
        }
        SyncNotificationsCommand::Remove {
            subscription,
            dry_run,
        } => {
            let report = store
                .remove(parse_id(subscription)?, *dry_run)
                .map_err(CliError::operation)?;
            print_mutation(cli.output, &report)
        }
        SyncNotificationsCommand::Test { subscription } => {
            let id = parse_id(subscription)?;
            let subscription = store.show(id).map_err(CliError::operation)?;
            let _credential = store.credential(id).map_err(CliError::operation)?;
            let report = NotificationTestReport {
                subscription,
                configuration_valid: true,
                transport_tested: false,
                detail: "configuration and credential are valid; transport testing requires the daemon listener"
                    .to_string(),
            };
            print_test(cli.output, &report)
        }
        SyncNotificationsCommand::Status { wiki, all: _ } => {
            let wiki = wiki
                .as_deref()
                .map(WikiId::parse)
                .transpose()
                .map_err(CliError::operation)?;
            let subscriptions = store.list(wiki.as_ref()).map_err(CliError::operation)?;
            let report = NotificationStatusReport {
                configured: subscriptions.len(),
                listening: false,
                state: "daemon_required",
                detail: "realtime listening is unavailable until the daemon notification runtime is configured"
                    .to_string(),
                subscriptions,
            };
            print_status(cli.output, &report)
        }
    }
}

fn parse_id(value: &str) -> Result<NotificationSubscriptionId, CliError> {
    NotificationSubscriptionId::parse(value).map_err(CliError::operation)
}

fn read_bundle(source: &str) -> Result<SubscriptionBundle, CliError> {
    let bytes = if source == "-" {
        let mut bytes = Vec::new();
        io::stdin()
            .lock()
            .take(MAX_BUNDLE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(CliError::operation)?;
        bytes
    } else {
        read_private_bundle_file(Path::new(source))?
    };
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(CliError::operation(format!(
            "subscription bundle exceeds the {MAX_BUNDLE_BYTES} byte limit"
        )));
    }
    serde_json::from_slice(&bytes).map_err(CliError::operation)
}

fn read_private_bundle_file(path: &Path) -> Result<Vec<u8>, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(CliError::operation)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_BUNDLE_BYTES
    {
        return Err(CliError::operation(format!(
            "subscription bundle at {} must be a regular file no larger than {MAX_BUNDLE_BYTES} bytes",
            path.display()
        )));
    }
    validate_private_permissions(&metadata, path)?;
    fs::read(path).map_err(CliError::operation)
}

#[cfg(unix)]
fn validate_private_permissions(metadata: &fs::Metadata, path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(CliError::operation(format!(
            "subscription bundle at {} is readable by group or other users; use mode 0600 or stdin",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_metadata: &fs::Metadata, _path: &Path) -> Result<(), CliError> {
    Ok(())
}

#[derive(Serialize)]
struct NotificationListReport<'a> {
    subscriptions: &'a [NotificationSubscription],
}

#[derive(Serialize)]
struct NotificationTestReport {
    subscription: NotificationSubscription,
    configuration_valid: bool,
    transport_tested: bool,
    detail: String,
}

#[derive(Serialize)]
struct NotificationStatusReport {
    configured: usize,
    listening: bool,
    state: &'static str,
    detail: String,
    subscriptions: Vec<NotificationSubscription>,
}

fn print_mutation(
    output: OutputFormat,
    report: &NotificationMutationReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let verb = match report.action {
        NotificationMutationAction::Import => "import",
        NotificationMutationAction::Remove => "remove",
    };
    let preview = if report.dry_run { "Would " } else { "" };
    println!(
        "{preview}{verb} notification {} for wiki {}",
        report.subscription.id, report.subscription.wiki_id
    );
    Ok(())
}

fn print_list(
    output: OutputFormat,
    subscriptions: &[NotificationSubscription],
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(&NotificationListReport { subscriptions });
    }
    if subscriptions.is_empty() {
        println!("No notification subscriptions configured.");
    } else {
        for subscription in subscriptions {
            println!(
                "{}  {}  {}  {}",
                subscription.id,
                subscription.wiki_id,
                subscription.source,
                subscription.refs.join(",")
            );
        }
    }
    Ok(())
}

fn print_subscription(
    output: OutputFormat,
    subscription: &NotificationSubscription,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(subscription);
    }
    println!("Notification: {}", subscription.id);
    println!("Wiki: {}", subscription.wiki_id);
    println!("Source: {}", subscription.source);
    println!("Refs: {}", subscription.refs.join(", "));
    println!("Credential: {}", subscription.credential_id);
    Ok(())
}

fn print_test(output: OutputFormat, report: &NotificationTestReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!("Notification {}: {}", report.subscription.id, report.detail);
    Ok(())
}

fn print_status(output: OutputFormat, report: &NotificationStatusReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Notifications: {} configured; {}",
        report.configured, report.detail
    );
    Ok(())
}
