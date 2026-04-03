use serde_json::Value;
use std::mem::MaybeUninit;

use crate::expression::functions::{parse_date_like_string, parse_duration_string, parse_wikilink};

#[cfg(unix)]
use libc::{localtime_r, time_t, tm};
#[cfg(windows)]
use libc::{localtime_s, time_t, tm};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataviewValueType {
    Null,
    Text,
    Number,
    Boolean,
    Date,
    Duration,
    Link,
    List,
    Object,
}

impl DataviewValueType {
    #[must_use]
    pub fn typeof_name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Text => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Date => "date",
            Self::Duration => "duration",
            Self::Link => "link",
            Self::List => "array",
            Self::Object => "object",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DataviewTimeZone {
    #[default]
    System,
    FixedOffsetMinutes(i32),
}

impl DataviewTimeZone {
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Self {
        let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
            return Self::System;
        };

        match raw.to_ascii_lowercase().as_str() {
            "system" | "local" => Self::System,
            "utc" | "z" => Self::FixedOffsetMinutes(0),
            _ => parse_fixed_offset_minutes(raw).map_or(Self::System, Self::FixedOffsetMinutes),
        }
    }

    #[must_use]
    pub fn offset_ms_at(self, timestamp_ms: i64) -> i64 {
        match self {
            Self::System => system_local_offset_ms(timestamp_ms),
            Self::FixedOffsetMinutes(minutes) => i64::from(minutes) * 60_000,
        }
    }

    #[must_use]
    pub fn localize_utc_ms(self, timestamp_ms: i64) -> i64 {
        timestamp_ms.saturating_add(self.offset_ms_at(timestamp_ms))
    }
}

#[must_use]
pub fn dataview_value_type(value: &Value) -> DataviewValueType {
    match value {
        Value::Null => DataviewValueType::Null,
        Value::Bool(_) => DataviewValueType::Boolean,
        Value::Number(_) => DataviewValueType::Number,
        Value::String(text) => {
            let trimmed = text.trim();
            if parse_wikilink(trimmed).is_some() {
                DataviewValueType::Link
            } else if parse_date_like_string(trimmed).is_some() {
                DataviewValueType::Date
            } else if parse_duration_string(trimmed).is_some() {
                DataviewValueType::Duration
            } else {
                DataviewValueType::Text
            }
        }
        Value::Array(_) => DataviewValueType::List,
        Value::Object(_) => DataviewValueType::Object,
    }
}

fn parse_fixed_offset_minutes(raw: &str) -> Option<i32> {
    let trimmed = raw.trim();
    let sign = match trimmed.as_bytes().first().copied() {
        Some(b'+') => 1_i32,
        Some(b'-') => -1_i32,
        _ => return None,
    };
    let rest = &trimmed[1..];

    let (hours, minutes) = if let Some((hours, minutes)) = rest.split_once(':') {
        (hours, minutes)
    } else if rest.len() == 4 {
        (&rest[..2], &rest[2..])
    } else if rest.len() == 2 {
        (rest, "00")
    } else {
        return None;
    };

    let hours = hours.parse::<i32>().ok()?;
    let minutes = minutes.parse::<i32>().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }

    Some(sign * (hours * 60 + minutes))
}

#[cfg(unix)]
fn system_local_offset_ms(timestamp_ms: i64) -> i64 {
    let seconds = timestamp_ms.div_euclid(1000);
    let mut result = MaybeUninit::<tm>::uninit();
    let time = seconds as time_t;

    // SAFETY: `time` and `result` point to valid memory for the duration of the call.
    let local = unsafe {
        if localtime_r(&time, result.as_mut_ptr()).is_null() {
            return 0;
        }
        result.assume_init()
    };

    let year = i64::from(local.tm_year) + 1900;
    let month = i64::from(local.tm_mon) + 1;
    let day = i64::from(local.tm_mday);
    let hour = i64::from(local.tm_hour);
    let minute = i64::from(local.tm_min);
    let second = i64::from(local.tm_sec);
    let local_ms = timestamp_from_civil(year, month, day)
        .saturating_add(hour * 3_600_000)
        .saturating_add(minute * 60_000)
        .saturating_add(second * 1_000);
    local_ms.saturating_sub(seconds * 1_000)
}

#[cfg(windows)]
fn system_local_offset_ms(timestamp_ms: i64) -> i64 {
    let seconds = timestamp_ms.div_euclid(1000);
    let mut result = MaybeUninit::<tm>::uninit();
    let time = seconds as time_t;

    // SAFETY: `time` and `result` point to valid memory for the duration of the call.
    let local = unsafe {
        if localtime_s(result.as_mut_ptr(), &time) != 0 {
            return 0;
        }
        result.assume_init()
    };

    let year = i64::from(local.tm_year) + 1900;
    let month = i64::from(local.tm_mon) + 1;
    let day = i64::from(local.tm_mday);
    let hour = i64::from(local.tm_hour);
    let minute = i64::from(local.tm_min);
    let second = i64::from(local.tm_sec);
    let local_ms = timestamp_from_civil(year, month, day)
        .saturating_add(hour * 3_600_000)
        .saturating_add(minute * 60_000)
        .saturating_add(second * 1_000);
    local_ms.saturating_sub(seconds * 1_000)
}

#[cfg(not(any(unix, windows)))]
fn system_local_offset_ms(_timestamp_ms: i64) -> i64 {
    0
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let adjusted_month = if month <= 2 { month + 9 } else { month - 3 };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn timestamp_from_civil(year: i64, month: i64, day: i64) -> i64 {
    days_from_civil(year, month, day) * 86_400_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_all_dataview_value_kinds() {
        assert_eq!(dataview_value_type(&Value::Null), DataviewValueType::Null);
        assert_eq!(
            dataview_value_type(&json!("hello")),
            DataviewValueType::Text
        );
        assert_eq!(dataview_value_type(&json!(42)), DataviewValueType::Number);
        assert_eq!(
            dataview_value_type(&json!(true)),
            DataviewValueType::Boolean
        );
        assert_eq!(
            dataview_value_type(&json!("2026-04-18")),
            DataviewValueType::Date
        );
        assert_eq!(
            dataview_value_type(&json!("1d 3h")),
            DataviewValueType::Duration
        );
        assert_eq!(
            dataview_value_type(&json!("[[Alpha]]")),
            DataviewValueType::Link
        );
        assert_eq!(
            dataview_value_type(&json!(["a", "b"])),
            DataviewValueType::List
        );
        assert_eq!(
            dataview_value_type(&json!({"a": 1})),
            DataviewValueType::Object
        );
    }

    #[test]
    fn parses_timezone_override_strings() {
        assert_eq!(
            DataviewTimeZone::parse(Some("+02:30")),
            DataviewTimeZone::FixedOffsetMinutes(150)
        );
        assert_eq!(
            DataviewTimeZone::parse(Some("-0530")),
            DataviewTimeZone::FixedOffsetMinutes(-330)
        );
        assert_eq!(
            DataviewTimeZone::parse(Some("utc")),
            DataviewTimeZone::FixedOffsetMinutes(0)
        );
        assert_eq!(
            DataviewTimeZone::parse(Some("local")),
            DataviewTimeZone::System
        );
        assert_eq!(
            DataviewTimeZone::parse(Some("bogus")),
            DataviewTimeZone::System
        );
    }

    #[test]
    fn localizes_using_fixed_offsets() {
        let zone = DataviewTimeZone::FixedOffsetMinutes(120);
        assert_eq!(zone.offset_ms_at(0), 7_200_000);
        assert_eq!(zone.localize_utc_ms(1_000), 7_201_000);
    }
}
