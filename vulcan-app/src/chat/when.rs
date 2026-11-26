//! Times typed in a chat reply: `in 2h`, `90m`, `15:00`, `tomorrow`,
//! `tomorrow 15:00`, `friday at 9am`, `2026-11-20 08:30`.

use crate::tasks::ReminderTimezone;
use jiff::civil::{Date, DateTime, Time};
use jiff::SignedDuration;
use vulcan_core::config::TaskNotesConfig;
use vulcan_core::expression::functions::parse_duration_string;
use vulcan_core::parse_tasknote_natural_language;

/// A parsed reply time: a wall-clock instant, or a whole day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatWhen {
    At(DateTime),
    Day(Date),
}

impl ChatWhen {
    /// The wall-clock time, with `default_time` for a whole day.
    #[must_use]
    pub fn at_or(self, default_time: Time) -> DateTime {
        match self {
            Self::At(at) => at,
            Self::Day(day) => day.to_datetime(default_time),
        }
    }
}

/// Parse `text` as a time relative to `now_ms`, in `timezone`. `None` when
/// the text is not just a time: a reply must not move a task on a guess.
#[must_use]
pub fn parse_chat_when(
    text: &str,
    config: &TaskNotesConfig,
    timezone: &ReminderTimezone,
    now_ms: i64,
) -> Option<ChatWhen> {
    let text = text.trim().trim_end_matches(['.', '!']).trim();
    if text.is_empty() {
        return None;
    }
    let local_now = timezone.local_datetime(now_ms);
    let duration = text.strip_prefix("in ").unwrap_or(text).replace(' ', "");
    if let Some(ms) = parse_duration_string(&duration).filter(|ms| *ms > 0) {
        // A duration is elapsed time, so it is added to the instant: "in 2h"
        // across a DST change is two real hours, not two on the wall clock.
        let at = timezone.local_datetime(now_ms.checked_add(ms)?);
        // Up to the next whole minute, the precision of reminders.
        let rounded = at.with().second(0).subsec_nanosecond(0).build().ok()?;
        let rounded = if rounded < at {
            rounded.checked_add(SignedDuration::from_mins(1)).ok()?
        } else {
            rounded
        };
        return Some(ChatWhen::At(rounded));
    }
    if let Some(time) = clock_time(text) {
        let today = local_now.date().to_datetime(time);
        // A time already past today means tomorrow.
        return Some(ChatWhen::At(if today <= local_now {
            today.checked_add(jiff::Span::new().days(1)).ok()?
        } else {
            today
        }));
    }
    // The task NLP reads dates relative to the reference's UTC day, so pass
    // the local day.
    let reference = local_now
        .date()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_millisecond();
    let parsed = parse_tasknote_natural_language(&format!("x {text}"), config, reference);
    if parsed.title != "x" || parsed.recurrence.is_some() {
        return None;
    }
    let value = parsed.due.or(parsed.scheduled)?;
    if let Ok(at) = DateTime::strptime("%Y-%m-%dT%H:%M:%S", &value) {
        return Some(ChatWhen::At(at));
    }
    Date::strptime("%Y-%m-%d", &value).ok().map(ChatWhen::Day)
}

/// `15:00`, `9:30`, `3pm`, `3 pm`, `10:15am`.
fn clock_time(text: &str) -> Option<Time> {
    let text = text.to_ascii_lowercase();
    let (clock, meridiem) = match text.strip_suffix("am").or_else(|| text.strip_suffix("pm")) {
        Some(clock) => (clock.trim(), Some(text.ends_with("pm"))),
        None => (text.as_str(), None),
    };
    let (hour, minute) = clock.split_once(':').unwrap_or((clock, "00"));
    if hour.is_empty() || hour.len() > 2 || minute.len() != 2 {
        return None;
    }
    let (mut hour, minute) = (hour.parse::<i8>().ok()?, minute.parse::<i8>().ok()?);
    match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + if pm { 12 } else { 0 };
        }
        // A bare number is not a time.
        None if !clock.contains(':') => return None,
        None => {}
    }
    Time::new(hour, minute, 0, 0).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn when_in(zone: &str, now: &str, text: &str) -> Option<ChatWhen> {
        let now = now.parse::<jiff::Timestamp>().unwrap().as_millisecond();
        parse_chat_when(
            text,
            &TaskNotesConfig::default(),
            &ReminderTimezone::named(zone).unwrap(),
            now,
        )
    }

    fn when(text: &str) -> Option<ChatWhen> {
        // Saturday 2026-11-07 10:20:30 UTC.
        when_in("UTC", "2026-11-07T10:20:30Z", text)
    }

    fn at(text: &str) -> DateTime {
        DateTime::strptime("%Y-%m-%d %H:%M", text).unwrap()
    }

    #[test]
    fn reads_durations_clock_times_and_date_phrases() {
        assert_eq!(when("in 2h"), Some(ChatWhen::At(at("2026-11-07 12:21"))));
        assert_eq!(when("90m"), Some(ChatWhen::At(at("2026-11-07 11:51"))));
        assert_eq!(when("15:00"), Some(ChatWhen::At(at("2026-11-07 15:00"))));
        assert_eq!(
            when("9:00"),
            Some(ChatWhen::At(at("2026-11-08 09:00"))),
            "past today"
        );
        assert_eq!(when("3pm"), Some(ChatWhen::At(at("2026-11-07 15:00"))));
        assert_eq!(when("12:15 am"), Some(ChatWhen::At(at("2026-11-08 00:15"))));
        assert_eq!(when("12pm"), Some(ChatWhen::At(at("2026-11-07 12:00"))));
        assert_eq!(
            when("tomorrow 15:00"),
            Some(ChatWhen::At(at("2026-11-08 15:00")))
        );
        assert_eq!(
            when("monday at 9am"),
            Some(ChatWhen::At(at("2026-11-09 09:00")))
        );
        assert_eq!(
            when("tomorrow"),
            Some(ChatWhen::Day(jiff::civil::date(2026, 11, 8)))
        );
        assert_eq!(
            when("2026-11-20 08:30"),
            Some(ChatWhen::At(at("2026-11-20 08:30")))
        );
    }

    #[test]
    fn durations_are_elapsed_time_across_dst_changes() {
        // 00:30 CET on the night clocks go forward (02:00 -> 03:00): two
        // hours later is 03:30 on the wall clock, not 02:30.
        assert_eq!(
            when_in("Europe/Berlin", "2026-03-28T23:30:00Z", "in 2h"),
            Some(ChatWhen::At(at("2026-03-29 03:30")))
        );
        // 01:30 CEST on the night clocks go back (03:00 -> 02:00): two hours
        // later is 02:30 CET, the second 02:30.
        assert_eq!(
            when_in("Europe/Berlin", "2026-10-24T23:30:00Z", "in 2h"),
            Some(ChatWhen::At(at("2026-10-25 02:30")))
        );
    }

    #[test]
    fn refuses_anything_that_is_not_just_a_time() {
        assert_eq!(when(""), None);
        assert_eq!(when("thanks"), None);
        assert_eq!(when("15"), None, "a bare number");
        assert_eq!(when("25:00"), None);
        assert_eq!(when("tomorrow buy milk"), None);
        assert_eq!(when("every monday"), None);
    }
}
