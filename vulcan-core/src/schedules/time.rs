//! Schedule time: anchored intervals and five-field cron in an IANA time
//! zone (`docs/specs/scheduled-execution.md`, "Time").
//!
//! Everything here is pure and takes instants as arguments, so the
//! scheduler's clock is injected and DST, downtime, and clock jumps are
//! tested without real time.

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use std::fmt::{Display, Formatter};

/// The shortest and longest interval a job may use.
pub const MIN_INTERVAL_SECONDS: i64 = 60;
pub const MAX_INTERVAL_SECONDS: i64 = 31 * 24 * 60 * 60;
/// How far a cron search looks for a matching day before giving up, which
/// bounds expressions that can never match (such as 31 February).
const CRON_SEARCH_DAYS: i64 = 5 * 366;

/// When a job fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Every `every_seconds`, at `anchor + k × every`.
    Every {
        every_seconds: i64,
        anchor: DateTime<Utc>,
    },
    Cron {
        expression: CronExpression,
        timezone: Tz,
    },
}

impl Trigger {
    /// The first occurrence strictly after `instant`.
    #[must_use]
    pub fn next_after(&self, instant: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Self::Every {
                every_seconds,
                anchor,
            } => {
                let step = *every_seconds;
                let elapsed = (instant - *anchor).num_seconds();
                let steps = if elapsed < 0 {
                    0
                } else {
                    elapsed.div_euclid(step) + 1
                };
                Some(*anchor + Duration::seconds(steps * step))
            }
            Self::Cron {
                expression,
                timezone,
            } => expression.next_after(instant, *timezone),
        }
    }

    /// The latest occurrence at or before `instant`.
    #[must_use]
    pub fn latest_at_or_before(&self, instant: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Self::Every {
                every_seconds,
                anchor,
            } => {
                let elapsed = (instant - *anchor).num_seconds();
                (elapsed >= 0).then(|| {
                    *anchor + Duration::seconds(elapsed.div_euclid(*every_seconds) * every_seconds)
                })
            }
            Self::Cron {
                expression,
                timezone,
            } => expression.latest_at_or_before(instant, *timezone),
        }
    }
}

/// Parses an interval such as `90s`, `15m`, `2h`, or `1d`.
pub fn parse_interval(text: &str) -> Result<i64, ScheduleTimeError> {
    let invalid = || ScheduleTimeError::Interval(text.to_string());
    let text = text.trim();
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .ok_or_else(invalid)?;
    let (number, unit) = text.split_at(split);
    let number = number.parse::<i64>().map_err(|_| invalid())?;
    let unit_seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err(invalid()),
    };
    let seconds = number.checked_mul(unit_seconds).ok_or_else(invalid)?;
    if !(MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&seconds) {
        return Err(ScheduleTimeError::IntervalBounds(text.to_string()));
    }
    Ok(seconds)
}

/// A five-field cron expression: minute, hour, day of month, month, day of
/// week. Lists, ranges, steps, and `*` are supported; names and `@`
/// shortcuts are not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpression {
    source: String,
    minutes: u64,
    hours: u32,
    days_of_month: u32,
    months: u16,
    days_of_week: u8,
    /// Whether each day field is restricted (does not start with `*`).
    /// When both are, a day matches if either does, as in Vixie cron.
    day_of_month_restricted: bool,
    day_of_week_restricted: bool,
}

impl CronExpression {
    pub fn parse(source: &str) -> Result<Self, ScheduleTimeError> {
        let fields = source.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(ScheduleTimeError::Cron(
                source.to_string(),
                "a cron expression has five fields: minute hour day-of-month month day-of-week"
                    .to_string(),
            ));
        }
        let field = |index: usize, low: u32, high: u32| {
            parse_field(fields[index], low, high)
                .map_err(|detail| ScheduleTimeError::Cron(source.to_string(), detail))
        };
        let minutes = field(0, 0, 59)?;
        let hours = field(1, 0, 23)?;
        let days_of_month = field(2, 1, 31)?;
        let months = field(3, 1, 12)?;
        let mut days_of_week = field(4, 0, 7)?;
        // 7 is Sunday too.
        if days_of_week & (1 << 7) != 0 {
            days_of_week |= 1;
        }
        Ok(Self {
            source: source.trim().to_string(),
            minutes,
            hours: u32::try_from(hours).expect("hours fit in 24 bits"),
            days_of_month: u32::try_from(days_of_month).expect("days fit in 32 bits"),
            months: u16::try_from(months).expect("months fit in 13 bits"),
            days_of_week: u8::try_from(days_of_week & 0x7f).expect("weekdays fit in 7 bits"),
            day_of_month_restricted: !fields[2].starts_with('*'),
            day_of_week_restricted: !fields[4].starts_with('*'),
        })
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    fn matches_day(&self, date: NaiveDate) -> bool {
        use chrono::Datelike;
        if self.months & (1 << date.month()) == 0 {
            return false;
        }
        let day_of_month = self.days_of_month & (1 << date.day()) != 0;
        let day_of_week = self.days_of_week & (1 << date.weekday().num_days_from_sunday()) != 0;
        match (self.day_of_month_restricted, self.day_of_week_restricted) {
            (true, true) => day_of_month || day_of_week,
            (true, false) => day_of_month,
            (false, true) => day_of_week,
            (false, false) => true,
        }
    }

    /// The matching local times of one day, earliest first.
    fn times_of_day(&self) -> impl DoubleEndedIterator<Item = (u32, u32)> + '_ {
        (0..24u32)
            .filter(|hour| self.hours & (1 << hour) != 0)
            .flat_map(move |hour| {
                (0..60u32)
                    .filter(|minute| self.minutes & (1 << minute) != 0)
                    .map(move |minute| (hour, minute))
            })
    }

    fn next_after(&self, instant: DateTime<Utc>, timezone: Tz) -> Option<DateTime<Utc>> {
        let start = instant.with_timezone(&timezone).date_naive();
        for offset in 0..CRON_SEARCH_DAYS {
            let date = start + Duration::days(offset);
            if !self.matches_day(date) {
                continue;
            }
            for (hour, minute) in self.times_of_day() {
                let local = date.and_hms_opt(hour, minute, 0)?;
                if let Some(occurrence) = resolve_local(local, timezone) {
                    if occurrence > instant {
                        return Some(occurrence);
                    }
                }
            }
        }
        None
    }

    fn latest_at_or_before(&self, instant: DateTime<Utc>, timezone: Tz) -> Option<DateTime<Utc>> {
        let start = instant.with_timezone(&timezone).date_naive();
        for offset in 0..CRON_SEARCH_DAYS {
            let date = start - Duration::days(offset);
            if !self.matches_day(date) {
                continue;
            }
            for (hour, minute) in self.times_of_day().rev() {
                let local = date.and_hms_opt(hour, minute, 0)?;
                if let Some(occurrence) = resolve_local(local, timezone) {
                    if occurrence <= instant {
                        return Some(occurrence);
                    }
                }
            }
        }
        None
    }
}

/// The instant a local wall-clock time names. A repeated time (fall back)
/// is its first occurrence, so it fires once. A time inside a gap (spring
/// forward) moves to the first valid instant after the gap.
fn resolve_local(local: NaiveDateTime, timezone: Tz) -> Option<DateTime<Utc>> {
    match timezone.from_local_datetime(&local) {
        LocalResult::Single(time) => Some(time.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        LocalResult::None => {
            // Gaps last at most a few hours and end on a whole minute in
            // every real zone, so the first local minute that exists is the
            // first instant after the gap.
            let mut probe = local;
            for _ in 0..(4 * 60) {
                probe += Duration::minutes(1);
                if let LocalResult::Single(time) | LocalResult::Ambiguous(time, _) =
                    timezone.from_local_datetime(&probe)
                {
                    return Some(time.with_timezone(&Utc));
                }
            }
            None
        }
    }
}

/// Parses one cron field into a bit set over `low..=high`.
fn parse_field(field: &str, low: u32, high: u32) -> Result<u64, String> {
    let mut bits = 0_u64;
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step = step
                    .parse::<u32>()
                    .ok()
                    .filter(|step| *step > 0)
                    .ok_or_else(|| format!("invalid step `{step}` in `{field}`"))?;
                (range, step)
            }
            None => (part, 1),
        };
        let (start, end) = if range == "*" {
            (low, high)
        } else if let Some((start, end)) = range.split_once('-') {
            (
                number(start, low, high, field)?,
                number(end, low, high, field)?,
            )
        } else {
            let value = number(range, low, high, field)?;
            // `a/n` means from `a` to the field's end in steps of `n`.
            (value, if part.contains('/') { high } else { value })
        };
        if start > end {
            return Err(format!("range `{range}` runs backward in `{field}`"));
        }
        for value in (start..=end).step_by(step as usize) {
            bits |= 1 << value;
        }
    }
    Ok(bits)
}

fn number(text: &str, low: u32, high: u32, field: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|value| (low..=high).contains(value))
        .ok_or_else(|| format!("`{text}` is outside {low}-{high} in `{field}`"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleTimeError {
    Interval(String),
    IntervalBounds(String),
    Cron(String, String),
    Timezone(String),
}

impl Display for ScheduleTimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interval(text) => write!(
                formatter,
                "`{text}` is not an interval; use a whole number with s, m, h, or d, such as 15m"
            ),
            Self::IntervalBounds(text) => write!(
                formatter,
                "interval `{text}` is outside 1 minute to 31 days"
            ),
            Self::Cron(source, detail) => write!(formatter, "cron `{source}`: {detail}"),
            Self::Timezone(name) => write!(formatter, "`{name}` is not an IANA time zone"),
        }
    }
}

impl std::error::Error for ScheduleTimeError {}

/// Parses an IANA time-zone name such as `Europe/Berlin` or `UTC`.
pub fn parse_timezone(name: &str) -> Result<Tz, ScheduleTimeError> {
    name.parse::<Tz>()
        .map_err(|_| ScheduleTimeError::Timezone(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn cron(expression: &str, timezone: &str) -> Trigger {
        Trigger::Cron {
            expression: CronExpression::parse(expression).unwrap(),
            timezone: parse_timezone(timezone).unwrap(),
        }
    }

    #[test]
    fn intervals_fire_on_their_anchor_grid() {
        assert_eq!(parse_interval("15m").unwrap(), 900);
        assert_eq!(parse_interval("1d").unwrap(), 86_400);
        for invalid in ["", "15", "m", "15x", "-5m", "30s", "32d"] {
            assert!(parse_interval(invalid).is_err(), "{invalid}");
        }
        let trigger = Trigger::Every {
            every_seconds: 900,
            anchor: utc("2026-01-01T00:00:00Z"),
        };
        assert_eq!(
            trigger.next_after(utc("2026-03-04T10:07:00Z")),
            Some(utc("2026-03-04T10:15:00Z"))
        );
        assert_eq!(
            trigger.next_after(utc("2026-03-04T10:15:00Z")),
            Some(utc("2026-03-04T10:30:00Z"))
        );
        assert_eq!(
            trigger.latest_at_or_before(utc("2026-03-04T10:15:00Z")),
            Some(utc("2026-03-04T10:15:00Z"))
        );
        assert_eq!(
            trigger.next_after(utc("2025-12-31T00:00:00Z")),
            Some(utc("2026-01-01T00:00:00Z"))
        );
        assert_eq!(
            trigger.latest_at_or_before(utc("2025-12-31T00:00:00Z")),
            None
        );
    }

    #[test]
    fn cron_fields_parse_lists_ranges_and_steps() {
        let expression = CronExpression::parse("*/15 9-17 * * 1-5").unwrap();
        assert_eq!(
            expression.minutes,
            (1 << 0) | (1 << 15) | (1 << 30) | (1 << 45)
        );
        let sunday = CronExpression::parse("0 0 * * 7").unwrap();
        assert_eq!(sunday.days_of_week, 1);
        let from = CronExpression::parse("5/20 * * * *").unwrap();
        assert_eq!(from.minutes, (1 << 5) | (1 << 25) | (1 << 45));
        for invalid in [
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "10-5 * * * *",
            "@daily",
            "0 0 * * mon",
        ] {
            assert!(CronExpression::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn cron_runs_in_its_time_zone_and_either_day_field_may_match() {
        let nightly = cron("30 2 * * *", "Europe/Berlin");
        // 02:30 in Berlin in winter is 01:30 UTC.
        assert_eq!(
            nightly.next_after(utc("2026-01-10T12:00:00Z")),
            Some(utc("2026-01-11T01:30:00Z"))
        );
        // The 13th, or any Friday.
        let either = cron("0 12 13 * 5", "UTC");
        assert_eq!(
            either.next_after(utc("2026-03-01T00:00:00Z")),
            Some(utc("2026-03-06T12:00:00Z")),
            "the first Friday comes before the 13th"
        );
        assert_eq!(
            either.next_after(utc("2026-03-12T13:00:00Z")),
            Some(utc("2026-03-13T12:00:00Z"))
        );
        assert_eq!(
            cron("0 0 31 2 *", "UTC").next_after(utc("2026-01-01T00:00:00Z")),
            None,
            "an expression that never matches finds nothing"
        );
    }

    #[test]
    fn a_time_in_the_spring_gap_fires_once_when_the_gap_ends() {
        // Berlin skipped 02:00-03:00 on 2026-03-29.
        let in_gap = cron("30 2 * * *", "Europe/Berlin");
        let occurrence = in_gap.next_after(utc("2026-03-28T12:00:00Z")).unwrap();
        assert_eq!(occurrence, utc("2026-03-29T01:00:00Z"), "03:00 local");
        assert_eq!(
            in_gap.next_after(occurrence),
            Some(utc("2026-03-30T00:30:00Z")),
            "the next run is the following night at 02:30 summer time"
        );
        // Every minute in the gap collapses into one run.
        let every_minute = cron("* 2 * * *", "Europe/Berlin");
        let first = every_minute
            .next_after(utc("2026-03-29T00:59:00Z"))
            .unwrap();
        assert_eq!(first, utc("2026-03-29T01:00:00Z"));
        assert_eq!(
            every_minute.next_after(first),
            Some(utc("2026-03-30T00:00:00Z"))
        );
    }

    #[test]
    fn a_repeated_fall_time_fires_once() {
        // Berlin repeated 02:00-03:00 on 2026-10-25.
        let repeated = cron("30 2 * * *", "Europe/Berlin");
        let first = repeated.next_after(utc("2026-10-24T12:00:00Z")).unwrap();
        assert_eq!(first, utc("2026-10-25T00:30:00Z"), "the summer-time 02:30");
        assert_eq!(
            repeated.next_after(first),
            Some(utc("2026-10-26T01:30:00Z")),
            "not again at the winter-time 02:30"
        );
        assert_eq!(
            repeated.latest_at_or_before(utc("2026-10-25T02:00:00Z")),
            Some(first)
        );
    }

    #[test]
    fn latest_and_next_agree_for_cron() {
        let trigger = cron("*/10 * * * *", "America/New_York");
        let now = utc("2026-07-01T14:37:12Z");
        let latest = trigger.latest_at_or_before(now).unwrap();
        assert_eq!(latest, utc("2026-07-01T14:30:00Z"));
        assert_eq!(
            trigger.next_after(latest),
            Some(utc("2026-07-01T14:40:00Z"))
        );
        assert!(parse_timezone("Mars/Olympus").is_err());
    }
}
