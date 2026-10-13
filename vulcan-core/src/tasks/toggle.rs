//! Toggling an inline Markdown task the way the Tasks plugin does.
//!
//! Completing an open task sets the first completed status and a `✅` done
//! date. A task with a `🔁` recurrence also gets its next occurrence: a new
//! open line whose reference date (due, else scheduled, else start) moves to
//! its next occurrence (counted from the completion date for `when done`
//! rules), with the other dates moving by the same number of days. Reopening a completed task restores the
//! first todo status and drops the done date.

use super::parse_recurrence_text;
use super::recurrence::{date_ms, format_day, next_occurrence_after, DAY_MS};
use crate::config::TaskStatusesConfig;
use regex::Regex;
use std::sync::OnceLock;

/// Where a recurring task's next occurrence goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrencePlacement {
    /// Above the completed task (the Tasks default).
    Above,
    /// Below the completed task (`recurrence_on_completion = "next-line"`).
    Below,
}

impl RecurrencePlacement {
    /// The placement configured by `tasks.recurrence_on_completion`.
    #[must_use]
    pub fn from_config(value: Option<&str>) -> Self {
        if value == Some("next-line") {
            Self::Below
        } else {
            Self::Above
        }
    }
}

/// The result of toggling one task line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineTaskToggle {
    /// The toggled line.
    pub line: String,
    /// The next occurrence of a completed recurring task.
    pub next_occurrence: Option<String>,
    pub placement: RecurrencePlacement,
    /// Whether the task is now completed.
    pub completed: bool,
}

/// Toggle the task on `line`, completing it on `today` (`YYYY-MM-DD`).
/// Returns `None` when the line is not a task.
#[must_use]
pub fn toggle_inline_task_line(
    line: &str,
    statuses: &TaskStatusesConfig,
    today: &str,
    placement: RecurrencePlacement,
) -> Option<InlineTaskToggle> {
    let captures = checkbox_pattern().captures(line)?;
    let marker = captures.get(2)?;
    let status = marker.as_str();
    let is_completed = statuses.completed.iter().any(|symbol| symbol == status)
        || statuses.cancelled.iter().any(|symbol| symbol == status);
    let set_status = |line: &str, symbol: &str| {
        format!(
            "{}{symbol}{}",
            &line[..marker.start()],
            &line[marker.end()..]
        )
    };

    if is_completed {
        let todo = first_symbol(&statuses.todo, " ");
        // Reopening never removes an occurrence created on completion.
        let reopened = remove_marker(&set_status(line, todo), &["✅"]);
        return Some(InlineTaskToggle {
            line: reopened,
            next_occurrence: None,
            placement,
            completed: false,
        });
    }

    let done = first_symbol(&statuses.completed, "x");
    let completed = set_marker(&set_status(line, done), "✅", today);
    let next_occurrence = next_occurrence_line(line, marker.start(), marker.end(), statuses, today);
    Some(InlineTaskToggle {
        line: completed,
        next_occurrence,
        placement,
        completed: true,
    })
}

/// Complete the task on `line` without toggling: an already completed or
/// cancelled task is returned unchanged. Returns `None` when the line is
/// not a task.
#[must_use]
pub fn complete_inline_task_line(
    line: &str,
    statuses: &TaskStatusesConfig,
    today: &str,
    placement: RecurrencePlacement,
) -> Option<InlineTaskToggle> {
    let status = checkbox_pattern()
        .captures(line)?
        .get(2)?
        .as_str()
        .to_string();
    if statuses.completed.contains(&status) || statuses.cancelled.contains(&status) {
        return Some(InlineTaskToggle {
            line: line.to_string(),
            next_occurrence: None,
            placement,
            completed: true,
        });
    }
    toggle_inline_task_line(line, statuses, today, placement)
}

/// Insert a toggle's result for the task on 0-based `line_index` of
/// `source`. Returns `None` when that line is not a task.
#[must_use]
pub fn apply_inline_task_toggle(
    source: &str,
    line_index: usize,
    toggle: impl FnOnce(&str) -> Option<InlineTaskToggle>,
) -> Option<(String, InlineTaskToggle)> {
    let mut lines = source
        .split('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let current = lines.get(line_index)?;
    // Keep a CRLF line ending on the toggled line and its new occurrence.
    let (body, ending) = match current.strip_suffix('\r') {
        Some(body) => (body.to_string(), "\r"),
        None => (current.clone(), ""),
    };
    let result = toggle(&body)?;
    lines[line_index] = format!("{}{ending}", result.line);
    if let Some(next) = &result.next_occurrence {
        let at = match result.placement {
            RecurrencePlacement::Above => line_index,
            RecurrencePlacement::Below => line_index + 1,
        };
        lines.insert(at, format!("{next}{ending}"));
    }
    Some((lines.join("\n"), result))
}

/// The open copy of a recurring task with its dates moved to the next
/// occurrence.
fn next_occurrence_line(
    line: &str,
    marker_start: usize,
    marker_end: usize,
    statuses: &TaskStatusesConfig,
    today: &str,
) -> Option<String> {
    let rule = marker_segment(line, "🔁")?;
    let (rule_text, when_done) = match rule.strip_suffix("when done") {
        Some(stripped) => (stripped.trim(), true),
        None => (rule.as_str(), false),
    };
    let recurrence = parse_recurrence_text(rule_text)?;
    let dates = [
        ("due", &["📅", "📆", "🗓️", "🗓"][..]),
        ("scheduled", &["⏳", "⌛"][..]),
        ("start", &["🛫"][..]),
    ]
    .into_iter()
    .filter_map(|(_, markers)| marker_date(line, markers))
    .collect::<Vec<_>>();
    // The reference date (due, else scheduled, else start) moves to the
    // next occurrence after itself, or after today for `when done` rules;
    // every other date moves by the same number of days.
    let reference = dates.first().map(|(_, _, date)| date.clone())?;
    let next = next_occurrence_after(&recurrence, if when_done { today } else { &reference })?;
    let shift = (date_ms(&next)? - date_ms(&reference)?) / DAY_MS;

    let todo = first_symbol(&statuses.todo, " ");
    let mut next_line = format!("{}{todo}{}", &line[..marker_start], &line[marker_end..]);
    next_line = remove_marker(&next_line, &["✅", "❌"]);
    // A block id names one task; the new occurrence must not duplicate it.
    next_line = block_id_pattern().replace(&next_line, "").into_owned();
    // Status symbols may differ in width, so dates are re-located in the
    // new line by their markers rather than by byte offsets.
    for (_, marker, date) in &dates {
        let moved = format_day(date_ms(date)? + shift * DAY_MS);
        if let Some((range, _, _)) = marker_date(&next_line, &[marker.as_str()]) {
            next_line.replace_range(range, &moved);
        }
    }
    Some(next_line)
}

/// The due date (`YYYY-MM-DD`) written on a task line, from a Tasks `📅`
/// marker or a Dataview `[due:: ...]` / `(due:: ...)` field.
#[must_use]
pub fn task_line_due_date(line: &str) -> Option<String> {
    if let Some((_, _, date)) = marker_date(line, &["📅", "📆", "🗓️", "🗓"]) {
        return Some(date);
    }
    let lower = line.to_ascii_lowercase();
    let at = lower.find("due::")?;
    let value = line[at + "due::".len()..]
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == ']' || c == ')')
        .next()?;
    date_ms(value)?;
    Some(value.to_string())
}

/// Whole days from `today` to `date` (both `YYYY-MM-DD`): negative when
/// `date` is past.
#[must_use]
pub fn days_between(today: &str, date: &str) -> Option<i64> {
    Some((date_ms(date)? - date_ms(today)?) / DAY_MS)
}

/// The byte range, marker, and value of the first date after any marker.
fn marker_date(line: &str, markers: &[&str]) -> Option<(std::ops::Range<usize>, String, String)> {
    markers.iter().find_map(|marker| {
        let at = line.find(marker)?;
        let rest = &line[at + marker.len()..];
        let skipped = rest.len() - rest.trim_start().len();
        let start = at + marker.len() + skipped;
        let value = line[start..].split_whitespace().next()?;
        date_ms(value)?;
        Some((
            start..start + value.len(),
            (*marker).to_string(),
            value.to_string(),
        ))
    })
}

/// The text after `marker` up to the next Tasks annotation.
fn marker_segment(line: &str, marker: &str) -> Option<String> {
    let at = line.find(marker)?;
    let rest = line[at + marker.len()..].trim_start();
    let end = ANNOTATION_MARKERS
        .iter()
        .filter_map(|candidate| rest.find(candidate))
        .min()
        .unwrap_or(rest.len());
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// `line` with `marker DATE` set to `date`, appended when missing.
fn set_marker(line: &str, marker: &str, date: &str) -> String {
    match marker_date(line, &[marker]) {
        Some((range, _, _)) => format!("{}{date}{}", &line[..range.start], &line[range.end..]),
        None => format!("{} {marker} {date}", line.trim_end()),
    }
}

/// `line` without any `marker DATE` annotations.
fn remove_marker(line: &str, markers: &[&str]) -> String {
    let mut result = line.to_string();
    for marker in markers {
        while let Some(at) = result.find(marker) {
            let after = at + marker.len();
            let rest = &result[after..];
            let skipped = rest.len() - rest.trim_start().len();
            let value_end = rest[skipped..]
                .find(char::is_whitespace)
                .map_or(result.len(), |end| after + skipped + end);
            let start = result[..at].trim_end().len();
            result.replace_range(start..value_end, "");
        }
    }
    result
}

fn first_symbol<'a>(symbols: &'a [String], fallback: &'a str) -> &'a str {
    symbols.first().map_or(fallback, String::as_str)
}

fn block_id_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\s+\^[A-Za-z0-9-]+\s*$").expect("regex should compile"))
}

fn checkbox_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^(\s*(?:>\s*)*(?:[-*+]|\d+[.)])\s+\[)([^\]])\]").expect("regex should compile")
    })
}

const ANNOTATION_MARKERS: &[&str] = &[
    "📅", "📆", "🗓️", "🗓", "✅", "❌", "➕", "🛫", "⏳", "⌛", "⏫", "🔺", "🔼", "🔽", "⏬", "🔁",
    "🏁", "⛔", "🆔",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn toggle(line: &str) -> Option<InlineTaskToggle> {
        toggle_inline_task_line(
            line,
            &TaskStatusesConfig::default(),
            "2026-10-12",
            RecurrencePlacement::Above,
        )
    }

    #[test]
    fn completes_and_reopens_plain_tasks() {
        let done = toggle("- [ ] Write report 📅 2026-10-10").unwrap();
        assert!(done.completed);
        assert_eq!(done.line, "- [x] Write report 📅 2026-10-10 ✅ 2026-10-12");
        assert_eq!(done.next_occurrence, None);

        let reopened = toggle(&done.line).unwrap();
        assert!(!reopened.completed);
        assert_eq!(reopened.line, "- [ ] Write report 📅 2026-10-10");

        assert_eq!(toggle("Not a task"), None);
        assert_eq!(
            toggle("  1. [ ] Nested").unwrap().line,
            "  1. [x] Nested ✅ 2026-10-12"
        );
    }

    #[test]
    fn recurring_tasks_get_their_next_occurrence_with_shifted_dates() {
        let done =
            toggle("- [ ] Water plants 🔁 every week ⏳ 2026-10-08 📅 2026-10-10 ^water").unwrap();
        assert_eq!(
            done.line,
            "- [x] Water plants 🔁 every week ⏳ 2026-10-08 📅 2026-10-10 ^water ✅ 2026-10-12"
        );
        // Due is the reference; every date moves by the same week.
        assert_eq!(
            done.next_occurrence.as_deref(),
            Some("- [ ] Water plants 🔁 every week ⏳ 2026-10-15 📅 2026-10-17")
        );
    }

    #[test]
    fn when_done_rules_count_from_the_completion_date() {
        let done = toggle("- [ ] Haircut 🔁 every 4 weeks when done 📅 2026-01-01").unwrap();
        assert_eq!(
            done.next_occurrence.as_deref(),
            Some("- [ ] Haircut 🔁 every 4 weeks when done 📅 2026-11-09")
        );
    }

    #[test]
    fn recurring_tasks_without_dates_or_rules_only_complete() {
        assert_eq!(
            toggle("- [ ] Ping 🔁 every day").unwrap().next_occurrence,
            None
        );
        assert_eq!(
            toggle("- [ ] Odd 🔁 sometimes 📅 2026-10-10")
                .unwrap()
                .next_occurrence,
            None
        );
    }

    #[test]
    fn completing_never_reopens_and_insertion_follows_placement() {
        let statuses = TaskStatusesConfig::default();
        let done = complete_inline_task_line(
            "- [x] Done",
            &statuses,
            "2026-10-12",
            RecurrencePlacement::Above,
        )
        .unwrap();
        assert_eq!(done.line, "- [x] Done");
        assert!(done.completed);

        let source = "# Tasks\r\n- [ ] Daily 🔁 every day 📅 2026-10-12\r\nafter\r\n";
        for (placement, expected) in [
            (
                RecurrencePlacement::Above,
                "# Tasks\r\n- [ ] Daily 🔁 every day 📅 2026-10-13\r\n- [x] Daily 🔁 every day 📅 2026-10-12 ✅ 2026-10-12\r\nafter\r\n",
            ),
            (
                RecurrencePlacement::Below,
                "# Tasks\r\n- [x] Daily 🔁 every day 📅 2026-10-12 ✅ 2026-10-12\r\n- [ ] Daily 🔁 every day 📅 2026-10-13\r\nafter\r\n",
            ),
        ] {
            let (updated, _) = apply_inline_task_toggle(source, 1, |line| {
                complete_inline_task_line(line, &statuses, "2026-10-12", placement)
            })
            .unwrap();
            assert_eq!(updated, expected);
        }
        assert!(apply_inline_task_toggle(source, 0, |line| {
            complete_inline_task_line(line, &statuses, "2026-10-12", RecurrencePlacement::Above)
        })
        .is_none());
    }

    #[test]
    fn reads_due_dates_from_markers_and_fields() {
        assert_eq!(
            task_line_due_date("- [ ] a 📅 2026-10-14").as_deref(),
            Some("2026-10-14")
        );
        assert_eq!(
            task_line_due_date("- [ ] a [due:: 2026-10-15]").as_deref(),
            Some("2026-10-15")
        );
        assert_eq!(
            task_line_due_date("- [ ] a (due:: 2026-10-16) x").as_deref(),
            Some("2026-10-16")
        );
        assert_eq!(task_line_due_date("- [ ] a ⏳ 2026-10-14"), None);
        assert_eq!(days_between("2026-10-13", "2026-10-16"), Some(3));
        assert_eq!(days_between("2026-10-13", "2026-10-11"), Some(-2));
    }

    #[test]
    fn placement_follows_configuration() {
        assert_eq!(
            RecurrencePlacement::from_config(Some("next-line")),
            RecurrencePlacement::Below
        );
        assert_eq!(
            RecurrencePlacement::from_config(None),
            RecurrencePlacement::Above
        );
    }
}
