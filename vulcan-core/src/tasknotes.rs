use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{
    TaskNotesConfig, TaskNotesDateDefault, TaskNotesIdentificationMethod,
    TaskNotesRecurrenceDefault, TaskNotesStatusConfig, TaskNotesUserFieldType,
};
use crate::expression::functions::{date_components, parse_date_like_string};
use crate::tasks::parse_recurrence_text;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexedTaskNote {
    pub title: String,
    pub status: String,
    pub priority: String,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub completed_date: Option<String>,
    pub date_created: Option<String>,
    pub date_modified: Option<String>,
    pub archived: bool,
    pub tags: Vec<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub time_estimate: Option<f64>,
    pub recurrence: Option<String>,
    pub recurrence_anchor: Option<String>,
    pub complete_instances: Vec<String>,
    pub skipped_instances: Vec<String>,
    pub blocked_by: Vec<Value>,
    pub reminders: Vec<Value>,
    pub time_entries: Vec<Value>,
    pub custom_fields: Map<String, Value>,
}

impl IndexedTaskNote {
    #[must_use]
    pub fn json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskNotesStatusState {
    pub name: String,
    pub status_type: String,
    pub completed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedTaskNoteInput {
    pub title: String,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub due: Option<String>,
    pub scheduled: Option<String>,
    pub contexts: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub time_estimate: Option<usize>,
    pub recurrence: Option<String>,
}

#[must_use]
pub fn parse_tasknote_natural_language(
    text: &str,
    config: &TaskNotesConfig,
    reference_ms: i64,
) -> ParsedTaskNoteInput {
    let original = text.trim();
    if original.is_empty() {
        return ParsedTaskNoteInput::default();
    }

    let mut remaining = original.to_string();
    let mut parsed = ParsedTaskNoteInput {
        contexts: extract_prefixed_values(&mut remaining, nlp_trigger(config, "contexts"), true),
        tags: extract_prefixed_values(&mut remaining, nlp_trigger(config, "tags"), false),
        projects: extract_project_values(&mut remaining, nlp_trigger(config, "projects")),
        status: extract_choice_value(
            &mut remaining,
            nlp_trigger(config, "status"),
            &config
                .statuses
                .iter()
                .map(|status| status.value.as_str())
                .collect::<Vec<_>>(),
        ),
        priority: extract_priority_value(&mut remaining, config),
        time_estimate: extract_time_estimate(&mut remaining),
        recurrence: extract_recurrence(&mut remaining),
        ..Default::default()
    };

    let mut due = extract_date_value(&mut remaining, "due", reference_ms);
    let mut scheduled = extract_date_value(&mut remaining, "scheduled", reference_ms)
        .or_else(|| extract_date_value(&mut remaining, "schedule", reference_ms));
    if due.is_none() && scheduled.is_none() {
        let default_field = if config.nlp_default_to_scheduled {
            "scheduled"
        } else {
            "due"
        };
        if let Some(value) = extract_ambiguous_date_value(&mut remaining, reference_ms) {
            match default_field {
                "scheduled" => scheduled = Some(value),
                _ => due = Some(value),
            }
        }
    }
    parsed.due = due;
    parsed.scheduled = scheduled;

    let normalized_title = normalize_nlp_title(&remaining);
    parsed.title = if normalized_title.is_empty() {
        original.to_string()
    } else {
        normalized_title
    };
    parsed
}

#[must_use]
pub fn extract_tasknote(
    document_path: &str,
    document_title: &str,
    properties: &Value,
    config: &TaskNotesConfig,
) -> Option<IndexedTaskNote> {
    let object = properties.as_object()?;
    if !is_tasknote_document(document_path, object, config) {
        return None;
    }

    let mapping = &config.field_mapping;
    let title = string_field(object, &mapping.title).unwrap_or_else(|| document_title.to_string());
    let status = status_value(object, config);
    let priority =
        string_field(object, &mapping.priority).unwrap_or_else(|| config.default_priority.clone());
    let tags = string_list_field(object, "tags");
    let archived = tags
        .iter()
        .any(|tag| normalized_tag(tag) == normalized_tag(&mapping.archive_tag));

    Some(IndexedTaskNote {
        title,
        status,
        priority,
        due: string_field(object, &mapping.due),
        scheduled: string_field(object, &mapping.scheduled),
        completed_date: string_field(object, &mapping.completed_date),
        date_created: string_field(object, &mapping.date_created),
        date_modified: string_field(object, &mapping.date_modified),
        archived,
        tags,
        contexts: string_list_field(object, &mapping.contexts),
        projects: string_list_field(object, &mapping.projects),
        time_estimate: numeric_field(object, &mapping.time_estimate),
        recurrence: string_field(object, &mapping.recurrence),
        recurrence_anchor: recurrence_anchor_field(object, &mapping.recurrence_anchor),
        complete_instances: string_list_field(object, &mapping.complete_instances),
        skipped_instances: string_list_field(object, &mapping.skipped_instances),
        blocked_by: value_list_field(object, &mapping.blocked_by),
        reminders: value_list_field(object, &mapping.reminders),
        time_entries: value_list_field(object, &mapping.time_entries),
        custom_fields: custom_fields(object, config),
    })
}

#[must_use]
pub fn is_tasknote_document(
    document_path: &str,
    properties: &Map<String, Value>,
    config: &TaskNotesConfig,
) -> bool {
    if is_excluded_path(document_path, &config.excluded_folders) {
        return false;
    }

    match config.identification_method {
        TaskNotesIdentificationMethod::Tag => {
            let task_tag = normalized_tag(&config.task_tag);
            string_list_field(properties, "tags")
                .iter()
                .any(|tag| normalized_tag(tag) == task_tag)
        }
        TaskNotesIdentificationMethod::Property => {
            if let Some(property_name) = &config.task_property_name {
                let Some(value) = properties.get(property_name) else {
                    return false;
                };
                if let Some(expected) = &config.task_property_value {
                    return value_matches_text(value, expected);
                }
                return true;
            }

            let mapping = &config.field_mapping;
            properties.contains_key(&mapping.status) && properties.contains_key(&mapping.priority)
        }
    }
}

#[must_use]
pub fn tasknotes_status_state(config: &TaskNotesConfig, status: &str) -> TaskNotesStatusState {
    let normalized = status.trim().to_ascii_lowercase();
    let definition = config
        .statuses
        .iter()
        .find(|candidate| candidate.value.trim().eq_ignore_ascii_case(status))
        .cloned()
        .unwrap_or_else(|| fallback_status_definition(config, &normalized));
    let status_type = if definition.is_completed {
        "DONE"
    } else if matches!(
        normalized.as_str(),
        "in-progress" | "in_progress" | "in progress" | "started" | "doing"
    ) {
        "IN_PROGRESS"
    } else if matches!(normalized.as_str(), "cancelled" | "canceled" | "abandoned") {
        "CANCELLED"
    } else {
        "TODO"
    };

    TaskNotesStatusState {
        name: definition.label,
        status_type: status_type.to_string(),
        completed: definition.is_completed,
    }
}

#[must_use]
pub fn tasknotes_priority_weight(config: &TaskNotesConfig, priority: &str) -> Option<f64> {
    config
        .priorities
        .iter()
        .find(|candidate| candidate.value.eq_ignore_ascii_case(priority))
        .map(|candidate| f64::from(candidate.weight))
}

#[must_use]
pub fn tasknotes_default_date_value(
    default: TaskNotesDateDefault,
    reference_ms: i64,
) -> Option<String> {
    let day_ms = day_start(reference_ms);
    let resolved = match default {
        TaskNotesDateDefault::None => return None,
        TaskNotesDateDefault::Today => day_ms,
        TaskNotesDateDefault::Tomorrow => day_ms + DAY_MS,
        TaskNotesDateDefault::NextWeek => day_ms + (7 * DAY_MS),
    };
    Some(format_day_ms(resolved))
}

#[must_use]
pub fn tasknotes_default_recurrence_rule(default: TaskNotesRecurrenceDefault) -> Option<String> {
    match default {
        TaskNotesRecurrenceDefault::None => None,
        TaskNotesRecurrenceDefault::Daily => Some("FREQ=DAILY;INTERVAL=1".to_string()),
        TaskNotesRecurrenceDefault::Weekly => Some("FREQ=WEEKLY;INTERVAL=1".to_string()),
        TaskNotesRecurrenceDefault::Monthly => Some("FREQ=MONTHLY;INTERVAL=1".to_string()),
        TaskNotesRecurrenceDefault::Yearly => Some("FREQ=YEARLY;INTERVAL=1".to_string()),
    }
}

fn nlp_trigger<'a>(config: &'a TaskNotesConfig, property_id: &str) -> Option<&'a str> {
    config
        .nlp_triggers
        .iter()
        .find(|trigger| trigger.enabled && trigger.property_id.eq_ignore_ascii_case(property_id))
        .map(|trigger| trigger.trigger.as_str())
}

fn extract_prefixed_values(
    text: &mut String,
    trigger: Option<&str>,
    keep_prefix: bool,
) -> Vec<String> {
    let Some(trigger) = trigger.filter(|trigger| !trigger.is_empty()) else {
        return Vec::new();
    };
    let regex = Regex::new(&format!(
        r#"(?P<prefix>^|\s){}(?P<value>"[^"]+"|[^\s]+)"#,
        regex::escape(trigger)
    ))
    .expect("valid NLP prefix regex");
    let values = regex
        .captures_iter(text)
        .filter_map(|capture| {
            let value = capture.name("value")?.as_str().trim_matches('"').trim();
            if value.is_empty() {
                None
            } else if keep_prefix && !value.starts_with('@') {
                Some(format!("@{value}"))
            } else {
                Some(value.to_string())
            }
        })
        .collect::<Vec<_>>();
    *text = regex.replace_all(text, "$prefix").into_owned();
    dedup_preserve_order(values)
}

fn extract_project_values(text: &mut String, trigger: Option<&str>) -> Vec<String> {
    let Some(trigger) = trigger.filter(|trigger| !trigger.is_empty()) else {
        return Vec::new();
    };
    let regex = Regex::new(&format!(
        r#"(?P<prefix>^|\s){}(?P<value>\[\[[^\]]+\]\]|"[^"]+"|[^\s]+)"#,
        regex::escape(trigger)
    ))
    .expect("valid NLP project regex");
    let values = regex
        .captures_iter(text)
        .filter_map(|capture| {
            let raw = capture.name("value")?.as_str().trim_matches('"').trim();
            if raw.is_empty() {
                None
            } else if raw.starts_with("[[") && raw.ends_with("]]") {
                Some(raw.to_string())
            } else {
                Some(format!("[[{raw}]]"))
            }
        })
        .collect::<Vec<_>>();
    *text = regex.replace_all(text, "$prefix").into_owned();
    dedup_preserve_order(values)
}

fn extract_choice_value(
    text: &mut String,
    trigger: Option<&str>,
    choices: &[&str],
) -> Option<String> {
    let trigger = trigger.filter(|trigger| !trigger.is_empty())?;
    let regex = Regex::new(&format!(
        r#"(?P<prefix>^|\s){}(?P<value>"[^"]+"|[^\s]+)"#,
        regex::escape(trigger)
    ))
    .expect("valid NLP choice regex");
    let capture = regex.captures(text)?;
    let raw = capture
        .name("value")
        .map(|value| value.as_str().trim_matches('"').trim().to_string())?;
    *text = regex.replace(text, "$prefix").into_owned();
    choices
        .iter()
        .find(|choice| choice.eq_ignore_ascii_case(&raw))
        .map(|choice| (*choice).to_string())
        .or(Some(raw))
}

fn extract_priority_value(text: &mut String, config: &TaskNotesConfig) -> Option<String> {
    if let Some(priority) = extract_choice_value(
        text,
        nlp_trigger(config, "priority"),
        &config
            .priorities
            .iter()
            .map(|priority| priority.value.as_str())
            .collect::<Vec<_>>(),
    ) {
        return Some(priority);
    }

    for priority in &config.priorities {
        let phrases = [
            format!("{} priority", priority.value),
            format!("{} priority", priority.label.to_ascii_lowercase()),
        ];
        for phrase in phrases {
            if remove_phrase_case_insensitive(text, &phrase).is_some() {
                return Some(priority.value.clone());
            }
        }
    }

    if remove_phrase_case_insensitive(text, "urgent").is_some() {
        if let Some(priority) = config
            .priorities
            .iter()
            .find(|priority| priority.value.eq_ignore_ascii_case("urgent"))
        {
            return Some(priority.value.clone());
        }
        if let Some(priority) = config
            .priorities
            .iter()
            .find(|priority| priority.value.eq_ignore_ascii_case("high"))
        {
            return Some(priority.value.clone());
        }
    }

    None
}

fn extract_time_estimate(text: &mut String) -> Option<usize> {
    let compact = Regex::new(r"(?i)(?P<prefix>^|\s)~?(?P<hours>\d+)h(?P<minutes>\d+)m\b")
        .expect("valid compact estimate regex");
    if let Some(capture) = compact.captures(text) {
        let hours = capture
            .name("hours")
            .and_then(|value| value.as_str().parse::<usize>().ok())
            .unwrap_or_default();
        let minutes = capture
            .name("minutes")
            .and_then(|value| value.as_str().parse::<usize>().ok())
            .unwrap_or_default();
        *text = compact.replace(text, "$prefix").into_owned();
        return Some((hours * 60) + minutes);
    }

    let simple = Regex::new(
        r"(?i)(?P<prefix>^|\s)~?(?P<amount>\d+)\s*(?P<unit>h|hr|hrs|hour|hours|m|min|mins|minute|minutes)\b",
    )
    .expect("valid estimate regex");
    let capture = simple.captures(text)?;
    let amount = capture
        .name("amount")
        .and_then(|value| value.as_str().parse::<usize>().ok())?;
    let unit = capture.name("unit")?.as_str().to_ascii_lowercase();
    *text = simple.replace(text, "$prefix").into_owned();
    Some(if unit.starts_with('h') {
        amount * 60
    } else {
        amount
    })
}

fn extract_recurrence(text: &mut String) -> Option<String> {
    let regex = Regex::new(
        r"(?i)(?P<prefix>^|\s)(?P<phrase>every weekday|every day|every week|every month|every year|every monday|every tuesday|every wednesday|every thursday|every friday|every saturday|every sunday|daily|weekly|monthly|yearly)\b",
    )
    .expect("valid recurrence regex");
    let capture = regex.captures(text)?;
    let phrase = capture.name("phrase")?.as_str().to_string();
    *text = regex.replace(text, "$prefix").into_owned();
    parse_recurrence_text(&phrase).map(|recurrence| recurrence.rule)
}

fn extract_date_value(text: &mut String, keyword: &str, reference_ms: i64) -> Option<String> {
    let regex = Regex::new(&format!(
        r"(?i)(?P<prefix>^|\s){}(?P<separator>\s+)(?P<phrase>today|tomorrow|next week|in \d+ days?|next monday|next tuesday|next wednesday|next thursday|next friday|next saturday|next sunday|monday|tuesday|wednesday|thursday|friday|saturday|sunday|[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}|(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\s+\d{{1,2}}(?:st|nd|rd|th)?(?:,\s*\d{{4}})?)(?:\s+at\s+(?P<time>\d{{1,2}}(?::\d{{2}})?\s*(?:am|pm)?))?",
        regex::escape(keyword)
    ))
    .expect("valid explicit date regex");
    let capture = regex.captures(text)?;
    let phrase = capture.name("phrase")?.as_str();
    let time = capture.name("time").map(|value| value.as_str());
    let value = parse_nlp_date_phrase(phrase, time, reference_ms)?;
    *text = regex.replace(text, "$prefix").into_owned();
    Some(value)
}

fn extract_ambiguous_date_value(text: &mut String, reference_ms: i64) -> Option<String> {
    let regex = Regex::new(
        r"(?i)(?P<prefix>^|\s)(?P<phrase>today|tomorrow|next week|in \d+ days?|next monday|next tuesday|next wednesday|next thursday|next friday|next saturday|next sunday|monday|tuesday|wednesday|thursday|friday|saturday|sunday|[0-9]{4}-[0-9]{2}-[0-9]{2}|(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\s+\d{1,2}(?:st|nd|rd|th)?(?:,\s*\d{4})?)(?:\s+at\s+(?P<time>\d{1,2}(?::\d{2})?\s*(?:am|pm)?))?",
    )
    .expect("valid ambiguous date regex");
    let capture = regex.captures(text)?;
    let phrase = capture.name("phrase")?.as_str();
    let time = capture.name("time").map(|value| value.as_str());
    let value = parse_nlp_date_phrase(phrase, time, reference_ms)?;
    *text = regex.replace(text, "$prefix").into_owned();
    Some(value)
}

fn parse_nlp_date_phrase(phrase: &str, time: Option<&str>, reference_ms: i64) -> Option<String> {
    let normalized = phrase.trim().to_ascii_lowercase();
    let day_ms = if normalized == "today" {
        day_start(reference_ms)
    } else if normalized == "tomorrow" {
        day_start(reference_ms) + DAY_MS
    } else if normalized == "next week" {
        day_start(reference_ms) + (7 * DAY_MS)
    } else if let Some(days) = normalized
        .strip_prefix("in ")
        .and_then(|value| value.strip_suffix(" days"))
        .or_else(|| {
            normalized
                .strip_prefix("in ")
                .and_then(|value| value.strip_suffix(" day"))
        })
        .and_then(|value| value.parse::<i64>().ok())
    {
        day_start(reference_ms) + (days * DAY_MS)
    } else if let Some(target_weekday) = weekday_name_to_index(&normalized) {
        next_weekday(reference_ms, target_weekday, false)
    } else if let Some(target_weekday) = normalized
        .strip_prefix("next ")
        .and_then(weekday_name_to_index)
    {
        next_weekday(reference_ms, target_weekday, false)
    } else if let Some(parsed) = parse_month_day_phrase(&normalized, reference_ms) {
        parsed
    } else {
        parse_date_like_string(phrase).map(day_start)?
    };

    if let Some(time_text) = time {
        let (hour, minute) = parse_clock_time(time_text)?;
        return Some(format!(
            "{}T{:02}:{:02}:00",
            format_day_ms(day_ms),
            hour,
            minute
        ));
    }

    Some(format_day_ms(day_ms))
}

fn parse_month_day_phrase(phrase: &str, reference_ms: i64) -> Option<i64> {
    let regex = Regex::new(
        r"(?i)^(?P<month>jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\s+(?P<day>\d{1,2})(?:st|nd|rd|th)?(?:,\s*(?P<year>\d{4}))?$",
    )
    .expect("valid month-day regex");
    let capture = regex.captures(phrase)?;
    let month = month_name_to_number(capture.name("month")?.as_str())?;
    let day = capture
        .name("day")
        .and_then(|value| value.as_str().parse::<i64>().ok())?;
    let reference_day = day_start(reference_ms);
    let (reference_year, _, _, _, _, _, _) = date_components(reference_day);
    let year = capture
        .name("year")
        .and_then(|value| value.as_str().parse::<i64>().ok())
        .unwrap_or(reference_year);
    let base = parse_date_like_string(&format!("{year:04}-{month:02}-{day:02}"))?;
    let candidate = if capture.name("year").is_none() && base < reference_day {
        parse_date_like_string(&format!("{:04}-{month:02}-{day:02}", year + 1))?
    } else {
        base
    };
    Some(day_start(candidate))
}

fn parse_clock_time(text: &str) -> Option<(i64, i64)> {
    let regex =
        Regex::new(r"(?i)^(?P<hour>\d{1,2})(?::(?P<minute>\d{2}))?\s*(?P<ampm>am|pm)?$").ok()?;
    let capture = regex.captures(text.trim())?;
    let mut hour = capture
        .name("hour")
        .and_then(|value| value.as_str().parse::<i64>().ok())?;
    let minute = capture
        .name("minute")
        .and_then(|value| value.as_str().parse::<i64>().ok())
        .unwrap_or(0);
    if let Some(ampm) = capture
        .name("ampm")
        .map(|value| value.as_str().to_ascii_lowercase())
    {
        if hour == 12 {
            hour = 0;
        }
        if ampm == "pm" {
            hour += 12;
        }
    }
    (hour < 24 && minute < 60).then_some((hour, minute))
}

fn normalize_nlp_title(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|character: char| matches!(character, ',' | ';' | ':' | '-' | '(' | ')'))
        .trim()
        .to_string()
}

fn dedup_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut deduped: Vec<String> = Vec::new();
    for value in values {
        if !deduped
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
        {
            deduped.push(value);
        }
    }
    deduped
}

fn remove_phrase_case_insensitive(text: &mut String, phrase: &str) -> Option<String> {
    let regex = Regex::new(&format!(
        r"(?i)(?P<prefix>^|\s)(?P<phrase>{})\b",
        regex::escape(phrase)
    ))
    .expect("valid phrase regex");
    let capture = regex.captures(text)?;
    let value = capture.name("phrase")?.as_str().to_ascii_lowercase();
    *text = regex.replace(text, "$prefix").into_owned();
    Some(value)
}

fn weekday_name_to_index(text: &str) -> Option<i64> {
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

fn month_name_to_number(text: &str) -> Option<i64> {
    match text {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "sept" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => None,
    }
}

fn next_weekday(reference_ms: i64, target: i64, force_next_week: bool) -> i64 {
    let current_day = day_start(reference_ms);
    let current_weekday = weekday_number(current_day);
    let mut delta = (target - current_weekday).rem_euclid(7);
    if force_next_week || delta == 0 {
        delta += 7;
    }
    current_day + (delta * DAY_MS)
}

fn weekday_number(ms: i64) -> i64 {
    ((ms.div_euclid(DAY_MS) + 3).rem_euclid(7)) + 1
}

fn day_start(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS) * DAY_MS
}

fn format_day_ms(ms: i64) -> String {
    let (year, month, day, _, _, _, _) = date_components(ms);
    format!("{year:04}-{month:02}-{day:02}")
}

const DAY_MS: i64 = 86_400_000;

fn custom_fields(properties: &Map<String, Value>, config: &TaskNotesConfig) -> Map<String, Value> {
    let reserved = config.field_mapping.reserved_property_names();
    let typed_fields = config
        .user_fields
        .iter()
        .map(|field| (field.key.as_str(), field.field_type))
        .collect::<std::collections::HashMap<_, _>>();
    let mut custom_fields = Map::new();

    for (key, value) in properties {
        if reserved.contains(key.as_str()) {
            continue;
        }
        if let Some(field_type) = typed_fields.get(key.as_str()) {
            if let Some(normalized) = normalize_user_field_value(value, *field_type) {
                custom_fields.insert(key.clone(), normalized);
            }
            continue;
        }
        custom_fields.insert(key.clone(), value.clone());
    }

    custom_fields
}

fn normalize_user_field_value(value: &Value, field_type: TaskNotesUserFieldType) -> Option<Value> {
    match field_type {
        TaskNotesUserFieldType::Number => value.as_f64().map(number_value),
        TaskNotesUserFieldType::Text | TaskNotesUserFieldType::Date => {
            string_scalar(value).map(Value::String)
        }
        TaskNotesUserFieldType::Boolean => value.as_bool().map(Value::Bool),
        TaskNotesUserFieldType::List => Some(Value::Array(
            string_list_from_value(value)
                .into_iter()
                .map(Value::String)
                .collect(),
        )),
    }
}

fn fallback_status_definition(config: &TaskNotesConfig, normalized: &str) -> TaskNotesStatusConfig {
    if normalized == "true" {
        return config
            .statuses
            .iter()
            .find(|status| status.is_completed)
            .cloned()
            .unwrap_or_else(|| TaskNotesStatusConfig {
                id: "done".to_string(),
                value: "done".to_string(),
                label: "Done".to_string(),
                color: "#16a34a".to_string(),
                is_completed: true,
                order: 0,
                auto_archive: false,
                auto_archive_delay: 5,
            });
    }

    if normalized == "false" {
        return config
            .statuses
            .iter()
            .find(|status| !status.is_completed)
            .cloned()
            .unwrap_or_else(|| TaskNotesStatusConfig {
                id: "open".to_string(),
                value: config.default_status.clone(),
                label: "Open".to_string(),
                color: "#808080".to_string(),
                is_completed: false,
                order: 0,
                auto_archive: false,
                auto_archive_delay: 5,
            });
    }

    TaskNotesStatusConfig {
        id: normalized.to_string(),
        value: normalized.to_string(),
        label: normalized.to_string(),
        color: "#808080".to_string(),
        is_completed: false,
        order: 0,
        auto_archive: false,
        auto_archive_delay: 5,
    }
}

fn status_value(properties: &Map<String, Value>, config: &TaskNotesConfig) -> String {
    let value = properties.get(&config.field_mapping.status);
    match value {
        Some(Value::Bool(flag)) => {
            if *flag {
                config
                    .statuses
                    .iter()
                    .find(|status| status.is_completed)
                    .map_or_else(|| "done".to_string(), |status| status.value.clone())
            } else {
                config.default_status.clone()
            }
        }
        Some(_) => string_field(properties, &config.field_mapping.status)
            .unwrap_or_else(|| config.default_status.clone()),
        None => config.default_status.clone(),
    }
}

fn recurrence_anchor_field(properties: &Map<String, Value>, key: &str) -> Option<String> {
    let value = string_field(properties, key)?;
    match value.as_str() {
        "scheduled" | "completion" => Some(value),
        _ => None,
    }
}

fn string_field(properties: &Map<String, Value>, key: &str) -> Option<String> {
    properties.get(key).and_then(string_scalar)
}

fn numeric_field(properties: &Map<String, Value>, key: &str) -> Option<f64> {
    properties.get(key).and_then(|value| match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    })
}

fn string_list_field(properties: &Map<String, Value>, key: &str) -> Vec<String> {
    properties
        .get(key)
        .map_or_else(Vec::new, string_list_from_value)
}

fn value_list_field(properties: &Map<String, Value>, key: &str) -> Vec<Value> {
    properties
        .get(key)
        .map_or_else(Vec::new, value_list_from_value)
}

fn string_list_from_value(value: &Value) -> Vec<String> {
    match value {
        Value::Array(values) => values.iter().filter_map(string_scalar).collect(),
        Value::String(text) => split_multivalue_string(text),
        other => string_scalar(other).into_iter().collect(),
    }
}

fn value_list_from_value(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(values) => values.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

fn split_multivalue_string(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.contains(',') {
        trimmed
            .split(',')
            .filter_map(|item| string_scalar(&Value::String(item.to_string())))
            .collect()
    } else {
        vec![trimmed.to_string()]
    }
}

fn string_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn normalized_tag(tag: &str) -> String {
    tag.trim().trim_start_matches('#').to_ascii_lowercase()
}

fn value_matches_text(value: &Value, expected: &str) -> bool {
    let expected = expected.trim();
    match value {
        Value::Array(values) => values.iter().any(|item| value_matches_text(item, expected)),
        Value::String(text) => text.trim().eq_ignore_ascii_case(expected),
        Value::Bool(flag) => flag.to_string().eq_ignore_ascii_case(expected),
        Value::Number(number) => number.to_string().eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn is_excluded_path(document_path: &str, excluded_folders: &[String]) -> bool {
    excluded_folders.iter().any(|folder| {
        let normalized = folder.trim().trim_matches('/');
        !normalized.is_empty()
            && (document_path == normalized || document_path.starts_with(&format!("{normalized}/")))
    })
}

fn number_value(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::config::{
        TaskNotesConfig, TaskNotesDateDefault, TaskNotesIdentificationMethod,
        TaskNotesRecurrenceDefault, TaskNotesUserFieldConfig, TaskNotesUserFieldType,
    };

    use super::*;

    #[test]
    fn extracts_tag_identified_tasknotes_with_mapped_fields() {
        let mut config = TaskNotesConfig::default();
        config.field_mapping.due = "deadline".to_string();
        config.user_fields = vec![TaskNotesUserFieldConfig {
            id: "effort".to_string(),
            display_name: "Effort".to_string(),
            key: "effort".to_string(),
            field_type: TaskNotesUserFieldType::Number,
        }];

        let properties = json!({
            "title": "Write docs",
            "status": "in-progress",
            "priority": "high",
            "deadline": "2026-04-10",
            "tags": ["task", "docs"],
            "contexts": ["@desk"],
            "projects": ["[[Website]]"],
            "blockedBy": [{"uid": "[[Prep]]", "reltype": "FINISHTOSTART"}],
            "reminders": [{"id": "r1", "type": "relative"}],
            "timeEntries": [{"startTime": "2026-04-01T10:00:00Z"}],
            "effort": 3,
        });

        let indexed = extract_tasknote(
            "TaskNotes/Tasks/Write docs.md",
            "Write docs",
            &properties,
            &config,
        )
        .expect("tasknote should be indexed");

        assert_eq!(indexed.title, "Write docs");
        assert_eq!(indexed.status, "in-progress");
        assert_eq!(indexed.priority, "high");
        assert_eq!(indexed.due.as_deref(), Some("2026-04-10"));
        assert_eq!(indexed.contexts, vec!["@desk".to_string()]);
        assert_eq!(indexed.projects, vec!["[[Website]]".to_string()]);
        assert_eq!(indexed.blocked_by.len(), 1);
        assert_eq!(indexed.reminders.len(), 1);
        assert_eq!(indexed.time_entries.len(), 1);
        assert_eq!(indexed.custom_fields.get("effort"), Some(&json!(3.0)));
    }

    #[test]
    fn supports_property_identification_with_status_priority_fallback() {
        let mut config = TaskNotesConfig::default();
        config.identification_method = TaskNotesIdentificationMethod::Property;

        let fallback = json!({
            "title": "Fallback task",
            "status": "open",
            "priority": "normal"
        });
        assert!(extract_tasknote(
            "TaskNotes/Tasks/Fallback.md",
            "Fallback",
            &fallback,
            &config
        )
        .is_some());

        config.task_property_name = Some("isTask".to_string());
        config.task_property_value = Some("yes".to_string());
        let explicit = json!({
            "title": "Explicit task",
            "status": "open",
            "priority": "normal",
            "isTask": "yes"
        });
        assert!(extract_tasknote(
            "TaskNotes/Tasks/Explicit.md",
            "Explicit",
            &explicit,
            &config
        )
        .is_some());

        let not_a_task = json!({
            "title": "Project note",
            "status": "open",
            "priority": "normal",
            "isTask": "no"
        });
        assert!(extract_tasknote("Projects/Project.md", "Project", &not_a_task, &config).is_none());
    }

    #[test]
    fn excludes_configured_folders_and_computes_archived_tag() {
        let mut config = TaskNotesConfig::default();
        config.excluded_folders = vec!["TaskNotes/Archive".to_string()];

        let archived = json!({
            "title": "Archived task",
            "status": true,
            "priority": "low",
            "tags": ["task", "archived"]
        });

        assert!(extract_tasknote(
            "TaskNotes/Archive/Archived task.md",
            "Archived task",
            &archived,
            &config
        )
        .is_none());

        let indexed = extract_tasknote(
            "TaskNotes/Tasks/Archived task.md",
            "Archived task",
            &archived,
            &config,
        )
        .expect("task outside excluded folders should index");

        assert_eq!(indexed.status, "done");
        assert!(indexed.archived);
    }

    #[test]
    fn maps_status_values_into_unified_task_categories() {
        let config = TaskNotesConfig::default();

        assert_eq!(tasknotes_status_state(&config, "done").status_type, "DONE");
        assert_eq!(
            tasknotes_status_state(&config, "in-progress").status_type,
            "IN_PROGRESS"
        );
        assert_eq!(tasknotes_status_state(&config, "open").status_type, "TODO");
    }

    #[test]
    fn parses_natural_language_task_with_tags_contexts_dates_and_priority() {
        let config = TaskNotesConfig::default();

        let parsed = parse_tasknote_natural_language(
            "Buy groceries tomorrow at 3pm @home #errands high priority",
            &config,
            parse_date_like_string("2026-04-04").expect("reference date should parse"),
        );

        assert_eq!(parsed.title, "Buy groceries");
        assert_eq!(parsed.priority.as_deref(), Some("high"));
        assert_eq!(parsed.due.as_deref(), Some("2026-04-05T15:00:00"));
        assert_eq!(parsed.contexts, vec!["@home".to_string()]);
        assert_eq!(parsed.tags, vec!["errands".to_string()]);
    }

    #[test]
    fn parses_natural_language_projects_estimates_and_recurrence() {
        let config = TaskNotesConfig::default();

        let parsed = parse_tasknote_natural_language(
            "Water plants every monday +[[Projects/Home Ops]] ~30m",
            &config,
            parse_date_like_string("2026-04-04").expect("reference date should parse"),
        );

        assert_eq!(parsed.title, "Water plants");
        assert_eq!(parsed.projects, vec!["[[Projects/Home Ops]]".to_string()]);
        assert_eq!(parsed.time_estimate, Some(30));
        assert_eq!(
            parsed.recurrence.as_deref(),
            Some("FREQ=WEEKLY;INTERVAL=1;BYDAY=MO")
        );
    }

    #[test]
    fn honors_nlp_default_to_scheduled_for_ambiguous_dates() {
        let mut config = TaskNotesConfig::default();
        config.nlp_default_to_scheduled = true;

        let parsed = parse_tasknote_natural_language(
            "Plan sprint next monday",
            &config,
            parse_date_like_string("2026-04-04").expect("reference date should parse"),
        );

        assert_eq!(parsed.title, "Plan sprint");
        assert_eq!(parsed.due, None);
        assert_eq!(parsed.scheduled.as_deref(), Some("2026-04-06"));
    }

    #[test]
    fn tasknotes_creation_defaults_expand_dates_and_recurrence() {
        let reference_ms =
            parse_date_like_string("2026-04-04").expect("reference date should parse");

        assert_eq!(
            tasknotes_default_date_value(TaskNotesDateDefault::Today, reference_ms).as_deref(),
            Some("2026-04-04")
        );
        assert_eq!(
            tasknotes_default_date_value(TaskNotesDateDefault::NextWeek, reference_ms).as_deref(),
            Some("2026-04-11")
        );
        assert_eq!(
            tasknotes_default_recurrence_rule(TaskNotesRecurrenceDefault::Monthly).as_deref(),
            Some("FREQ=MONTHLY;INTERVAL=1")
        );
    }
}
