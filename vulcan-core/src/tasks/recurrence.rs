use serde::Serialize;
use serde_json::{Map, Value};

use crate::expression::functions::{date_components, date_field_value, parse_date_like_string};

const DAY_MS: i64 = 86_400_000;
const MAX_OCCURRENCE_DAYS: i64 = 366 * 50;

#[derive(Debug, Clone, Default)]
struct RecurrenceDetails {
    weekdays: Vec<String>,
    month_day: Option<u32>,
    month: Option<u32>,
    count: Option<u32>,
    until: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskRecurrence {
    pub raw: String,
    pub rule: String,
    pub frequency: String,
    pub interval: u32,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub weekdays: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_day: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

#[must_use]
pub fn parse_recurrence_text(text: &str) -> Option<TaskRecurrence> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }

    parse_rrule(raw).or_else(|| parse_human_recurrence(raw))
}

#[must_use]
pub fn parse_task_recurrence(task: &Map<String, Value>) -> Option<TaskRecurrence> {
    task_recurrence_source(task).and_then(parse_recurrence_text)
}

#[must_use]
pub fn task_recurrence_anchor(task: &Map<String, Value>) -> Option<String> {
    task_recurrence_anchor_ms(task).map(format_day)
}

#[must_use]
pub fn task_upcoming_occurrences(
    task: &Map<String, Value>,
    from_ms: i64,
    limit: usize,
) -> Vec<String> {
    let Some(recurrence) = parse_task_recurrence(task) else {
        return Vec::new();
    };
    let Some(anchor_ms) = task_recurrence_anchor_ms(task) else {
        return Vec::new();
    };

    recurrence_occurrences(&recurrence, anchor_ms, from_ms, limit)
}

pub(crate) fn task_recurrence_properties(task: &Map<String, Value>) -> Vec<(String, Value)> {
    let Some(recurrence) = parse_task_recurrence(task) else {
        return Vec::new();
    };

    let mut properties = vec![
        (
            "recurrence".to_string(),
            Value::String(recurrence.raw.clone()),
        ),
        (
            "recurrenceRaw".to_string(),
            Value::String(recurrence.raw.clone()),
        ),
        (
            "recurrenceRule".to_string(),
            Value::String(recurrence.rule.clone()),
        ),
        (
            "recurrenceFrequency".to_string(),
            Value::String(recurrence.frequency.clone()),
        ),
        (
            "recurrenceInterval".to_string(),
            Value::Number(u64::from(recurrence.interval).into()),
        ),
    ];

    if !recurrence.weekdays.is_empty() {
        properties.push((
            "recurrenceWeekdays".to_string(),
            Value::Array(
                recurrence
                    .weekdays
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        ));
    }
    if let Some(month_day) = recurrence.month_day {
        properties.push((
            "recurrenceMonthDay".to_string(),
            Value::Number(u64::from(month_day).into()),
        ));
    }
    if let Some(month) = recurrence.month {
        properties.push((
            "recurrenceMonth".to_string(),
            Value::Number(u64::from(month).into()),
        ));
    }
    if let Some(count) = recurrence.count {
        properties.push((
            "recurrenceCount".to_string(),
            Value::Number(u64::from(count).into()),
        ));
    }
    if let Some(until) = &recurrence.until {
        properties.push(("recurrenceUntil".to_string(), Value::String(until.clone())));
    }
    if let Some(anchor) = task_recurrence_anchor(task) {
        properties.push(("recurrenceAnchor".to_string(), Value::String(anchor)));
    }

    properties
}

pub(crate) fn inject_task_recurrence_fields(task: &mut Map<String, Value>) {
    for (key, value) in task_recurrence_properties(task) {
        if key == "recurrence" {
            task.entry(key).or_insert(value);
        } else {
            task.insert(key, value);
        }
    }
}

fn recurrence_occurrences(
    recurrence: &TaskRecurrence,
    anchor_ms: i64,
    from_ms: i64,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }

    let start_ms = day_start(from_ms).max(anchor_ms);
    let until_ms = recurrence
        .until
        .as_deref()
        .and_then(normalized_date_ms)
        .unwrap_or_else(|| anchor_ms.saturating_add(MAX_OCCURRENCE_DAYS * DAY_MS));
    if until_ms < anchor_ms {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut seen = 0_u32;
    let mut current_ms = anchor_ms;

    while current_ms <= until_ms && results.len() < limit {
        if recurrence_matches(recurrence, anchor_ms, current_ms) {
            seen = seen.saturating_add(1);
            if recurrence.count.map_or(true, |count| seen <= count) && current_ms >= start_ms {
                results.push(format_day(current_ms));
            }
            if recurrence.count.is_some_and(|count| seen >= count) {
                break;
            }
        }
        current_ms = current_ms.saturating_add(DAY_MS);
    }

    results
}

fn recurrence_matches(recurrence: &TaskRecurrence, anchor_ms: i64, current_ms: i64) -> bool {
    if current_ms < anchor_ms {
        return false;
    }

    let interval = i64::from(recurrence.interval.max(1));
    let diff_days = (current_ms - anchor_ms).div_euclid(DAY_MS);
    let (anchor_year, anchor_month, anchor_day, _, _, _, _) = date_components(anchor_ms);
    let (year, month, day, _, _, _, _) = date_components(current_ms);

    match recurrence.frequency.as_str() {
        "daily" => diff_days.rem_euclid(interval) == 0,
        "weekly" => {
            let weekdays = recurrence_weekday_numbers(recurrence, anchor_ms);
            if !weekdays.contains(&weekday_number(current_ms)) {
                return false;
            }

            let anchor_week_start = start_of_iso_week(anchor_ms);
            let current_week_start = start_of_iso_week(current_ms);
            let week_diff = (current_week_start - anchor_week_start).div_euclid(7 * DAY_MS);
            week_diff >= 0 && week_diff.rem_euclid(interval) == 0
        }
        "monthly" => {
            let target_day = recurrence.month_day.map_or(anchor_day, i64::from);
            let month_diff = (year - anchor_year) * 12 + (month - anchor_month);
            month_diff >= 0 && month_diff.rem_euclid(interval) == 0 && day == target_day
        }
        "yearly" => {
            let target_month = recurrence.month.map_or(anchor_month, i64::from);
            let target_day = recurrence.month_day.map_or(anchor_day, i64::from);
            let year_diff = year - anchor_year;
            year_diff >= 0
                && year_diff.rem_euclid(interval) == 0
                && month == target_month
                && day == target_day
        }
        _ => false,
    }
}

fn parse_human_recurrence(raw: &str) -> Option<TaskRecurrence> {
    let lowered = raw.trim().to_ascii_lowercase();
    let text = lowered.trim();
    let text = text.strip_prefix("every ").unwrap_or(text);

    if text == "weekday" {
        return Some(build_recurrence(
            raw,
            "weekly",
            1,
            RecurrenceDetails {
                weekdays: vec![
                    "monday".to_string(),
                    "tuesday".to_string(),
                    "wednesday".to_string(),
                    "thursday".to_string(),
                    "friday".to_string(),
                ],
                ..RecurrenceDetails::default()
            },
        ));
    }

    if let Some(weekday) = parse_weekday_name(text) {
        return Some(build_recurrence(
            raw,
            "weekly",
            1,
            RecurrenceDetails {
                weekdays: vec![weekday.to_string()],
                ..RecurrenceDetails::default()
            },
        ));
    }

    if let Some(day) = parse_month_day_pattern(text) {
        return Some(build_recurrence(
            raw,
            "monthly",
            1,
            RecurrenceDetails {
                month_day: Some(day),
                ..RecurrenceDetails::default()
            },
        ));
    }

    let mut parts = text.split_whitespace().collect::<Vec<_>>();
    if parts.len() == 1 {
        parts.insert(0, "1");
    }

    if parts.len() == 2 {
        let interval = parts[0].parse::<u32>().ok()?;
        let frequency = match parts[1].trim_end_matches('s') {
            "day" => "daily",
            "week" => "weekly",
            "month" => "monthly",
            "year" => "yearly",
            _ => return None,
        };
        return Some(build_recurrence(
            raw,
            frequency,
            interval,
            RecurrenceDetails::default(),
        ));
    }

    None
}

fn parse_rrule(raw: &str) -> Option<TaskRecurrence> {
    let rule = raw.trim().strip_prefix("RRULE:").unwrap_or(raw.trim());
    let mut frequency = None;
    let mut interval = 1_u32;
    let mut details = RecurrenceDetails::default();

    for segment in rule.split(';') {
        let (key, value) = segment.split_once('=')?;
        match key.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                frequency = Some(match value.trim().to_ascii_uppercase().as_str() {
                    "DAILY" => "daily",
                    "WEEKLY" => "weekly",
                    "MONTHLY" => "monthly",
                    "YEARLY" => "yearly",
                    _ => return None,
                });
            }
            "INTERVAL" => {
                interval = value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0)?;
            }
            "BYDAY" => {
                details.weekdays = value
                    .split(',')
                    .map(parse_rrule_weekday)
                    .collect::<Option<Vec<_>>>()?;
            }
            "BYMONTHDAY" => {
                details.month_day = value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|day| (1..=31).contains(day));
            }
            "BYMONTH" => {
                details.month = value
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|month| (1..=12).contains(month));
            }
            "COUNT" => {
                details.count = value.trim().parse::<u32>().ok().filter(|value| *value > 0);
            }
            "UNTIL" => details.until = normalize_rrule_until(value),
            _ => {}
        }
    }

    Some(build_recurrence(raw, frequency?, interval, details))
}

fn build_recurrence(
    raw: &str,
    frequency: &str,
    interval: u32,
    details: RecurrenceDetails,
) -> TaskRecurrence {
    TaskRecurrence {
        raw: raw.trim().to_string(),
        rule: build_rule(
            frequency,
            interval,
            &details.weekdays,
            details.month_day,
            details.month,
            details.count,
            details.until.as_deref(),
        ),
        frequency: frequency.to_string(),
        interval,
        weekdays: details.weekdays,
        month_day: details.month_day,
        month: details.month,
        count: details.count,
        until: details.until,
    }
}

fn build_rule(
    frequency: &str,
    interval: u32,
    weekdays: &[String],
    month_day: Option<u32>,
    month: Option<u32>,
    count: Option<u32>,
    until: Option<&str>,
) -> String {
    let mut parts = vec![
        format!("FREQ={}", frequency.to_ascii_uppercase()),
        format!("INTERVAL={interval}"),
    ];
    if !weekdays.is_empty() {
        parts.push(format!(
            "BYDAY={}",
            weekdays
                .iter()
                .filter_map(|weekday| weekday_to_rrule(weekday))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if let Some(month_day) = month_day {
        parts.push(format!("BYMONTHDAY={month_day}"));
    }
    if let Some(month) = month {
        parts.push(format!("BYMONTH={month}"));
    }
    if let Some(count) = count {
        parts.push(format!("COUNT={count}"));
    }
    if let Some(until) = until {
        parts.push(format!("UNTIL={}", until.replace('-', "")));
    }
    parts.join(";")
}

fn task_recurrence_source(task: &Map<String, Value>) -> Option<&str> {
    for field in ["recurrenceRaw", "repeat", "recurrence"] {
        if let Some(value) = task
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
    }

    None
}

fn task_recurrence_anchor_ms(task: &Map<String, Value>) -> Option<i64> {
    let anchor_fields = match task
        .get("recurrenceAnchor")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some("completion") => ["completion", "done", "scheduled", "start", "due", "created"],
        _ => ["scheduled", "start", "due", "created", "done", "completion"],
    };

    for field in anchor_fields {
        if let Some(value) = task
            .get(field)
            .and_then(Value::as_str)
            .and_then(normalized_date_ms)
        {
            return Some(value);
        }
    }

    None
}

fn normalized_date_ms(text: &str) -> Option<i64> {
    parse_date_like_string(text).map(day_start)
}

fn normalize_rrule_until(text: &str) -> Option<String> {
    normalized_date_string(text).or_else(|| {
        let digits = text
            .chars()
            .filter(char::is_ascii_digit)
            .take(8)
            .collect::<String>();
        if digits.len() != 8 {
            return None;
        }
        Some(format!(
            "{}-{}-{}",
            &digits[0..4],
            &digits[4..6],
            &digits[6..8]
        ))
    })
}

fn normalized_date_string(text: &str) -> Option<String> {
    normalized_date_ms(text).map(format_day)
}

fn parse_month_day_pattern(text: &str) -> Option<u32> {
    let suffix = text.strip_prefix("month on the ")?;
    let digits = suffix
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits
        .parse::<u32>()
        .ok()
        .filter(|day| (1..=31).contains(day))
}

fn parse_weekday_name(text: &str) -> Option<&'static str> {
    match text.trim().trim_end_matches('s') {
        "monday" | "mon" => Some("monday"),
        "tuesday" | "tue" | "tues" => Some("tuesday"),
        "wednesday" | "wed" => Some("wednesday"),
        "thursday" | "thu" | "thur" | "thurs" => Some("thursday"),
        "friday" | "fri" => Some("friday"),
        "saturday" | "sat" => Some("saturday"),
        "sunday" | "sun" => Some("sunday"),
        _ => None,
    }
}

fn parse_rrule_weekday(text: &str) -> Option<String> {
    match text.trim().to_ascii_uppercase().as_str() {
        "MO" => Some("monday".to_string()),
        "TU" => Some("tuesday".to_string()),
        "WE" => Some("wednesday".to_string()),
        "TH" => Some("thursday".to_string()),
        "FR" => Some("friday".to_string()),
        "SA" => Some("saturday".to_string()),
        "SU" => Some("sunday".to_string()),
        _ => None,
    }
}

fn weekday_to_rrule(text: &str) -> Option<&'static str> {
    match text {
        "monday" => Some("MO"),
        "tuesday" => Some("TU"),
        "wednesday" => Some("WE"),
        "thursday" => Some("TH"),
        "friday" => Some("FR"),
        "saturday" => Some("SA"),
        "sunday" => Some("SU"),
        _ => None,
    }
}

fn recurrence_weekday_numbers(recurrence: &TaskRecurrence, anchor_ms: i64) -> Vec<i64> {
    if recurrence.weekdays.is_empty() {
        return vec![weekday_number(anchor_ms)];
    }

    recurrence
        .weekdays
        .iter()
        .filter_map(|weekday| parse_rrule_weekday(weekday_to_rrule(weekday).unwrap_or_default()))
        .filter_map(|weekday| weekday_name_to_number(&weekday))
        .collect()
}

fn weekday_name_to_number(text: &str) -> Option<i64> {
    match text {
        "monday" => Some(1),
        "tuesday" => Some(2),
        "wednesday" => Some(3),
        "thursday" => Some(4),
        "friday" => Some(5),
        "saturday" => Some(6),
        "sunday" => Some(7),
        _ => None,
    }
}

fn weekday_number(ms: i64) -> i64 {
    date_field_value(ms, "weekday")
        .and_then(|value| value.as_i64())
        .unwrap_or(1)
}

fn start_of_iso_week(ms: i64) -> i64 {
    day_start(ms) - (weekday_number(ms) - 1) * DAY_MS
}

fn day_start(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS) * DAY_MS
}

fn format_day(ms: i64) -> String {
    let (year, month, day, _, _, _, _) = date_components(ms);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(fields: &[(&str, Value)]) -> Map<String, Value> {
        fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    fn ms(date: &str) -> i64 {
        normalized_date_ms(date).expect("date should parse")
    }

    #[test]
    fn parses_supported_human_recurrence_patterns() {
        let daily = parse_recurrence_text("every day").expect("daily recurrence should parse");
        assert_eq!(daily.frequency, "daily");
        assert_eq!(daily.interval, 1);
        assert_eq!(daily.rule, "FREQ=DAILY;INTERVAL=1");

        let weekly =
            parse_recurrence_text("every 2 weeks").expect("weekly recurrence should parse");
        assert_eq!(weekly.frequency, "weekly");
        assert_eq!(weekly.interval, 2);
        assert!(weekly.weekdays.is_empty());

        let weekday =
            parse_recurrence_text("every weekday").expect("weekday recurrence should parse");
        assert_eq!(
            weekday.weekdays,
            vec![
                "monday".to_string(),
                "tuesday".to_string(),
                "wednesday".to_string(),
                "thursday".to_string(),
                "friday".to_string()
            ]
        );
        assert_eq!(weekday.rule, "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,TU,WE,TH,FR");

        let monday =
            parse_recurrence_text("Mondays").expect("weekday-name recurrence should parse");
        assert_eq!(monday.weekdays, vec!["monday".to_string()]);

        let monthly = parse_recurrence_text("every month on the 15th")
            .expect("monthly recurrence should parse");
        assert_eq!(monthly.frequency, "monthly");
        assert_eq!(monthly.month_day, Some(15));
        assert_eq!(monthly.rule, "FREQ=MONTHLY;INTERVAL=1;BYMONTHDAY=15");
    }

    #[test]
    fn parses_rrule_subset() {
        let recurrence =
            parse_recurrence_text("RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TH;COUNT=3;UNTIL=20260430")
                .expect("rrule recurrence should parse");

        assert_eq!(recurrence.frequency, "weekly");
        assert_eq!(recurrence.interval, 2);
        assert_eq!(recurrence.weekdays, vec!["thursday".to_string()]);
        assert_eq!(recurrence.count, Some(3));
        assert_eq!(recurrence.until.as_deref(), Some("2026-04-30"));
        assert_eq!(
            recurrence.rule,
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=TH;COUNT=3;UNTIL=20260430"
        );
    }

    #[test]
    fn expands_supported_recurrence_patterns() {
        let weekday_task = task(&[
            ("scheduled", Value::String("2026-03-27".to_string())),
            ("recurrence", Value::String("every weekday".to_string())),
        ]);
        assert_eq!(
            task_upcoming_occurrences(&weekday_task, ms("2026-03-29"), 4),
            vec![
                "2026-03-30".to_string(),
                "2026-03-31".to_string(),
                "2026-04-01".to_string(),
                "2026-04-02".to_string()
            ]
        );

        let monthly_task = task(&[
            ("due", Value::String("2026-02-15".to_string())),
            (
                "repeat",
                Value::String("every month on the 15th".to_string()),
            ),
        ]);
        assert_eq!(
            task_upcoming_occurrences(&monthly_task, ms("2026-03-01"), 3),
            vec![
                "2026-03-15".to_string(),
                "2026-04-15".to_string(),
                "2026-05-15".to_string()
            ]
        );
    }

    #[test]
    fn respects_rrule_count_limits_for_upcoming_occurrences() {
        let rrule_task = task(&[
            ("scheduled", Value::String("2026-03-26".to_string())),
            (
                "repeat",
                Value::String("RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TH;COUNT=3".to_string()),
            ),
        ]);

        assert_eq!(
            task_upcoming_occurrences(&rrule_task, ms("2026-03-29"), 5),
            vec!["2026-04-09".to_string(), "2026-04-23".to_string()]
        );
    }

    #[test]
    fn builds_normalized_recurrence_properties() {
        let recurring_task = task(&[
            ("scheduled", Value::String("2026-03-27".to_string())),
            ("recurrence", Value::String("every weekday".to_string())),
        ]);

        let properties = task_recurrence_properties(&recurring_task)
            .into_iter()
            .collect::<Map<_, _>>();

        assert_eq!(
            properties.get("recurrenceRule"),
            Some(&Value::String(
                "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO,TU,WE,TH,FR".to_string()
            ))
        );
        assert_eq!(
            properties.get("recurrenceFrequency"),
            Some(&Value::String("weekly".to_string()))
        );
        assert_eq!(
            properties.get("recurrenceInterval"),
            Some(&Value::Number(1.into()))
        );
        assert_eq!(
            properties.get("recurrenceWeekdays"),
            Some(&serde_json::json!([
                "monday",
                "tuesday",
                "wednesday",
                "thursday",
                "friday"
            ]))
        );
        assert_eq!(
            properties.get("recurrenceAnchor"),
            Some(&Value::String("2026-03-27".to_string()))
        );
    }

    #[test]
    fn completion_anchor_overrides_scheduled_date_for_tasknotes_tasks() {
        let recurring_task = task(&[
            ("scheduled", Value::String("2026-04-08".to_string())),
            (
                "recurrence",
                Value::String("RRULE:FREQ=WEEKLY;INTERVAL=1;BYDAY=FR".to_string()),
            ),
            ("recurrenceAnchor", Value::String("completion".to_string())),
            ("completion", Value::String("2026-04-04".to_string())),
        ]);

        assert_eq!(
            task_recurrence_anchor(&recurring_task),
            Some("2026-04-04".to_string())
        );
        assert_eq!(
            task_upcoming_occurrences(&recurring_task, ms("2026-04-04"), 3),
            vec![
                "2026-04-10".to_string(),
                "2026-04-17".to_string(),
                "2026-04-24".to_string()
            ]
        );
    }
}
