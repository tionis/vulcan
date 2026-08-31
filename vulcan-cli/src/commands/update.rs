use crate::output::print_json;
#[cfg(feature = "web")]
use crate::{build_update_channel, build_version, UpdateChannelArg};
use crate::{Cli, CliError, OutputFormat, UpdateChannelArgs, UpdateCommand};
#[cfg(feature = "web")]
use base64::engine::general_purpose::STANDARD as BASE64;
#[cfg(feature = "web")]
use base64::Engine as _;
use vulcan_app::update::UpdateCheckReport;
#[cfg(feature = "web")]
use vulcan_app::update::{
    apply_prepared_update, check_for_update, prepare_update, TrustedUpdateKey, UpdateCheckRequest,
};

#[cfg(feature = "web")]
const STABLE_CHANNEL_URL: &str =
    "https://github.com/tionis/vulcan/releases/latest/download/vulcan-update-channel.json";
#[cfg(feature = "web")]
const MAIN_CHANNEL_URL: &str =
    "https://github.com/tionis/vulcan/releases/download/main/vulcan-update-channel.json";

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
    }
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
    let Some(encoded) = option_env!("VULCAN_UPDATE_PUBLIC_KEY") else {
        return Ok(Vec::new());
    };
    let key_id = option_env!("VULCAN_UPDATE_KEY_ID").ok_or_else(|| {
        CliError::operation("build embeds an update public key without VULCAN_UPDATE_KEY_ID")
    })?;
    let decoded = BASE64.decode(encoded).map_err(|error| {
        CliError::operation(format!("invalid embedded update public key: {error}"))
    })?;
    let public_key: [u8; 32] = decoded.try_into().map_err(|_| {
        CliError::operation("embedded Ed25519 update public key must contain exactly 32 bytes")
    })?;
    Ok(vec![TrustedUpdateKey {
        key_id: key_id.to_string(),
        public_key,
    }])
}

#[cfg(feature = "web")]
fn current_target() -> Result<&'static str, CliError> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok("aarch64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("x86_64-pc-windows-msvc")
    } else {
        Err(CliError::operation(
            "this platform does not have a portable Vulcan update artifact",
        ))
    }
}
