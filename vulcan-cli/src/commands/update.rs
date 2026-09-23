#[cfg(feature = "web")]
use crate::build_version;
use crate::output::print_json;
use crate::{
    build_update_channel, Cli, CliError, OutputFormat, UpdateChannelArg, UpdateChannelArgs,
    UpdateCommand, UpdateNetworkArg, UpdatePolicyArgs, UpdateScheduleCommand,
};
#[cfg(feature = "web")]
use base64::engine::general_purpose::STANDARD as BASE64;
#[cfg(feature = "web")]
use base64::Engine as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
#[cfg(feature = "web")]
use vulcan_app::background_policy::{
    evaluate, probe_conditions, BackgroundPolicy, NetworkRequirement, PowerRequirement,
    UnknownBehavior,
};
use vulcan_app::background_policy::{BackgroundConditions, DeferReason};
use vulcan_app::update::UpdateCheckReport;
#[cfg(feature = "web")]
use vulcan_app::update::{
    apply_prepared_update, check_for_update, prepare_update, TrustedUpdateKey, UpdateCheckRequest,
};
use vulcan_daemon::process::DaemonProcessContext;
#[cfg(feature = "web")]
use vulcan_daemon::process::{daemon_status, request_daemon_shutdown};
#[cfg(feature = "web")]
use vulcan_daemon::service::{
    inspect_daemon_service, plan_daemon_service, DaemonServiceAction, DaemonServicePlatform,
    DaemonServiceUser,
};
use vulcan_daemon::update_schedule::{
    apply_update_schedule, load_update_schedule, plan_update_schedule, UpdateScheduleAction,
    UpdateScheduleNetwork, UpdateScheduleOptions, UpdateSchedulePlatform, UpdateScheduleReport,
    UpdateScheduleUnknown,
};

#[cfg(feature = "web")]
const STABLE_CHANNEL_URL: &str =
    "https://github.com/tionis/vulcan/releases/latest/download/vulcan-update-channel.json";
#[cfg(feature = "web")]
const MAIN_CHANNEL_URL: &str =
    "https://github.com/tionis/vulcan/releases/download/rolling-main/vulcan-update-channel.json";
#[cfg(feature = "web")]
const TRUSTED_UPDATE_KEYS: &[(&str, &str, &str)] = &[
    (
        "stable-2026-09",
        "stable",
        "sOrBt76ruZ2kSR+4glX9k/ZjSoS1YSvmK9yMSVCiWpE=",
    ),
    (
        "main-2026-09",
        "main",
        "6gbtjy5nGZoT8kFAfYELB5x73S34kjv+/tPn8XEjrg0=",
    ),
];

pub(crate) fn handle_update_command(
    cli: &Cli,
    command: Option<&UpdateCommand>,
) -> Result<(), CliError> {
    match command {
        None | Some(UpdateCommand::Check { .. }) => {
            let default = UpdateChannelArgs {
                channel: None,
                channel_url: None,
                allow_unsigned: false,
            };
            let options = match command {
                Some(UpdateCommand::Check { channel }) => channel,
                _ => &default,
            };
            let report = run_check(options)?;
            print_check(cli.output, &report)
        }
        Some(UpdateCommand::Apply {
            channel,
            dry_run,
            allow_downgrade,
        }) => {
            let report = run_check(channel)?;
            if !report.update_available && !allow_downgrade {
                return print_check(cli.output, &report);
            }
            #[cfg(feature = "web")]
            {
                let source = vulcan_app::update::HttpUpdateSource::new()?;
                let prepared = prepare_update(&source, report, *allow_downgrade)?;
                let executable = std::env::current_exe().map_err(CliError::operation)?;
                let applied = apply_prepared_update(&prepared, &executable, *dry_run)?;
                if cli.output == OutputFormat::Json {
                    print_json(&applied)
                } else {
                    println!(
                        "{} Vulcan {} -> {} from the `{}` channel at {}",
                        if applied.dry_run {
                            "Would update"
                        } else {
                            "Updated"
                        },
                        applied.previous_version,
                        applied.installed_version,
                        applied.channel,
                        applied.executable
                    );
                    if !applied.signature_verified {
                        println!("Warning: channel metadata was not signed by a trusted key.");
                    }
                    if let Some(backup) = &applied.retained_backup {
                        println!(
                            "The running platform retained the previous executable at {backup}."
                        );
                    }
                    if !applied.dry_run {
                        println!("Restart any running Vulcan daemon to use the new binary.");
                    }
                    Ok(())
                }
            }
            #[cfg(not(feature = "web"))]
            {
                let _ = (dry_run, allow_downgrade);
                Err(CliError::operation(
                    "the `self-update apply` command requires a build with the `web` feature enabled",
                ))
            }
        }
        Some(UpdateCommand::Schedule { command }) => handle_schedule(cli, command),
        Some(UpdateCommand::Run {
            channel,
            policy,
            notify_on_failure,
        }) => match run_unattended_update(channel, policy) {
            Ok(report) => print_unattended_report(cli.output, &report),
            Err(error) => {
                if *notify_on_failure {
                    let _ = notify_update_failure(&error.to_string());
                }
                Err(error)
            }
        },
    }
}

#[derive(Debug, serde::Serialize)]
struct UnattendedUpdateReport {
    action: &'static str,
    update_available: bool,
    daemon_was_running: bool,
    daemon_restored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<UpdateCheckReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deferred: Option<DeferReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conditions: Option<BackgroundConditions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied: Option<vulcan_app::update::UpdateApplyReport>,
}

fn handle_schedule(cli: &Cli, command: &UpdateScheduleCommand) -> Result<(), CliError> {
    let context = DaemonProcessContext::user_default().map_err(CliError::operation)?;
    match command {
        UpdateScheduleCommand::Show => {
            let plan = load_update_schedule(&context.state_root).map_err(CliError::operation)?;
            if cli.output == OutputFormat::Json {
                print_json(&plan)
            } else if let Some(plan) = plan {
                println!(
                    "Scheduled {:?} updates on `{}`.",
                    plan.platform, plan.options.channel
                );
                if plan.platform == UpdateSchedulePlatform::TermuxJob {
                    println!(
                        "Approximate interval: {} hours",
                        plan.options.android_period_hours
                    );
                } else {
                    println!("Daily local time: {}", plan.options.daily_at);
                }
                print_policy(&plan.options);
                println!(
                    "Failure notifications: {}",
                    if plan.options.notify_on_failure {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                Ok(())
            } else {
                println!("No unattended Vulcan update schedule is installed.");
                Ok(())
            }
        }
        UpdateScheduleCommand::Install {
            channel,
            policy,
            at,
            android_period_hours,
            notify_on_failure,
            dry_run,
        } => {
            if channel.allow_unsigned {
                return Err(CliError::operation(
                    "unattended updates cannot weaken signature verification with --allow-unsigned",
                ));
            }
            let selected_channel = channel
                .channel
                .map_or_else(build_update_channel, UpdateChannelArg::as_str);
            let options = UpdateScheduleOptions {
                daily_at: at.clone(),
                android_period_hours: *android_period_hours,
                channel: selected_channel.to_string(),
                channel_url: channel.channel_url.clone(),
                notify_on_failure: *notify_on_failure,
                network: match policy.network.unwrap_or(UpdateNetworkArg::Unmetered) {
                    UpdateNetworkArg::Any => UpdateScheduleNetwork::Any,
                    UpdateNetworkArg::Unmetered => UpdateScheduleNetwork::Unmetered,
                },
                battery_not_low: !policy.allow_low_battery,
                charging: policy.require_charging,
                unknown: if policy.defer_on_unknown {
                    UpdateScheduleUnknown::Defer
                } else {
                    UpdateScheduleUnknown::Allow
                },
            };
            manage_schedule(
                cli.output,
                &context,
                UpdateScheduleAction::Install,
                options,
                *dry_run,
            )
        }
        UpdateScheduleCommand::Uninstall { dry_run } => {
            let options = load_update_schedule(&context.state_root)
                .map_err(CliError::operation)?
                .map_or_else(UpdateScheduleOptions::default, |plan| plan.options);
            manage_schedule(
                cli.output,
                &context,
                UpdateScheduleAction::Uninstall,
                options,
                *dry_run,
            )
        }
    }
}

fn manage_schedule(
    output: OutputFormat,
    context: &DaemonProcessContext,
    action: UpdateScheduleAction,
    options: UpdateScheduleOptions,
    dry_run: bool,
) -> Result<(), CliError> {
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let config_directory = context.registry.path().parent().ok_or_else(|| {
        CliError::operation("daemon registry path has no configuration directory")
    })?;
    let platform = UpdateSchedulePlatform::native().map_err(CliError::operation)?;
    let plan = plan_update_schedule(
        action,
        platform,
        &executable,
        config_directory,
        &context.state_root,
        &service_home_directory()?,
        service_windows_user_sid()?.as_deref(),
        service_user_id()?,
        options,
    )
    .map_err(CliError::operation)?;
    let report = apply_update_schedule(plan, dry_run).map_err(CliError::operation)?;
    print_schedule_report(output, &report)
}

fn print_schedule_report(
    output: OutputFormat,
    report: &UpdateScheduleReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let verb = match report.plan.action {
        UpdateScheduleAction::Install => "install",
        UpdateScheduleAction::Uninstall => "remove",
    };
    if report.dry_run {
        println!(
            "Would {verb} the {:?} unattended update job.",
            report.plan.platform
        );
    } else {
        println!(
            "Completed {:?} unattended update schedule {verb}.",
            report.plan.platform
        );
    }
    if report.plan.platform == UpdateSchedulePlatform::TermuxJob {
        println!(
            "Approximate interval: {} hours",
            report.plan.options.android_period_hours
        );
    } else {
        println!("Daily local time: {}", report.plan.options.daily_at);
    }
    if report.plan.action == UpdateScheduleAction::Install {
        print_policy(&report.plan.options);
    }
    Ok(())
}

fn print_policy(options: &UpdateScheduleOptions) {
    let network = match options.network {
        UpdateScheduleNetwork::Any => "any",
        UpdateScheduleNetwork::Unmetered => "unmetered",
    };
    let unknown = match options.unknown {
        UpdateScheduleUnknown::Allow => "allow",
        UpdateScheduleUnknown::Defer => "defer",
    };
    println!(
        "Background policy: network {network}, battery not low {}, external power {}, unknown readings {unknown}.",
        options.battery_not_low, options.charging
    );
}

#[cfg(feature = "web")]
fn run_unattended_update(
    options: &UpdateChannelArgs,
    policy_args: &UpdatePolicyArgs,
) -> Result<UnattendedUpdateReport, CliError> {
    let policy = BackgroundPolicy {
        // Old installed jobs omit --network and retain their original `any` policy.
        network: match policy_args.network.unwrap_or(UpdateNetworkArg::Any) {
            UpdateNetworkArg::Any => NetworkRequirement::Any,
            UpdateNetworkArg::Unmetered => NetworkRequirement::Unmetered,
        },
        power: if policy_args.require_charging && !policy_args.allow_low_battery {
            PowerRequirement::BatteryNotLowAndCharging
        } else if policy_args.require_charging {
            PowerRequirement::Charging
        } else if policy_args.allow_low_battery {
            PowerRequirement::Any
        } else {
            PowerRequirement::BatteryNotLow
        },
        unknown: if policy_args.defer_on_unknown {
            UnknownBehavior::Defer
        } else {
            UnknownBehavior::Allow
        },
    };
    let conditions = probe_conditions();
    if let Some(reason) = evaluate(&policy, &conditions) {
        return Ok(deferred_update(None, reason, conditions));
    }
    let check = run_check(options)?;
    if !check.update_available {
        return Ok(UnattendedUpdateReport {
            action: "scheduled_update",
            update_available: false,
            daemon_was_running: false,
            daemon_restored: false,
            check: Some(check),
            deferred: None,
            conditions: Some(conditions),
            applied: None,
        });
    }
    let conditions = probe_conditions();
    if let Some(reason) = evaluate(&policy, &conditions) {
        return Ok(deferred_update(Some(check), reason, conditions));
    }
    let source = vulcan_app::update::HttpUpdateSource::new()?;
    let prepared = prepare_update(&source, check.clone(), false)?;
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let context = DaemonProcessContext::user_default().map_err(CliError::operation)?;
    let daemon = daemon_status(&context).map_err(CliError::operation)?;
    if !daemon.running && daemon.runtime.is_some() {
        return Err(CliError::operation(format!(
            "refusing to replace the binary while the daemon runtime is unresponsive{}",
            daemon
                .capability_probe_error
                .as_deref()
                .map_or(String::new(), |error| format!(": {error}"))
        )));
    }
    let daemon_was_running = daemon.running;
    let service_installed = daemon_service_is_installed(&context, &executable)?;
    let (applied, daemon_restored) = coordinate_daemon_replacement(
        daemon_was_running,
        || {
            let stopped = request_daemon_shutdown(&context).map_err(CliError::operation)?;
            if stopped.running {
                return Err(CliError::operation(
                    "Vulcan daemon did not stop before binary replacement",
                ));
            }
            Ok(())
        },
        || apply_prepared_update(&prepared, &executable, false).map_err(Into::into),
        || restore_daemon(&executable, service_installed),
    )?;
    Ok(UnattendedUpdateReport {
        action: "scheduled_update",
        update_available: true,
        daemon_was_running,
        daemon_restored,
        check: Some(prepared.check),
        deferred: None,
        conditions: Some(conditions),
        applied: Some(applied),
    })
}

#[cfg(feature = "web")]
fn deferred_update(
    check: Option<UpdateCheckReport>,
    reason: DeferReason,
    conditions: BackgroundConditions,
) -> UnattendedUpdateReport {
    UnattendedUpdateReport {
        action: "scheduled_update",
        update_available: check.as_ref().is_some_and(|check| check.update_available),
        daemon_was_running: false,
        daemon_restored: false,
        check,
        deferred: Some(reason),
        conditions: Some(conditions),
        applied: None,
    }
}

fn coordinate_daemon_replacement<T, E>(
    daemon_running: bool,
    mut stop: impl FnMut() -> Result<(), E>,
    mut apply: impl FnMut() -> Result<T, E>,
    mut restore: impl FnMut() -> Result<(), E>,
) -> Result<(T, bool), E> {
    if daemon_running {
        stop()?;
    }
    let applied = match apply() {
        Ok(applied) => applied,
        Err(error) => {
            if daemon_running {
                let _ = restore();
            }
            return Err(error);
        }
    };
    if daemon_running {
        restore()?;
    }
    Ok((applied, daemon_running))
}

#[cfg(not(feature = "web"))]
fn run_unattended_update(
    _options: &UpdateChannelArgs,
    _policy: &UpdatePolicyArgs,
) -> Result<UnattendedUpdateReport, CliError> {
    Err(CliError::operation(
        "the `self-update run` command requires a build with the `web` feature enabled",
    ))
}

fn print_unattended_report(
    output: OutputFormat,
    report: &UnattendedUpdateReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if let Some(reason) = &report.deferred {
        let detail = match reason {
            DeferReason::MeteredNetwork => "the network is metered",
            DeferReason::LowBattery => "the battery is low",
            DeferReason::NotCharging => "external power is unavailable",
            DeferReason::UnknownNetwork => "the network cost is unknown",
            DeferReason::UnknownPower => "the power state is unknown",
        };
        println!("Deferred unattended update: {detail}.");
        return Ok(());
    }
    if let Some(applied) = &report.applied {
        println!(
            "Updated Vulcan {} -> {}.",
            applied.previous_version, applied.installed_version
        );
    } else {
        println!(
            "Vulcan {} is already current.",
            report
                .check
                .as_ref()
                .map_or("unknown", |check| check.current_version.as_str())
        );
    }
    if report.daemon_restored {
        println!("Restored the daemon after the update.");
    }
    Ok(())
}

fn daemon_service_is_installed(
    context: &DaemonProcessContext,
    executable: &std::path::Path,
) -> Result<bool, CliError> {
    let Ok(platform) = DaemonServicePlatform::native() else {
        return Ok(false);
    };
    let config_directory = context.registry.path().parent().ok_or_else(|| {
        CliError::operation("daemon registry path has no configuration directory")
    })?;
    let user = match service_windows_user_sid()? {
        Some(sid) => DaemonServiceUser::windows(sid),
        None => DaemonServiceUser::unix(service_user_id()?),
    };
    let plan = plan_daemon_service(
        DaemonServiceAction::Install,
        platform,
        executable,
        config_directory,
        &context.state_root,
        &service_home_directory()?,
        &user,
    )
    .map_err(CliError::operation)?;
    Ok(inspect_daemon_service(&plan)
        .map_err(CliError::operation)?
        .is_some_and(|service| service.installed))
}

fn restore_daemon(executable: &std::path::Path, service_installed: bool) -> Result<(), CliError> {
    let mut command = Command::new(executable);
    if service_installed {
        command.args(["daemon", "install"]);
    } else {
        command.args(["daemon", "start", "--detach"]);
    }
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(CliError::operation)?;
    if output.status.success() {
        return Ok(());
    }
    Err(CliError::operation(format!(
        "updated Vulcan but failed to restore the daemon: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn notify_update_failure(detail: &str) -> Result<(), CliError> {
    let detail = format!(
        "Vulcan automatic update failed: {}",
        detail.chars().take(500).collect::<String>()
    );
    #[cfg(target_os = "linux")]
    let mut command = if std::env::var_os("PREFIX").is_some() {
        let mut c = Command::new("termux-notification");
        c.args([
            "--group",
            "vulcan-update",
            "--priority",
            "high",
            "--title",
            "Vulcan update failed",
            "--content",
            &detail,
        ]);
        c
    } else {
        let mut c = Command::new("notify-send");
        c.args(["Vulcan update failed", &detail]);
        c
    };
    #[cfg(target_os = "android")]
    let mut command = {
        let mut c = Command::new("termux-notification");
        c.args([
            "--group",
            "vulcan-update",
            "--priority",
            "high",
            "--title",
            "Vulcan update failed",
            "--content",
            &detail,
        ]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("osascript");
        c.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 1 of argv) with title \"Vulcan update failed\"",
            "-e",
            "end run",
            "--",
            &detail,
        ]);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = Command::new("msg.exe");
        c.args(["*", &detail]);
        c
    };
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "windows"
    )))]
    return Err(CliError::operation(
        "native failure notifications are unsupported on this platform",
    ));
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(CliError::operation)?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::operation(
            "native failure notification helper failed",
        ))
    }
}

fn service_home_directory() -> Result<PathBuf, CliError> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| CliError::operation("cannot determine the user home directory"))
}

#[cfg(target_os = "macos")]
fn service_user_id() -> Result<u32, CliError> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(CliError::operation)?;
    if !output.status.success() {
        return Err(CliError::operation(
            "failed to determine the logged-in macOS user id",
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(CliError::operation)?
        .trim()
        .parse()
        .map_err(CliError::operation)
}
#[cfg(not(target_os = "macos"))]
#[allow(clippy::unnecessary_wraps)]
fn service_user_id() -> Result<u32, CliError> {
    Ok(0)
}

#[cfg(windows)]
fn service_windows_user_sid() -> Result<Option<String>, CliError> {
    let output = Command::new("whoami.exe")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .map_err(CliError::operation)?;
    let stdout = String::from_utf8(output.stdout).map_err(CliError::operation)?;
    let start = stdout
        .find("S-1-")
        .ok_or_else(|| CliError::operation("whoami did not report a user SID"))?;
    Ok(Some(
        stdout[start..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == 'S')
            .collect(),
    ))
}
#[cfg(not(windows))]
#[allow(clippy::unnecessary_wraps)]
fn service_windows_user_sid() -> Result<Option<String>, CliError> {
    Ok(None)
}

fn run_check(options: &UpdateChannelArgs) -> Result<UpdateCheckReport, CliError> {
    #[cfg(feature = "web")]
    {
        let channel = options
            .channel
            .map_or_else(build_update_channel, UpdateChannelArg::as_str);
        let default_url = match channel {
            "stable" => STABLE_CHANNEL_URL,
            "main" => MAIN_CHANNEL_URL,
            _ => {
                return Err(CliError::operation(format!(
                    "binary was built with unsupported update channel `{channel}`"
                )))
            }
        };
        let trusted_keys = trusted_update_keys()?;
        let source = vulcan_app::update::HttpUpdateSource::new()?;
        check_for_update(
            &source,
            &UpdateCheckRequest {
                channel_url: options.channel_url.as_deref().unwrap_or(default_url),
                expected_channel: channel,
                current_version: build_version(),
                target: current_target()?,
                require_signature: !options.allow_unsigned,
                trusted_keys: &trusted_keys,
            },
        )
        .map_err(Into::into)
    }
    #[cfg(not(feature = "web"))]
    {
        let _ = options;
        Err(CliError::operation(
            "the `self-update` command requires a build with the `web` feature enabled",
        ))
    }
}

fn print_check(output: OutputFormat, report: &UpdateCheckReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    if report.update_available {
        println!(
            "Vulcan {} is available on `{}` (installed: {}, target: {}).",
            report.available_version, report.channel, report.current_version, report.target
        );
    } else {
        println!(
            "Vulcan {} is up to date for `{}` (channel version: {}).",
            report.current_version, report.channel, report.available_version
        );
    }
    println!(
        "Metadata trust: {}.",
        if report.signature_verified {
            format!(
                "verified signature from {}",
                report.verified_key_id.as_deref().unwrap_or("trusted key")
            )
        } else {
            "checksum-only (explicitly allowed)".to_string()
        }
    );
    Ok(())
}

#[cfg(feature = "web")]
fn trusted_update_keys() -> Result<Vec<TrustedUpdateKey>, CliError> {
    TRUSTED_UPDATE_KEYS
        .iter()
        .map(|(key_id, channel, encoded)| {
            let decoded = BASE64.decode(encoded).map_err(|error| {
                CliError::operation(format!("invalid embedded update public key: {error}"))
            })?;
            let public_key: [u8; 32] = decoded.try_into().map_err(|_| {
                CliError::operation(
                    "embedded Ed25519 update public key must contain exactly 32 bytes",
                )
            })?;
            Ok(TrustedUpdateKey {
                key_id: (*key_id).to_string(),
                channel: (*channel).to_string(),
                public_key,
            })
        })
        .collect()
}

#[cfg(test)]
mod coordination_tests {
    use super::coordinate_daemon_replacement;
    use std::cell::RefCell;

    #[test]
    fn daemon_is_stopped_only_around_replacement_and_restored_after_success() {
        let events = RefCell::new(Vec::new());
        let (value, restored) = coordinate_daemon_replacement(
            true,
            || {
                events.borrow_mut().push("stop");
                Ok::<_, &'static str>(())
            },
            || {
                events.borrow_mut().push("apply");
                Ok::<_, &'static str>(42)
            },
            || {
                events.borrow_mut().push("restore");
                Ok::<_, &'static str>(())
            },
        )
        .expect("coordinated replacement");
        assert_eq!(value, 42);
        assert!(restored);
        assert_eq!(*events.borrow(), ["stop", "apply", "restore"]);
    }

    #[test]
    fn replacement_failure_still_attempts_daemon_restore() {
        let events = RefCell::new(Vec::new());
        let error = coordinate_daemon_replacement(
            true,
            || {
                events.borrow_mut().push("stop");
                Ok(())
            },
            || {
                events.borrow_mut().push("apply");
                Err::<(), _>("replacement failed")
            },
            || {
                events.borrow_mut().push("restore");
                Ok(())
            },
        )
        .expect_err("replacement must fail");
        assert_eq!(error, "replacement failed");
        assert_eq!(*events.borrow(), ["stop", "apply", "restore"]);
    }

    #[test]
    fn stopped_daemon_is_not_started_by_an_update() {
        let events = RefCell::new(Vec::new());
        let ((), restored) = coordinate_daemon_replacement(
            false,
            || {
                events.borrow_mut().push("stop");
                Ok::<_, &'static str>(())
            },
            || {
                events.borrow_mut().push("apply");
                Ok::<_, &'static str>(())
            },
            || {
                events.borrow_mut().push("restore");
                Ok::<_, &'static str>(())
            },
        )
        .expect("replacement");
        assert!(!restored);
        assert_eq!(*events.borrow(), ["apply"]);
    }
}

#[cfg(all(test, feature = "web"))]
mod tests {
    use super::{deferred_update, release_target, trusted_update_keys, MAIN_CHANNEL_URL};
    use vulcan_app::background_policy::{BackgroundConditions, DeferReason};

    #[test]
    fn deferred_cycle_is_successful_and_reports_why_no_check_was_made() {
        let report = deferred_update(
            None,
            DeferReason::MeteredNetwork,
            BackgroundConditions {
                metered: Some(true),
                battery_low: None,
                charging: None,
            },
        );
        let json = serde_json::to_value(report).expect("serializable update report");
        assert_eq!(json["action"], "scheduled_update");
        assert_eq!(json["deferred"], "metered_network");
        assert_eq!(json["conditions"]["metered"], true);
        assert!(json.get("check").is_none());
        assert!(json.get("applied").is_none());
    }

    #[test]
    fn main_channel_uses_a_tag_distinct_from_the_main_branch() {
        assert_eq!(
            MAIN_CHANNEL_URL,
            "https://github.com/tionis/vulcan/releases/download/rolling-main/vulcan-update-channel.json"
        );
    }

    #[test]
    fn embedded_update_keys_are_scoped_to_independent_channels() {
        let keys = trusted_update_keys().expect("decode trusted update keys");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].key_id, "stable-2026-09");
        assert_eq!(keys[0].channel, "stable");
        assert_eq!(keys[1].key_id, "main-2026-09");
        assert_eq!(keys[1].channel, "main");
        assert!(keys.iter().all(|key| key.public_key.len() == 32));
        assert_ne!(keys[0].public_key, keys[1].public_key);
    }

    #[test]
    fn android_uses_the_bionic_release_artifact() {
        assert_eq!(
            release_target("android", "aarch64"),
            Some("aarch64-linux-android")
        );
        assert_eq!(release_target("android", "x86_64"), None);
    }
}

#[cfg(feature = "web")]
fn current_target() -> Result<&'static str, CliError> {
    release_target(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        CliError::operation("this platform does not have a portable Vulcan update artifact")
    })
}

#[cfg(feature = "web")]
fn release_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("android", "aarch64") => Some("aarch64-linux-android"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}
