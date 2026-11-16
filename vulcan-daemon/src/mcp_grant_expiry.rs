//! Lifetimes a person may choose for a named MCP connection grant.
//!
//! The consent page offers a few presets and a custom last day. A grant never
//! lasts longer than about a year: possession-based agent credentials should be
//! short-lived, and a longer connection is renewed by approving it again.

use std::collections::BTreeMap;

use chrono::{
    DateTime, Datelike, Days, FixedOffset, Local, NaiveDate, NaiveTime, Offset, TimeZone, Utc,
};

/// Form field carrying a preset value or [`CUSTOM_EXPIRY`].
pub const EXPIRY_FIELD: &str = "expiry";
/// Form field carrying the custom last day as `YYYY-MM-DD`.
pub const EXPIRY_DATE_FIELD: &str = "expiry_date";
/// [`EXPIRY_FIELD`] value that selects [`EXPIRY_DATE_FIELD`].
pub const CUSTOM_EXPIRY: &str = "custom";
/// Preset selected when the consent page opens.
pub const DEFAULT_EXPIRY: &str = "30d";
/// The latest custom last day, counted from today.
pub const MAX_CUSTOM_DAYS: u64 = 365;

const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryPreset {
    pub value: &'static str,
    pub label: &'static str,
    pub seconds: u64,
}

pub const EXPIRY_PRESETS: &[ExpiryPreset] = &[
    ExpiryPreset {
        value: "1h",
        label: "1 hour",
        seconds: HOUR,
    },
    ExpiryPreset {
        value: "1d",
        label: "1 day",
        seconds: DAY,
    },
    ExpiryPreset {
        value: "7d",
        label: "7 days",
        seconds: 7 * DAY,
    },
    ExpiryPreset {
        value: "30d",
        label: "30 days",
        seconds: 30 * DAY,
    },
    ExpiryPreset {
        value: "90d",
        label: "90 days",
        seconds: 90 * DAY,
    },
    ExpiryPreset {
        value: "365d",
        label: "1 year",
        seconds: 365 * DAY,
    },
];

/// The current time and the time zone in which the consent page shows dates
/// and reads a custom last day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantClock {
    pub now: u64,
    pub offset: FixedOffset,
}

impl GrantClock {
    /// The clock in this host's local time zone at `now`.
    #[must_use]
    pub fn local(now: u64) -> Self {
        let offset = i64::try_from(now)
            .ok()
            .and_then(|seconds| Local.timestamp_opt(seconds, 0).single())
            .map_or_else(|| Utc.fix(), |instant| instant.offset().fix());
        Self { now, offset }
    }

    #[must_use]
    pub fn utc(now: u64) -> Self {
        Self {
            now,
            offset: Utc.fix(),
        }
    }

    fn at(&self, timestamp: u64) -> Option<DateTime<FixedOffset>> {
        let seconds = i64::try_from(timestamp).ok()?;
        self.offset.timestamp_opt(seconds, 0).single()
    }

    /// Today's date in this clock's time zone.
    #[must_use]
    pub fn today(&self) -> NaiveDate {
        self.at(self.now)
            .map_or(NaiveDate::MIN, |instant| instant.date_naive())
    }

    /// The latest last day a custom expiry accepts.
    #[must_use]
    pub fn latest_custom_date(&self) -> NaiveDate {
        self.today()
            .checked_add_days(Days::new(MAX_CUSTOM_DAYS))
            .unwrap_or(NaiveDate::MAX)
    }

    /// The instant a grant whose last day is `date` ends: midnight after it.
    #[must_use]
    pub fn end_of_day(&self, date: NaiveDate) -> Option<u64> {
        let next = date.succ_opt()?.and_time(NaiveTime::MIN);
        let instant = self.offset.from_local_datetime(&next).single()?;
        u64::try_from(instant.timestamp()).ok()
    }

    /// When a grant created now with `preset` ends.
    #[must_use]
    pub fn preset_end(&self, preset: &ExpiryPreset) -> Option<u64> {
        self.now.checked_add(preset.seconds)
    }

    /// A short description of `timestamp`: the time of day when it falls
    /// within the next day, otherwise the date.
    #[must_use]
    pub fn describe(&self, timestamp: u64) -> String {
        let Some(instant) = self.at(timestamp) else {
            return String::new();
        };
        if timestamp.saturating_sub(self.now) < DAY {
            instant.format("%H:%M").to_string()
        } else if instant.year() == self.today().year() {
            instant.format("%-d %b").to_string()
        } else {
            instant.format("%-d %b %Y").to_string()
        }
    }

    /// The time zone as the consent page names it, such as `UTC+01:00`.
    #[must_use]
    pub fn zone_label(&self) -> String {
        if self.offset.local_minus_utc() == 0 {
            "UTC".to_string()
        } else {
            format!("UTC{}", self.offset)
        }
    }
}

/// The Unix timestamp at which a grant approved with `form` expires.
///
/// # Errors
///
/// Returns a message for the person when no valid preset is chosen, or a
/// custom last day is missing, malformed, in the past, or more than
/// [`MAX_CUSTOM_DAYS`] ahead.
pub fn resolve_grant_expiry(
    form: &BTreeMap<String, String>,
    clock: &GrantClock,
) -> Result<u64, String> {
    let choice = form.get(EXPIRY_FIELD).map_or("", |value| value.trim());
    if choice == CUSTOM_EXPIRY {
        let raw = form
            .get(EXPIRY_DATE_FIELD)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "choose the last day for a custom expiry".to_string())?;
        let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .map_err(|_| "the custom expiry date must be a date such as 2027-01-31".to_string())?;
        if date < clock.today() {
            return Err("the custom expiry date is in the past".to_string());
        }
        if date > clock.latest_custom_date() {
            return Err(format!(
                "the custom expiry date must be no later than {}",
                clock.latest_custom_date().format("%Y-%m-%d")
            ));
        }
        return clock
            .end_of_day(date)
            .filter(|end| *end > clock.now)
            .ok_or_else(|| "the custom expiry date is out of range".to_string());
    }
    let preset = EXPIRY_PRESETS
        .iter()
        .find(|preset| preset.value == choice)
        .ok_or_else(|| {
            let presets = EXPIRY_PRESETS
                .iter()
                .map(|preset| preset.label)
                .collect::<Vec<_>>()
                .join(", ");
            format!("choose an expiry: {presets}, or a custom date")
        })?;
    clock
        .preset_end(preset)
        .ok_or_else(|| "grant expiry is out of range".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-16 12:00:00 UTC.
    const NOW: u64 = 1_794_830_400;

    fn form(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn cet() -> GrantClock {
        GrantClock {
            now: NOW,
            offset: FixedOffset::east_opt(3600).unwrap(),
        }
    }

    #[test]
    fn every_preset_adds_its_lifetime_to_now() {
        let clock = GrantClock::utc(NOW);
        for preset in EXPIRY_PRESETS {
            assert_eq!(
                resolve_grant_expiry(&form(&[(EXPIRY_FIELD, preset.value)]), &clock),
                Ok(NOW + preset.seconds),
                "{}",
                preset.value
            );
        }
        assert!(EXPIRY_PRESETS
            .iter()
            .any(|preset| preset.value == DEFAULT_EXPIRY));
    }

    #[test]
    fn unknown_or_missing_presets_are_refused() {
        let clock = GrantClock::utc(NOW);
        for choice in ["", "2d", "forever", "0", "30"] {
            let error =
                resolve_grant_expiry(&form(&[(EXPIRY_FIELD, choice)]), &clock).expect_err(choice);
            assert!(error.contains("1 hour, 1 day"), "{error}");
        }
        assert!(resolve_grant_expiry(&BTreeMap::new(), &clock).is_err());
        // The former 1/7/30-day field is no longer read.
        assert!(resolve_grant_expiry(&form(&[("expiry_days", "7")]), &clock).is_err());
    }

    #[test]
    fn custom_date_ends_at_midnight_after_the_last_day_in_the_clock_zone() {
        let clock = cet();
        let end = resolve_grant_expiry(
            &form(&[
                (EXPIRY_FIELD, CUSTOM_EXPIRY),
                (EXPIRY_DATE_FIELD, "2026-12-01"),
            ]),
            &clock,
        )
        .unwrap();
        // 2026-12-02 00:00 at UTC+01:00 is 2026-12-01 23:00 UTC.
        assert_eq!(end, 1_796_166_000);

        let today = resolve_grant_expiry(
            &form(&[
                (EXPIRY_FIELD, CUSTOM_EXPIRY),
                (EXPIRY_DATE_FIELD, "2026-11-16"),
            ]),
            &clock,
        )
        .unwrap();
        assert!(today > NOW && today - NOW < DAY);
    }

    #[test]
    fn custom_date_must_be_present_well_formed_and_within_a_year() {
        let clock = cet();
        let custom = |date: &str| {
            resolve_grant_expiry(
                &form(&[(EXPIRY_FIELD, CUSTOM_EXPIRY), (EXPIRY_DATE_FIELD, date)]),
                &clock,
            )
        };
        assert!(custom("").unwrap_err().contains("last day"));
        assert!(custom("16.11.2026").unwrap_err().contains("2027-01-31"));
        assert!(custom("2026-11-15").unwrap_err().contains("past"));
        assert_eq!(clock.latest_custom_date().to_string(), "2027-11-16");
        assert!(custom("2027-11-16").is_ok());
        assert!(custom("2027-11-17").unwrap_err().contains("2027-11-16"));
        assert!(resolve_grant_expiry(&form(&[(EXPIRY_FIELD, CUSTOM_EXPIRY)]), &clock).is_err());
    }

    #[test]
    fn descriptions_use_times_today_and_dates_later() {
        let clock = cet();
        assert_eq!(clock.describe(NOW + HOUR), "14:00");
        assert_eq!(clock.describe(NOW + 7 * DAY), "23 Nov");
        assert_eq!(clock.describe(NOW + 90 * DAY), "14 Feb 2027");
        assert_eq!(clock.zone_label(), "UTC+01:00");
        assert_eq!(GrantClock::utc(NOW).zone_label(), "UTC");
    }
}
