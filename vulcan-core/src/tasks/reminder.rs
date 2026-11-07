//! Reminder times written on inline Markdown tasks.
//!
//! The canonical form is `⏰ YYYY-MM-DD HH:mm`, the obsidian-reminder plugin's
//! Tasks-style marker. Its default `(@YYYY-MM-DD HH:mm)` form is read too.
//! Either may omit the time, which means "at the configured default time".
//! Times are wall-clock times in the vault's reminder timezone, so the
//! values stay naive here; resolving them to instants is the caller's job.
//! A Dataview `[remind:: ...]` field is indexed through the ordinary inline
//! field path under the same `remind` key.

use chrono::{NaiveDate, NaiveTime};
use regex::Regex;
use std::sync::OnceLock;

/// The canonical reminder marker on an inline task line.
pub const INLINE_REMINDER_MARKER: &str = "⏰";

/// The task property key reminder values are indexed under.
pub const INLINE_REMINDER_PROPERTY: &str = "remind";

/// A reminder time from a task line: a date with an optional wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineReminderTime {
    pub date: NaiveDate,
    pub time: Option<NaiveTime>,
}

impl InlineReminderTime {
    /// The normalized property value: `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM`.
    #[must_use]
    pub fn to_property_value(self) -> String {
        match self.time {
            Some(time) => format!("{}T{}", self.date.format("%Y-%m-%d"), time.format("%H:%M")),
            None => self.date.format("%Y-%m-%d").to_string(),
        }
    }

    /// The canonical marker text: `⏰ YYYY-MM-DD` or `⏰ YYYY-MM-DD HH:MM`.
    #[must_use]
    pub fn to_marker(self) -> String {
        match self.time {
            Some(time) => format!(
                "{INLINE_REMINDER_MARKER} {} {}",
                self.date.format("%Y-%m-%d"),
                time.format("%H:%M")
            ),
            None => format!("{INLINE_REMINDER_MARKER} {}", self.date.format("%Y-%m-%d")),
        }
    }
}

/// Parse a reminder value: `YYYY-MM-DD`, optionally followed by a space or
/// `T` and `HH:MM` (seconds are accepted and dropped). Used for indexed
/// `remind` property values, including Dataview `[remind:: ...]` fields.
#[must_use]
pub fn parse_inline_reminder_value(value: &str) -> Option<InlineReminderTime> {
    let captures = value_pattern().captures(value.trim())?;
    build_reminder(
        captures.get(1)?.as_str(),
        captures.get(2).map(|m| m.as_str()),
    )
}

/// The reminder written on a task line, from `⏰ …` or `(@…)`.
#[must_use]
pub fn extract_inline_reminder(text: &str) -> Option<InlineReminderTime> {
    [marker_pattern(), paren_pattern()]
        .into_iter()
        .find_map(|pattern| {
            let captures = pattern.captures(text)?;
            build_reminder(
                captures.get(1)?.as_str(),
                captures.get(2).map(|m| m.as_str()),
            )
        })
}

/// `line` with its reminder set to `reminder`: an existing `⏰` marker,
/// `(@…)` form, or `remind::` field is replaced in place by the canonical
/// marker, otherwise the marker is appended before any trailing block id.
#[must_use]
pub fn set_inline_reminder(line: &str, reminder: InlineReminderTime) -> String {
    let marker = reminder.to_marker();
    for pattern in [marker_pattern(), paren_pattern(), remind_field_pattern()] {
        if let Some(found) = pattern.find(line) {
            return format!("{}{marker}{}", &line[..found.start()], &line[found.end()..]);
        }
    }
    let trimmed = line.trim_end();
    match block_id_suffix().find(trimmed) {
        Some(block) => format!(
            "{} {marker} {}",
            trimmed[..block.start()].trim_end(),
            trimmed[block.start()..].trim_start()
        ),
        None => format!("{trimmed} {marker}"),
    }
}

/// The byte range and value of the `⏰` reminder on `line`, used to move
/// it with a recurring task's other dates.
#[must_use]
pub(crate) fn inline_reminder_marker_span(
    line: &str,
) -> Option<(std::ops::Range<usize>, InlineReminderTime)> {
    let captures = marker_pattern().captures(line)?;
    let whole = captures.get(0)?;
    let reminder = build_reminder(
        captures.get(1)?.as_str(),
        captures.get(2).map(|m| m.as_str()),
    )?;
    Some((whole.range(), reminder))
}

/// A task's text without Tasks annotations, Dataview inline fields, reminder
/// forms, or a trailing block id: the text a person wrote, for display and
/// for identifying a task that has no `🆔` or block id.
#[must_use]
pub fn inline_task_title(text: &str) -> String {
    let mut title = text.to_string();
    for pattern in title_strip_patterns() {
        title = pattern.replace_all(&title, " ").into_owned();
    }
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The block id (`^id`) at the end of a task's text.
#[must_use]
pub fn inline_task_block_id(text: &str) -> Option<&str> {
    block_id_suffix()
        .find(text.trim_end())
        .map(|found| found.as_str().trim_start().trim_start_matches('^'))
}

fn title_strip_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Reminder forms, before the generic date markers.
            r"⏰\s*\d{4}-\d{2}-\d{2}(?:[ T]\d{1,2}:\d{2}(?::\d{2})?)?",
            r"\(@\d{4}-\d{2}-\d{2}(?:[ T]\d{1,2}:\d{2}(?::\d{2})?)?\)",
            // Dataview inline fields.
            r"\[[^\[\]]*::[^\[\]]*\]",
            r"\([^()]*::[^()]*\)",
            // Tasks date annotations and markers with one value token.
            r"(?:📅|📆|🗓️|🗓|✅|❌|➕|🛫|⏳|⌛)\s*\d{4}-\d{2}-\d{2}",
            r"(?:🆔|⛔|🏁)\s*\S+",
            // A recurrence rule runs to the next annotation or the end.
            r"🔁[^📅📆🗓✅❌➕🛫⏳⌛⏫🔺🔼🔽⏬🏁⛔🆔⏰^]*",
            r"[⏫🔺🔼🔽⏬]",
            r"(?:^|\s)\^[A-Za-z0-9-]+\s*$",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid task title pattern"))
        .collect()
    })
}

fn build_reminder(date: &str, time: Option<&str>) -> Option<InlineReminderTime> {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let time = match time {
        // Reminders have minute precision; seconds are accepted and dropped.
        Some(time) => {
            let mut parts = time.splitn(3, ':');
            let hour = parts.next()?.parse::<u32>().ok()?;
            let minute = parts.next()?.parse::<u32>().ok()?;
            if let Some(seconds) = parts.next() {
                seconds
                    .parse::<u32>()
                    .ok()
                    .filter(|seconds| *seconds < 60)?;
            }
            Some(NaiveTime::from_hms_opt(hour, minute, 0)?)
        }
        None => None,
    };
    Some(InlineReminderTime { date, time })
}

fn marker_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"⏰\s*(\d{4}-\d{2}-\d{2})(?:[ T](\d{1,2}:\d{2}(?::\d{2})?))?(?:\b|$)")
            .expect("valid reminder marker pattern")
    })
}

fn paren_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"\(@(\d{4}-\d{2}-\d{2})(?:[ T](\d{1,2}:\d{2}(?::\d{2})?))?\)")
            .expect("valid reminder paren pattern")
    })
}

fn value_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^(\d{4}-\d{2}-\d{2})(?:[ T](\d{1,2}:\d{2}(?::\d{2})?))?$")
            .expect("valid reminder value pattern")
    })
}

fn remind_field_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"\[remind::[^\]]*\]|\(remind::[^)]*\)").expect("valid remind field pattern")
    })
}

fn block_id_suffix() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\s\^[A-Za-z0-9-]+$").expect("valid block id pattern"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(date: &str, time: Option<&str>) -> InlineReminderTime {
        build_reminder(date, time).expect("valid reminder")
    }

    #[test]
    fn extracts_canonical_marker_with_and_without_time() {
        assert_eq!(
            extract_inline_reminder("Call X ⏰ 2026-11-07 10:00 📅 2026-11-08"),
            Some(at("2026-11-07", Some("10:00")))
        );
        assert_eq!(
            extract_inline_reminder("Call X ⏰ 2026-11-07"),
            Some(at("2026-11-07", None))
        );
        assert_eq!(
            extract_inline_reminder("Call X ⏰2026-11-07T08:30"),
            Some(at("2026-11-07", Some("08:30")))
        );
    }

    #[test]
    fn extracts_obsidian_reminder_paren_form() {
        assert_eq!(
            extract_inline_reminder("Call X (@2026-11-07 10:00)"),
            Some(at("2026-11-07", Some("10:00")))
        );
        assert_eq!(
            extract_inline_reminder("Call X (@2026-11-07)"),
            Some(at("2026-11-07", None))
        );
    }

    #[test]
    fn rejects_invalid_dates_and_times() {
        assert_eq!(extract_inline_reminder("⏰ 2026-13-01"), None);
        assert_eq!(
            extract_inline_reminder("⏰ 2026-11-07 25:00"),
            None,
            "an invalid time invalidates the reminder instead of silently dropping it"
        );
        assert_eq!(extract_inline_reminder("no reminder here"), None);
    }

    #[test]
    fn normalizes_property_values() {
        assert_eq!(
            at("2026-11-07", Some("9:05")).to_property_value(),
            "2026-11-07T09:05"
        );
        assert_eq!(at("2026-11-07", None).to_property_value(), "2026-11-07");
        assert_eq!(
            parse_inline_reminder_value("2026-11-07 10:00:30"),
            Some(at("2026-11-07", Some("10:00")))
        );
        assert_eq!(parse_inline_reminder_value("tomorrow"), None);
    }

    #[test]
    fn titles_drop_annotations_fields_and_block_ids() {
        assert_eq!(
            inline_task_title(
                "Call X #work 🔁 every week ⏳ 2026-10-08 📅 2026-10-10 ⏰ 2026-10-09 18:00 🔼 [owner:: me] 🆔 abc ^call"
            ),
            "Call X #work"
        );
        assert_eq!(inline_task_title("Call X (@2026-11-07 10:00)"), "Call X");
        assert_eq!(inline_task_block_id("Call X ^call-x"), Some("call-x"));
        assert_eq!(inline_task_block_id("Call X"), None);
    }

    #[test]
    fn sets_reminder_in_place_or_appends_before_block_id() {
        let snoozed = at("2026-11-08", Some("09:00"));
        assert_eq!(
            set_inline_reminder("- [ ] Call X ⏰ 2026-11-07 10:00 📅 2026-11-09", snoozed),
            "- [ ] Call X ⏰ 2026-11-08 09:00 📅 2026-11-09"
        );
        assert_eq!(
            set_inline_reminder("- [ ] Call X (@2026-11-07 10:00) #work", snoozed),
            "- [ ] Call X ⏰ 2026-11-08 09:00 #work"
        );
        assert_eq!(
            set_inline_reminder("- [ ] Call X ^call-x", snoozed),
            "- [ ] Call X ⏰ 2026-11-08 09:00 ^call-x"
        );
        assert_eq!(
            set_inline_reminder("- [ ] Call X", snoozed),
            "- [ ] Call X ⏰ 2026-11-08 09:00"
        );
        assert_eq!(
            set_inline_reminder("- [ ] Call X [remind:: 2026-11-07T10:00] #work", snoozed),
            "- [ ] Call X ⏰ 2026-11-08 09:00 #work",
            "the field is replaced, so only one reminder remains"
        );
    }
}
