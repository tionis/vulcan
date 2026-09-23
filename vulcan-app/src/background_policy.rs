//! Portable policy evaluation and best-effort host probes for background work.
//!
//! Probes deliberately return `None` when the host cannot provide a reliable
//! answer. Callers choose whether unknown values permit or defer work through
//! [`UnknownBehavior`].

use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRequirement {
    #[default]
    Any,
    Unmetered,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerRequirement {
    #[default]
    Any,
    BatteryNotLow,
    Charging,
    BatteryNotLowAndCharging,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownBehavior {
    #[default]
    Allow,
    Defer,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackgroundPolicy {
    pub network: NetworkRequirement,
    pub power: PowerRequirement,
    pub unknown: UnknownBehavior,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackgroundConditions {
    pub metered: Option<bool>,
    pub battery_low: Option<bool>,
    pub charging: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferReason {
    MeteredNetwork,
    LowBattery,
    NotCharging,
    UnknownNetwork,
    UnknownPower,
}

/// Return the first unmet requirement, if this work should be deferred.
#[must_use]
pub fn evaluate(
    policy: &BackgroundPolicy,
    conditions: &BackgroundConditions,
) -> Option<DeferReason> {
    if policy.network == NetworkRequirement::Unmetered {
        match conditions.metered {
            Some(true) => return Some(DeferReason::MeteredNetwork),
            None if policy.unknown == UnknownBehavior::Defer => {
                return Some(DeferReason::UnknownNetwork)
            }
            Some(false) | None => {}
        }
    }

    match policy.power {
        PowerRequirement::Any => None,
        PowerRequirement::BatteryNotLow => match conditions.battery_low {
            Some(true) => Some(DeferReason::LowBattery),
            None if policy.unknown == UnknownBehavior::Defer => Some(DeferReason::UnknownPower),
            Some(false) | None => None,
        },
        PowerRequirement::Charging => match conditions.charging {
            Some(false) => Some(DeferReason::NotCharging),
            None if policy.unknown == UnknownBehavior::Defer => Some(DeferReason::UnknownPower),
            Some(true) | None => None,
        },
        PowerRequirement::BatteryNotLowAndCharging => {
            match conditions.battery_low {
                Some(true) => return Some(DeferReason::LowBattery),
                None if policy.unknown == UnknownBehavior::Defer => {
                    return Some(DeferReason::UnknownPower)
                }
                _ => {}
            }
            match conditions.charging {
                Some(false) => Some(DeferReason::NotCharging),
                None if policy.unknown == UnknownBehavior::Defer => Some(DeferReason::UnknownPower),
                Some(true) | None => None,
            }
        }
    }
}

/// Probe host conditions without requiring privileges or waiting indefinitely.
/// Unsupported platforms and unavailable services produce unknown values.
#[must_use]
pub fn probe_conditions() -> BackgroundConditions {
    let mut conditions = BackgroundConditions::default();
    #[cfg(target_os = "linux")]
    {
        conditions.metered = probe_networkmanager_metered();
        let (battery_low, charging) = probe_linux_battery();
        conditions.battery_low = battery_low;
        conditions.charging = charging;
    }
    #[cfg(target_os = "macos")]
    {
        let (battery_low, charging) = probe_macos_power();
        conditions.battery_low = battery_low;
        conditions.charging = charging;
    }
    #[cfg(target_os = "windows")]
    {
        let (battery_low, charging) = probe_windows_power();
        conditions.battery_low = battery_low;
        conditions.charging = charging;
    }
    conditions
}

#[cfg(target_os = "linux")]
fn probe_networkmanager_metered() -> Option<bool> {
    // Query NetworkManager's global property so disconnected adapters do not
    // influence the active connection state. busctl is optional.
    let output = run_bounded(
        "busctl",
        &[
            "--system",
            "get-property",
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
            "Metered",
        ],
    )?;
    parse_networkmanager_metered(&String::from_utf8_lossy(&output))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run_bounded(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    use std::{
        io::Read,
        process::{Command, Stdio},
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Windows PowerShell startup can take longer than a D-Bus or pmset query.
    #[cfg(target_os = "windows")]
    let timeout = Duration::from_secs(3);
    #[cfg(not(target_os = "windows"))]
    let timeout = Duration::from_millis(350);
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut bytes = Vec::new();
                child
                    .stdout
                    .take()?
                    .take(16 * 1024)
                    .read_to_end(&mut bytes)
                    .ok()?;
                return Some(bytes);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn probe_macos_power() -> (Option<bool>, Option<bool>) {
    let output = run_bounded("pmset", &["-g", "batt"]);
    output
        .as_deref()
        .map(|output| parse_pmset_battery(&String::from_utf8_lossy(output)))
        .unwrap_or((None, None))
}

#[cfg(target_os = "macos")]
fn parse_pmset_battery(text: &str) -> (Option<bool>, Option<bool>) {
    let external_power = text.lines().find_map(|line| {
        if line.contains("AC Power") {
            Some(true)
        } else if line.contains("Battery Power") {
            Some(false)
        } else {
            None
        }
    });
    let battery_lines = text
        .lines()
        .filter(|line| line.contains('%'))
        .collect::<Vec<_>>();
    if battery_lines.is_empty() {
        return (None, external_power);
    }

    let capacities = battery_lines
        .iter()
        .map(|line| {
            line.split('%')
                .next()?
                .rsplit(|character: char| !character.is_ascii_digit())
                .next()?
                .parse::<u8>()
                .ok()
        })
        .collect::<Option<Vec<_>>>();
    let battery_low = capacities
        .as_ref()
        .map(|values| values.iter().any(|capacity| *capacity <= 20));

    (battery_low, external_power)
}

#[cfg(target_os = "windows")]
fn probe_windows_power() -> (Option<bool>, Option<bool>) {
    const SCRIPT: &str = "Get-CimInstance -ClassName Win32_Battery | Select-Object BatteryStatus,EstimatedChargeRemaining | ConvertTo-Csv -NoTypeInformation";
    let output = run_bounded(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", SCRIPT],
    );
    output
        .as_deref()
        .map(|output| parse_windows_battery_csv(&String::from_utf8_lossy(output)))
        .unwrap_or((None, None))
}

#[cfg(target_os = "windows")]
fn parse_windows_battery_csv(text: &str) -> (Option<bool>, Option<bool>) {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(header) = lines.next() else {
        return (None, None);
    };
    let columns = header
        .split(',')
        .map(|column| column.trim_matches('"').to_ascii_lowercase())
        .collect::<Vec<_>>();
    let Some(status_column) = columns.iter().position(|column| column == "batterystatus") else {
        return (None, None);
    };
    let Some(capacity_column) = columns
        .iter()
        .position(|column| column == "estimatedchargeremaining")
    else {
        return (None, None);
    };

    let mut rows = Vec::new();
    for line in lines {
        let fields = line
            .split(',')
            .map(|field| field.trim_matches('"').trim())
            .collect::<Vec<_>>();
        let (Some(status), Some(capacity)) = (
            fields
                .get(status_column)
                .and_then(|field| field.parse::<u8>().ok()),
            fields
                .get(capacity_column)
                .and_then(|field| field.parse::<u8>().ok()),
        ) else {
            return (None, None);
        };
        rows.push((status, capacity));
    }
    if rows.is_empty() {
        return (None, None);
    }

    let battery_low = Some(
        rows.iter()
            .any(|(status, capacity)| matches!(status, 4 | 5 | 8 | 9) || *capacity <= 20),
    );
    let charging = if rows
        .iter()
        .any(|(status, _)| matches!(status, 2 | 6 | 7 | 8 | 9))
    {
        Some(true)
    } else if rows.iter().all(|(status, _)| *status == 1) {
        Some(false)
    } else {
        None
    };
    (battery_low, charging)
}

#[cfg(target_os = "linux")]
fn parse_networkmanager_metered(text: &str) -> Option<bool> {
    let mut fields = text.split_whitespace();
    let signature = fields.next()?;
    let value = fields.next()?.parse::<u32>().ok()?;
    if fields.next().is_some() || signature != "u" {
        return None;
    }
    // UNKNOWN and unrecognized future values remain unknown.
    match value {
        1 | 3 => Some(true),  // YES or GUESS_YES
        2 | 4 => Some(false), // NO or GUESS_NO
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn probe_linux_battery() -> (Option<bool>, Option<bool>) {
    use std::fs;
    let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
        return (None, None);
    };
    let mut samples = Vec::new();
    let mut external_power = false;
    let mut external_power_known = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let supply_type = fs::read_to_string(path.join("type"))
            .ok()
            .map(|value| value.trim().to_owned());
        if supply_type.as_deref() != Some("Battery") {
            if matches!(
                supply_type.as_deref(),
                Some("Mains" | "USB" | "USB_C" | "USB_PD")
            ) {
                if let Some(online) = fs::read_to_string(path.join("online"))
                    .ok()
                    .and_then(|value| value.trim().parse::<u8>().ok())
                {
                    external_power_known = true;
                    external_power |= online == 1;
                }
            }
            continue;
        }
        let capacity = fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok());
        let status = fs::read_to_string(path.join("status")).ok();
        samples.push((capacity, status.map(|s| s.trim().to_owned())));
    }
    summarize_battery_samples(&samples, external_power_known.then_some(external_power))
}

#[cfg(target_os = "linux")]
fn summarize_battery_samples(
    samples: &[(Option<u8>, Option<String>)],
    external_power: Option<bool>,
) -> (Option<bool>, Option<bool>) {
    if samples.is_empty() {
        return (None, external_power);
    }
    let battery_low = samples
        .iter()
        .map(|(capacity, _)| capacity.map(|value| value <= 20))
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().any(|low| low));
    (battery_low, external_power)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_defer_only_when_unmet_or_unknown_policy_says_so() {
        let policy = BackgroundPolicy {
            network: NetworkRequirement::Unmetered,
            power: PowerRequirement::BatteryNotLow,
            unknown: UnknownBehavior::Defer,
        };
        assert_eq!(
            evaluate(&policy, &BackgroundConditions::default()),
            Some(DeferReason::UnknownNetwork)
        );
        assert_eq!(
            evaluate(
                &policy,
                &BackgroundConditions {
                    metered: Some(true),
                    battery_low: Some(false),
                    charging: None
                }
            ),
            Some(DeferReason::MeteredNetwork)
        );
        assert_eq!(
            evaluate(
                &policy,
                &BackgroundConditions {
                    metered: Some(false),
                    battery_low: Some(true),
                    charging: None
                }
            ),
            Some(DeferReason::LowBattery)
        );
        assert_eq!(
            evaluate(
                &policy,
                &BackgroundConditions {
                    metered: Some(false),
                    battery_low: Some(false),
                    charging: None
                }
            ),
            None
        );
    }

    #[test]
    fn charging_unknown_and_not_charging_are_distinct() {
        let policy = BackgroundPolicy {
            network: NetworkRequirement::Any,
            power: PowerRequirement::Charging,
            unknown: UnknownBehavior::Defer,
        };
        assert_eq!(
            evaluate(&policy, &BackgroundConditions::default()),
            Some(DeferReason::UnknownPower)
        );
        assert_eq!(
            evaluate(
                &policy,
                &BackgroundConditions {
                    metered: None,
                    battery_low: None,
                    charging: Some(false)
                }
            ),
            Some(DeferReason::NotCharging)
        );
    }

    #[test]
    fn combined_power_requirement_checks_battery_and_charging() {
        let policy = BackgroundPolicy {
            network: NetworkRequirement::Any,
            power: PowerRequirement::BatteryNotLowAndCharging,
            unknown: UnknownBehavior::Defer,
        };
        assert_eq!(
            evaluate(
                &policy,
                &BackgroundConditions {
                    metered: None,
                    battery_low: Some(true),
                    charging: Some(false)
                }
            ),
            Some(DeferReason::LowBattery)
        );
        assert_eq!(
            evaluate(
                &policy,
                &BackgroundConditions {
                    metered: None,
                    battery_low: Some(false),
                    charging: Some(false)
                }
            ),
            Some(DeferReason::NotCharging)
        );
        assert_eq!(
            evaluate(
                &policy,
                &BackgroundConditions {
                    metered: None,
                    battery_low: Some(false),
                    charging: Some(true)
                }
            ),
            None
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn networkmanager_global_metered_property_parser() {
        assert_eq!(parse_networkmanager_metered("u 1"), Some(true));
        assert_eq!(parse_networkmanager_metered("u 3"), Some(true));
        assert_eq!(parse_networkmanager_metered("u 2"), Some(false));
        assert_eq!(parse_networkmanager_metered("u 4"), Some(false));
        assert_eq!(parse_networkmanager_metered("u 0"), None);
        assert_eq!(parse_networkmanager_metered("u 9"), None);
        assert_eq!(parse_networkmanager_metered(""), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn battery_samples_need_complete_answers() {
        assert_eq!(summarize_battery_samples(&[], None), (None, None));
        assert_eq!(
            summarize_battery_samples(&[(Some(15), Some("Discharging".into()))], None),
            (Some(true), None)
        );
        assert_eq!(
            summarize_battery_samples(
                &[
                    (Some(80), Some("Charging".into())),
                    (Some(10), Some("Discharging".into()))
                ],
                None
            ),
            (Some(true), None)
        );
        assert_eq!(
            summarize_battery_samples(&[(Some(80), None)], None),
            (Some(false), None)
        );
        assert_eq!(
            summarize_battery_samples(&[(None, Some("Full".into()))], None),
            (None, None)
        );
        assert_eq!(
            summarize_battery_samples(&[(Some(100), Some("Full".into()))], Some(true)),
            (Some(false), Some(true))
        );
        assert_eq!(
            summarize_battery_samples(&[(Some(100), Some("Full".into()))], Some(false)),
            (Some(false), Some(false))
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pmset_parser_reads_battery_and_external_power() {
        assert_eq!(
            parse_pmset_battery(
                "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1) 50%; not charging; 0:00 remaining present: true\n"
            ),
            (Some(false), Some(true))
        );
        assert_eq!(
            parse_pmset_battery("Now drawing from 'AC Power'\nNo batteries found"),
            (None, Some(true))
        );
        assert_eq!(
            parse_pmset_battery(
                "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1) 15%; discharging; 1:00 remaining present: true\n"
            ),
            (Some(true), Some(false))
        );
        assert_eq!(parse_pmset_battery("No batteries found"), (None, None));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_cim_parser_reads_status_and_capacity() {
        assert_eq!(
            parse_windows_battery_csv(
                "\"BatteryStatus\",\"EstimatedChargeRemaining\"\r\n\"6\",\"83\"\r\n"
            ),
            (Some(false), Some(true))
        );
        assert_eq!(
            parse_windows_battery_csv(
                "\"BatteryStatus\",\"EstimatedChargeRemaining\"\r\n\"4\",\"19\"\r\n"
            ),
            (Some(true), None)
        );
        assert_eq!(parse_windows_battery_csv(""), (None, None));
    }
}
