use crate::config::{PeriodicCadenceUnit, PeriodicConfig, PeriodicNoteConfig, PeriodicStartOfWeek};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodicNoteMatch {
    pub period_type: String,
    pub start_date: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DateParts {
    year: i64,
    month: i64,
    day: i64,
}

impl DateParts {
    fn iso_string(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn month_index(self) -> i64 {
        self.year * 12 + (self.month - 1)
    }

    fn quarter_index(self) -> i64 {
        self.year * 4 + (quarter_for_month(self.month) - 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PeriodDefinition {
    unit: PeriodicCadenceUnit,
    interval: i64,
    anchor: DateParts,
    start_of_week: PeriodicStartOfWeek,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FormatPart {
    Literal(String),
    Year,
    Month,
    Day,
    Week,
    IsoWeek,
    IsoWeekYear,
    Quarter,
}

#[derive(Debug, Clone, Copy, Default)]
struct ParsedFormatValues {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    week: Option<i64>,
    iso_week: Option<i64>,
    iso_week_year: Option<i64>,
    quarter: Option<i64>,
}

#[must_use]
pub fn expected_periodic_note_path(
    config: &PeriodicConfig,
    period_type: &str,
    date: &str,
) -> Option<String> {
    let entry = config.note(period_type)?;
    let start = period_start(period_type, parse_iso_date(date)?, entry)?;
    let file_name = format!("{}.md", render_period_name(start, &entry.format, entry)?);
    let folder = normalize_folder(&entry.folder);
    Some(if folder.is_empty() {
        file_name
    } else {
        format!("{folder}/{file_name}")
    })
}

#[must_use]
pub fn resolve_periodic_note(
    vault_root: &Path,
    config: &PeriodicConfig,
    period_type: &str,
    date: &str,
) -> Option<String> {
    let path = expected_periodic_note_path(config, period_type, date)?;
    vault_root.join(&path).is_file().then_some(path)
}

#[must_use]
pub fn resolve_daily_note(
    vault_root: &Path,
    config: &PeriodicConfig,
    date: &str,
) -> Option<String> {
    resolve_periodic_note(vault_root, config, "daily", date)
}

#[must_use]
pub fn match_periodic_note_path(
    config: &PeriodicConfig,
    relative_path: &str,
) -> Option<PeriodicNoteMatch> {
    let path = relative_path.strip_suffix(".md")?;
    let (folder, file_name) = path
        .rsplit_once('/')
        .map_or(("", path), |(folder, name)| (folder, name));

    for (period_type, note_config) in &config.notes {
        let expected_folder = normalize_folder(&note_config.folder);
        if expected_folder != folder {
            continue;
        }
        let start_date =
            parse_period_name(file_name, &note_config.format, period_type, note_config)?;
        return Some(PeriodicNoteMatch {
            period_type: period_type.clone(),
            start_date,
        });
    }

    None
}

#[must_use]
pub fn period_range_for_date(
    config: &PeriodicConfig,
    period_type: &str,
    date: &str,
) -> Option<(String, String)> {
    let entry = config.note(period_type)?;
    let start = period_start(period_type, parse_iso_date(date)?, entry)?;
    let end = period_end(period_type, start, entry)?;
    Some((start.iso_string(), end.iso_string()))
}

#[must_use]
pub fn step_period_start(
    config: &PeriodicConfig,
    period_type: &str,
    start_date: &str,
) -> Option<String> {
    let entry = config.note(period_type)?;
    let start = parse_iso_date(start_date)?;
    let next = next_period_start(period_type, start, entry)?;
    Some(next.iso_string())
}

#[must_use]
pub fn today_utc_string() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX);
    civil_from_days(seconds.div_euclid(86_400)).iso_string()
}

fn normalize_folder(folder: &Path) -> String {
    folder
        .components()
        .filter_map(|component| match component {
            std::path::Component::CurDir => None,
            other => Some(other.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn period_start(
    period_type: &str,
    date: DateParts,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    let definition = period_definition(period_type, config)?;
    match definition.unit {
        PeriodicCadenceUnit::Days => {
            let anchor_days = days_from_civil(
                definition.anchor.year,
                definition.anchor.month,
                definition.anchor.day,
            );
            let target_days = days_from_civil(date.year, date.month, date.day);
            Some(civil_from_days(
                anchor_days
                    + (target_days - anchor_days).div_euclid(definition.interval)
                        * definition.interval,
            ))
        }
        PeriodicCadenceUnit::Weeks => {
            let anchor_start = week_start(definition.anchor, definition.start_of_week);
            let target_start = week_start(date, definition.start_of_week);
            let diff_weeks =
                (days_from_civil(target_start.year, target_start.month, target_start.day)
                    - days_from_civil(anchor_start.year, anchor_start.month, anchor_start.day))
                .div_euclid(7);
            Some(add_days(
                anchor_start,
                diff_weeks.div_euclid(definition.interval) * definition.interval * 7,
            ))
        }
        PeriodicCadenceUnit::Months => {
            let start_month_index = definition.anchor.month_index()
                + (date.month_index() - definition.anchor.month_index())
                    .div_euclid(definition.interval)
                    * definition.interval;
            Some(date_from_month_index(start_month_index))
        }
        PeriodicCadenceUnit::Quarters => {
            let start_quarter_index = definition.anchor.quarter_index()
                + (date.quarter_index() - definition.anchor.quarter_index())
                    .div_euclid(definition.interval)
                    * definition.interval;
            Some(date_from_quarter_index(start_quarter_index))
        }
        PeriodicCadenceUnit::Years => Some(DateParts {
            year: definition.anchor.year
                + (date.year - definition.anchor.year).div_euclid(definition.interval)
                    * definition.interval,
            month: 1,
            day: 1,
        }),
    }
}

fn period_end(
    period_type: &str,
    start: DateParts,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    Some(add_days(next_period_start(period_type, start, config)?, -1))
}

fn next_period_start(
    period_type: &str,
    start: DateParts,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    let definition = period_definition(period_type, config)?;
    match definition.unit {
        PeriodicCadenceUnit::Days => Some(add_days(start, definition.interval)),
        PeriodicCadenceUnit::Weeks => Some(add_days(start, definition.interval * 7)),
        PeriodicCadenceUnit::Months => Some(date_from_month_index(
            start.month_index() + definition.interval,
        )),
        PeriodicCadenceUnit::Quarters => Some(date_from_quarter_index(
            start.quarter_index() + definition.interval,
        )),
        PeriodicCadenceUnit::Years => Some(DateParts {
            year: start.year + definition.interval,
            month: 1,
            day: 1,
        }),
    }
}

fn period_definition(period_type: &str, config: &PeriodicNoteConfig) -> Option<PeriodDefinition> {
    let unit = config.unit.or_else(|| built_in_unit(period_type))?;
    let interval = i64::try_from(config.interval.max(1)).ok()?;
    let anchor = period_anchor(config, unit);
    Some(PeriodDefinition {
        unit,
        interval,
        anchor,
        start_of_week: config.start_of_week,
    })
}

fn built_in_unit(period_type: &str) -> Option<PeriodicCadenceUnit> {
    match period_type {
        "daily" => Some(PeriodicCadenceUnit::Days),
        "weekly" => Some(PeriodicCadenceUnit::Weeks),
        "monthly" => Some(PeriodicCadenceUnit::Months),
        "quarterly" => Some(PeriodicCadenceUnit::Quarters),
        "yearly" => Some(PeriodicCadenceUnit::Years),
        _ => None,
    }
}

fn period_anchor(config: &PeriodicNoteConfig, unit: PeriodicCadenceUnit) -> DateParts {
    let raw_anchor = config
        .anchor_date
        .as_deref()
        .and_then(parse_iso_date)
        .unwrap_or(DateParts {
            year: 1970,
            month: 1,
            day: 1,
        });
    match unit {
        PeriodicCadenceUnit::Days => raw_anchor,
        PeriodicCadenceUnit::Weeks => week_start(raw_anchor, config.start_of_week),
        PeriodicCadenceUnit::Months => DateParts {
            year: raw_anchor.year,
            month: raw_anchor.month,
            day: 1,
        },
        PeriodicCadenceUnit::Quarters => DateParts {
            year: raw_anchor.year,
            month: (quarter_for_month(raw_anchor.month) - 1) * 3 + 1,
            day: 1,
        },
        PeriodicCadenceUnit::Years => DateParts {
            year: raw_anchor.year,
            month: 1,
            day: 1,
        },
    }
}

fn render_period_name(
    start: DateParts,
    format: &str,
    config: &PeriodicNoteConfig,
) -> Option<String> {
    let parts = tokenize_format(format)?;
    let (iso_week_year, iso_week) = iso_week_components(start);
    let custom_week = custom_week_number(start, config.start_of_week);
    let quarter = quarter_for_month(start.month);
    let mut rendered = String::new();

    for part in parts {
        match part {
            FormatPart::Literal(text) => rendered.push_str(&text),
            FormatPart::Year => {
                let _ = write!(rendered, "{:04}", start.year);
            }
            FormatPart::Month => {
                let _ = write!(rendered, "{:02}", start.month);
            }
            FormatPart::Day => {
                let _ = write!(rendered, "{:02}", start.day);
            }
            FormatPart::Week => {
                let _ = write!(rendered, "{custom_week:02}");
            }
            FormatPart::IsoWeek => {
                let _ = write!(rendered, "{iso_week:02}");
            }
            FormatPart::IsoWeekYear => {
                let _ = write!(rendered, "{iso_week_year:04}");
            }
            FormatPart::Quarter => rendered.push_str(&quarter.to_string()),
        }
    }

    Some(rendered)
}

fn parse_period_name(
    file_name: &str,
    format: &str,
    period_type: &str,
    config: &PeriodicNoteConfig,
) -> Option<String> {
    let parts = tokenize_format(format)?;
    let mut remaining = file_name;
    let mut values = ParsedFormatValues::default();

    for part in parts {
        match part {
            FormatPart::Literal(text) => {
                remaining = remaining.strip_prefix(&text)?;
            }
            FormatPart::Year => {
                let (value, rest) = parse_fixed_digits(remaining, 4)?;
                values.year = Some(value);
                remaining = rest;
            }
            FormatPart::Month => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.month = Some(value);
                remaining = rest;
            }
            FormatPart::Day => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.day = Some(value);
                remaining = rest;
            }
            FormatPart::Week => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.week = Some(value);
                remaining = rest;
            }
            FormatPart::IsoWeek => {
                let (value, rest) = parse_fixed_digits(remaining, 2)?;
                values.iso_week = Some(value);
                remaining = rest;
            }
            FormatPart::IsoWeekYear => {
                let (value, rest) = parse_fixed_digits(remaining, 4)?;
                values.iso_week_year = Some(value);
                remaining = rest;
            }
            FormatPart::Quarter => {
                let (value, rest) = parse_fixed_digits(remaining, 1)?;
                values.quarter = Some(value);
                remaining = rest;
            }
        }
    }

    if !remaining.is_empty() {
        return None;
    }

    let candidate = date_from_format_values(period_type, values, config)?;
    period_start(period_type, candidate, config).map(DateParts::iso_string)
}

fn date_from_format_values(
    period_type: &str,
    values: ParsedFormatValues,
    config: &PeriodicNoteConfig,
) -> Option<DateParts> {
    let unit = period_definition(period_type, config)?.unit;
    match unit {
        PeriodicCadenceUnit::Days => Some(DateParts {
            year: values.year?,
            month: values.month?,
            day: values.day?,
        }),
        PeriodicCadenceUnit::Weeks => {
            if let (Some(year), Some(month), Some(day)) = (values.year, values.month, values.day) {
                return Some(DateParts { year, month, day });
            }
            if let (Some(week_year), Some(week)) = (values.iso_week_year, values.iso_week) {
                return iso_week_start(week_year, week);
            }
            let year = values.year?;
            let week = values.week?;
            custom_week_start(year, week, config.start_of_week)
        }
        PeriodicCadenceUnit::Months => Some(DateParts {
            year: values.year?,
            month: values.month?,
            day: values.day.unwrap_or(1),
        }),
        PeriodicCadenceUnit::Quarters => {
            if let (Some(year), Some(month)) = (values.year, values.month) {
                return Some(DateParts {
                    year,
                    month,
                    day: values.day.unwrap_or(1),
                });
            }
            Some(DateParts {
                year: values.year?,
                month: (values.quarter? - 1) * 3 + 1,
                day: 1,
            })
        }
        PeriodicCadenceUnit::Years => Some(DateParts {
            year: values.year?,
            month: values.month.unwrap_or(1),
            day: values.day.unwrap_or(1),
        }),
    }
}

fn tokenize_format(format: &str) -> Option<Vec<FormatPart>> {
    let mut parts = Vec::new();
    let mut index = 0_usize;
    while index < format.len() {
        let remainder = &format[index..];
        if let Some(rest) = remainder.strip_prefix('[') {
            let end = rest.find(']')?;
            parts.push(FormatPart::Literal(rest[..end].to_string()));
            index += end + 2;
            continue;
        }
        if remainder.starts_with("GGGG") {
            parts.push(FormatPart::IsoWeekYear);
            index += 4;
            continue;
        }
        if remainder.starts_with("YYYY") {
            parts.push(FormatPart::Year);
            index += 4;
            continue;
        }
        if remainder.starts_with("WW") {
            parts.push(FormatPart::IsoWeek);
            index += 2;
            continue;
        }
        if remainder.starts_with("ww") {
            parts.push(FormatPart::Week);
            index += 2;
            continue;
        }
        if remainder.starts_with("MM") {
            parts.push(FormatPart::Month);
            index += 2;
            continue;
        }
        if remainder.starts_with("DD") {
            parts.push(FormatPart::Day);
            index += 2;
            continue;
        }
        if remainder.starts_with('Q') {
            parts.push(FormatPart::Quarter);
            index += 1;
            continue;
        }

        let ch = remainder.chars().next()?;
        match parts.last_mut() {
            Some(FormatPart::Literal(text)) => text.push(ch),
            _ => parts.push(FormatPart::Literal(ch.to_string())),
        }
        index += ch.len_utf8();
    }

    Some(parts)
}

fn parse_fixed_digits(input: &str, width: usize) -> Option<(i64, &str)> {
    let prefix = input.get(..width)?;
    if !prefix.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((prefix.parse().ok()?, &input[width..]))
}

fn parse_iso_date(date: &str) -> Option<DateParts> {
    let mut parts = date.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    (parts.next().is_none() && valid_date(year, month, day)).then_some(DateParts {
        year,
        month,
        day,
    })
}

fn valid_date(year: i64, month: i64, day: i64) -> bool {
    if !(1..=12).contains(&month) {
        return false;
    }
    (1..=days_in_month(year, month)).contains(&day)
}

fn quarter_for_month(month: i64) -> i64 {
    ((month - 1) / 3) + 1
}

fn add_days(date: DateParts, delta: i64) -> DateParts {
    civil_from_days(days_from_civil(date.year, date.month, date.day) + delta)
}

fn date_from_month_index(index: i64) -> DateParts {
    let year = index.div_euclid(12);
    let month = index.rem_euclid(12) + 1;
    DateParts {
        year,
        month,
        day: 1,
    }
}

fn date_from_quarter_index(index: i64) -> DateParts {
    let year = index.div_euclid(4);
    let quarter = index.rem_euclid(4) + 1;
    DateParts {
        year,
        month: (quarter - 1) * 3 + 1,
        day: 1,
    }
}

fn week_start(date: DateParts, start_of_week: PeriodicStartOfWeek) -> DateParts {
    let days = days_from_civil(date.year, date.month, date.day);
    civil_from_days(days - days_since_week_start(days, start_of_week))
}

fn custom_week_start(
    year: i64,
    week: i64,
    start_of_week: PeriodicStartOfWeek,
) -> Option<DateParts> {
    if week < 1 {
        return None;
    }
    let jan1 = DateParts {
        year,
        month: 1,
        day: 1,
    };
    Some(add_days(week_start(jan1, start_of_week), (week - 1) * 7))
}

fn custom_week_number(date: DateParts, start_of_week: PeriodicStartOfWeek) -> i64 {
    let year_start = week_start(
        DateParts {
            year: date.year,
            month: 1,
            day: 1,
        },
        start_of_week,
    );
    (days_from_civil(date.year, date.month, date.day)
        - days_from_civil(year_start.year, year_start.month, year_start.day))
    .div_euclid(7)
        + 1
}

fn iso_week_start(year: i64, week: i64) -> Option<DateParts> {
    if !(1..=53).contains(&week) {
        return None;
    }
    let jan4 = DateParts {
        year,
        month: 1,
        day: 4,
    };
    let jan4_days = days_from_civil(jan4.year, jan4.month, jan4.day);
    let monday = jan4_days - (iso_weekday(jan4) - 1);
    Some(civil_from_days(monday + (week - 1) * 7))
}

fn iso_week_components(date: DateParts) -> (i64, i64) {
    let weekday = iso_weekday(date);
    let ordinal =
        days_from_civil(date.year, date.month, date.day) - days_from_civil(date.year, 1, 1) + 1;
    let mut week = (ordinal - weekday + 10).div_euclid(7);
    let mut week_year = date.year;

    if week < 1 {
        week_year -= 1;
        week = iso_weeks_in_year(week_year);
    } else if week > iso_weeks_in_year(week_year) {
        week_year += 1;
        week = 1;
    }

    (week_year, week)
}

fn iso_weekday(date: DateParts) -> i64 {
    (days_from_civil(date.year, date.month, date.day) + 3).rem_euclid(7) + 1
}

fn iso_weeks_in_year(year: i64) -> i64 {
    let jan1 = iso_weekday(DateParts {
        year,
        month: 1,
        day: 1,
    });
    let dec31 = iso_weekday(DateParts {
        year,
        month: 12,
        day: 31,
    });
    if jan1 == 4 || dec31 == 4 {
        53
    } else {
        52
    }
}

fn days_since_week_start(days_since_epoch: i64, start_of_week: PeriodicStartOfWeek) -> i64 {
    let weekday = (days_since_epoch + 3).rem_euclid(7);
    let week_start = match start_of_week {
        PeriodicStartOfWeek::Monday => 0,
        PeriodicStartOfWeek::Sunday => 6,
        PeriodicStartOfWeek::Saturday => 5,
    };
    (weekday - week_start).rem_euclid(7)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
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

fn civil_from_days(days: i64) -> DateParts {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };

    DateParts {
        year: if month <= 2 { year + 1 } else { year },
        month,
        day,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PeriodicCadenceUnit, PeriodicConfig};
    use std::path::PathBuf;

    #[test]
    fn daily_paths_render_and_resolve() {
        let config = PeriodicConfig::default();

        assert_eq!(
            expected_periodic_note_path(&config, "daily", "2026-04-03"),
            Some("Journal/Daily/2026-04-03.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Daily/2026-04-03.md"),
            Some(PeriodicNoteMatch {
                period_type: "daily".to_string(),
                start_date: "2026-04-03".to_string(),
            })
        );
    }

    #[test]
    fn weekly_paths_respect_configured_week_start() {
        let mut config = PeriodicConfig::default();
        config.note_mut("weekly").start_of_week = PeriodicStartOfWeek::Sunday;

        assert_eq!(
            period_range_for_date(&config, "weekly", "2026-04-03"),
            Some(("2026-03-29".to_string(), "2026-04-04".to_string()))
        );
        assert_eq!(
            expected_periodic_note_path(&config, "weekly", "2026-04-03"),
            Some("Journal/Weekly/2026-W14.md".to_string())
        );
    }

    #[test]
    fn quarterly_resolution_round_trips() {
        let config = PeriodicConfig::default();

        assert_eq!(
            expected_periodic_note_path(&config, "quarterly", "2026-11-02"),
            Some("Journal/Quarterly/2026-Q4.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Quarterly/2026-Q4.md"),
            Some(PeriodicNoteMatch {
                period_type: "quarterly".to_string(),
                start_date: "2026-10-01".to_string(),
            })
        );
    }

    #[test]
    fn custom_folder_and_iso_week_tokens_round_trip() {
        let mut config = PeriodicConfig::default();
        let weekly = config.note_mut("weekly");
        weekly.folder = PathBuf::from("Weekly");
        weekly.format = "GGGG-[W]WW".to_string();

        assert_eq!(
            expected_periodic_note_path(&config, "weekly", "2027-01-01"),
            Some("Weekly/2026-W53.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Weekly/2026-W53.md"),
            Some(PeriodicNoteMatch {
                period_type: "weekly".to_string(),
                start_date: "2026-12-28".to_string(),
            })
        );
    }

    #[test]
    fn step_period_advances_across_month_and_year_boundaries() {
        let config = PeriodicConfig::default();

        assert_eq!(
            step_period_start(&config, "monthly", "2026-12-01"),
            Some("2027-01-01".to_string())
        );
        assert_eq!(
            step_period_start(&config, "yearly", "2026-01-01"),
            Some("2027-01-01".to_string())
        );
    }

    #[test]
    fn custom_biweekly_periods_align_to_anchor_dates() {
        let mut config = PeriodicConfig::default();
        let sprint = config.note_mut("sprint");
        sprint.enabled = true;
        sprint.folder = PathBuf::from("Journal/Sprints");
        sprint.format = "YYYY-[Sprint]-MM-DD".to_string();
        sprint.unit = Some(PeriodicCadenceUnit::Weeks);
        sprint.interval = 2;
        sprint.anchor_date = Some("2026-01-05".to_string());

        assert_eq!(
            period_range_for_date(&config, "sprint", "2026-01-15"),
            Some(("2026-01-05".to_string(), "2026-01-18".to_string()))
        );
        assert_eq!(
            expected_periodic_note_path(&config, "sprint", "2026-01-15"),
            Some("Journal/Sprints/2026-Sprint-01-05.md".to_string())
        );
        assert_eq!(
            step_period_start(&config, "sprint", "2026-01-05"),
            Some("2026-01-19".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Sprints/2026-Sprint-01-19.md"),
            Some(PeriodicNoteMatch {
                period_type: "sprint".to_string(),
                start_date: "2026-01-19".to_string(),
            })
        );
    }

    #[test]
    fn custom_multi_month_periods_round_trip_from_month_format() {
        let mut config = PeriodicConfig::default();
        let release = config.note_mut("release");
        release.enabled = true;
        release.folder = PathBuf::from("Journal/Releases");
        release.format = "YYYY-MM".to_string();
        release.unit = Some(PeriodicCadenceUnit::Months);
        release.interval = 2;
        release.anchor_date = Some("2026-02-01".to_string());

        assert_eq!(
            period_range_for_date(&config, "release", "2026-05-15"),
            Some(("2026-04-01".to_string(), "2026-05-31".to_string()))
        );
        assert_eq!(
            expected_periodic_note_path(&config, "release", "2026-05-15"),
            Some("Journal/Releases/2026-04.md".to_string())
        );
        assert_eq!(
            match_periodic_note_path(&config, "Journal/Releases/2026-04.md"),
            Some(PeriodicNoteMatch {
                period_type: "release".to_string(),
                start_date: "2026-04-01".to_string(),
            })
        );
    }
}
