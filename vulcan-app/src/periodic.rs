use crate::AppError;
use serde::Serialize;
use serde_json::Value;
use std::fs;
use vulcan_core::config::PeriodicConfig;
use vulcan_core::expression::functions::{date_components, parse_date_like_string};
use vulcan_core::{
    expected_periodic_note_path, list_daily_note_events, load_events_for_periodic_note,
    load_vault_config, match_periodic_note_path, period_range_for_date, resolve_periodic_note,
    VaultPaths,
};

use crate::templates::TemplateTimestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodicTarget {
    pub period_type: String,
    pub reference_date: String,
    pub start_date: String,
    pub end_date: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeriodicEventReport {
    pub start_time: String,
    pub end_time: Option<String>,
    pub title: String,
    pub metadata: Value,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeriodicShowReport {
    pub period_type: String,
    pub reference_date: String,
    pub start_date: String,
    pub path: String,
    pub end_date: String,
    pub content: String,
    pub events: Vec<PeriodicEventReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailyListItem {
    pub period_type: String,
    pub date: String,
    pub path: String,
    pub event_count: usize,
    pub events: Vec<PeriodicEventReport>,
}

#[must_use]
pub fn current_utc_date_string() -> String {
    TemplateTimestamp::current().default_date_string()
}

pub fn normalize_date_argument(date: Option<&str>) -> Result<String, AppError> {
    match date
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        None => Ok(current_utc_date_string()),
        Some(value) if value == "today" => Ok(current_utc_date_string()),
        Some(value) => {
            let timestamp = parse_date_like_string(&value)
                .ok_or_else(|| AppError::operation(format!("invalid date: {value}")))?;
            let (year, month, day, _, _, _, _) = date_components(timestamp);
            Ok(format!("{year:04}-{month:02}-{day:02}"))
        }
    }
}

pub fn resolve_periodic_target(
    config: &PeriodicConfig,
    period_type: &str,
    date: Option<&str>,
    require_enabled: bool,
) -> Result<PeriodicTarget, AppError> {
    let note = config
        .note(period_type)
        .ok_or_else(|| AppError::operation(format!("unknown periodic note type: {period_type}")))?;
    if require_enabled && !note.enabled {
        return Err(AppError::operation(format!(
            "periodic note type `{period_type}` is disabled in config"
        )));
    }
    let reference_date = normalize_date_argument(date)?;
    let (start_date, end_date) = period_range_for_date(config, period_type, &reference_date)
        .ok_or_else(|| {
            AppError::operation(format!(
                "failed to resolve period range for `{period_type}` and {reference_date}"
            ))
        })?;
    let path =
        expected_periodic_note_path(config, period_type, &reference_date).ok_or_else(|| {
            AppError::operation(format!(
                "failed to resolve note path for `{period_type}` and {reference_date}"
            ))
        })?;
    Ok(PeriodicTarget {
        period_type: period_type.to_string(),
        reference_date,
        start_date,
        end_date,
        path,
    })
}

pub fn resolve_daily_list_window(
    config: &PeriodicConfig,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<(String, String), AppError> {
    let today = current_utc_date_string();
    if week {
        return period_range_for_date(config, "weekly", &today)
            .ok_or_else(|| AppError::operation("failed to resolve weekly date range"));
    }
    if month {
        return period_range_for_date(config, "monthly", &today)
            .ok_or_else(|| AppError::operation("failed to resolve monthly date range"));
    }
    let start = normalize_date_argument(from)?;
    let end = match to {
        Some(value) => normalize_date_argument(Some(value))?,
        None if from.is_some() => start.clone(),
        None => today,
    };
    if start > end {
        return Err(AppError::operation(format!(
            "start date must be before or equal to end date: {start} > {end}"
        )));
    }
    Ok((start, end))
}

pub fn list_daily_notes(
    paths: &VaultPaths,
    from: Option<&str>,
    to: Option<&str>,
    week: bool,
    month: bool,
) -> Result<Vec<DailyListItem>, AppError> {
    let config = load_vault_config(paths).config;
    let (start, end) = resolve_daily_list_window(&config.periodic, from, to, week, month)?;
    list_daily_note_events(paths, &start, &end)
        .map(|items| {
            items
                .into_iter()
                .map(|item| {
                    let events = item
                        .events
                        .into_iter()
                        .map(|event| PeriodicEventReport {
                            start_time: event.start_time,
                            end_time: event.end_time,
                            title: event.title,
                            metadata: event.metadata,
                            tags: event.tags,
                        })
                        .collect::<Vec<_>>();
                    DailyListItem {
                        period_type: "daily".to_string(),
                        date: item.date,
                        path: item.path,
                        event_count: events.len(),
                        events,
                    }
                })
                .collect()
        })
        .map_err(AppError::operation)
}

pub fn show_periodic_note(
    paths: &VaultPaths,
    date: Option<&str>,
    period_type: &str,
) -> Result<PeriodicShowReport, AppError> {
    let config = load_vault_config(paths).config;
    let target = resolve_periodic_target(&config.periodic, period_type, date, false)?;
    let resolved = resolve_periodic_note(
        paths.vault_root(),
        &config.periodic,
        period_type,
        &target.reference_date,
    )
    .unwrap_or_else(|| target.path.clone());
    let absolute_path = paths.vault_root().join(&resolved);
    if !absolute_path.is_file() {
        return Err(AppError::operation(format!(
            "{period_type} note does not exist on disk: {}",
            target.path
        )));
    }
    let events = if period_type == "daily" {
        load_events_for_periodic_note(paths, &resolved)
            .map_err(AppError::operation)?
            .into_iter()
            .map(|event| PeriodicEventReport {
                start_time: event.start_time,
                end_time: event.end_time,
                title: event.title,
                metadata: event.metadata,
                tags: event.tags,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(PeriodicShowReport {
        period_type: period_type.to_string(),
        reference_date: target.reference_date,
        start_date: target.start_date,
        end_date: target.end_date,
        path: resolved,
        content: fs::read_to_string(&absolute_path).map_err(AppError::operation)?,
        events,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DailyReadTarget<'a> {
    Latest,
    Date(&'a str),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailyNoteReadReport {
    pub operation: String,
    pub date: Option<String>,
    pub path: Option<String>,
    pub exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn read_daily_note(
    paths: &VaultPaths,
    target: DailyReadTarget<'_>,
    include_content: bool,
) -> Result<DailyNoteReadReport, AppError> {
    let config = load_vault_config(paths).config;
    let (operation, date, path) = match target {
        DailyReadTarget::Latest => {
            let latest = latest_daily_note_where(paths, &config.periodic, |_| true)?;
            (
                "latest",
                latest.as_ref().map(|item| item.0.clone()),
                latest.map(|item| item.1),
            )
        }
        DailyReadTarget::Date(date) => {
            let path = expected_periodic_note_path(&config.periodic, "daily", date)
                .ok_or_else(|| AppError::operation(format!("invalid daily-note date: {date}")))?;
            ("show", Some(date.to_string()), Some(path))
        }
    };

    let exists = path
        .as_deref()
        .is_some_and(|path| paths.vault_root().join(path).is_file());
    let content = if include_content && exists {
        Some(fs::read_to_string(paths.vault_root().join(
            path.as_deref().expect("existing daily note has a path"),
        ))?)
    } else {
        None
    };
    let reason = (!exists).then(|| {
        if matches!(target, DailyReadTarget::Latest) {
            "no_daily_notes".to_string()
        } else {
            "daily_note_not_found".to_string()
        }
    });

    Ok(DailyNoteReadReport {
        operation: operation.to_string(),
        date,
        path,
        exists,
        content,
        reason,
    })
}

pub fn read_latest_daily_note_where(
    paths: &VaultPaths,
    include_content: bool,
    predicate: impl FnMut(&str) -> bool,
) -> Result<DailyNoteReadReport, AppError> {
    let config = load_vault_config(paths).config;
    let latest = latest_daily_note_where(paths, &config.periodic, predicate)?;
    let (date, path) = latest.map_or((None, None), |(date, path)| (Some(date), Some(path)));
    let exists = path.is_some();
    let content = if include_content {
        path.as_deref()
            .map(|path| fs::read_to_string(paths.vault_root().join(path)))
            .transpose()?
    } else {
        None
    };
    Ok(DailyNoteReadReport {
        operation: "latest".to_string(),
        date,
        path,
        exists,
        content,
        reason: (!exists).then(|| "no_daily_notes".to_string()),
    })
}

fn latest_daily_note_where(
    paths: &VaultPaths,
    config: &vulcan_core::config::PeriodicConfig,
    mut predicate: impl FnMut(&str) -> bool,
) -> Result<Option<(String, String)>, AppError> {
    let daily = config
        .note("daily")
        .ok_or_else(|| AppError::operation("daily periodic-note configuration is missing"))?;
    let folder_string = daily.folder.to_string_lossy().replace('\\', "/");
    let folder = folder_string.trim_matches('/');
    let absolute_folder = if folder.is_empty() {
        paths.vault_root().to_path_buf()
    } else {
        paths.vault_root().join(folder)
    };
    if !absolute_folder.is_dir() {
        return Ok(None);
    }

    let mut latest: Option<(String, String)> = None;
    for entry in fs::read_dir(&absolute_folder)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let relative_path = if folder.is_empty() {
            file_name
        } else {
            format!("{folder}/{file_name}")
        };
        let Some(periodic_match) = match_periodic_note_path(config, &relative_path) else {
            continue;
        };
        if periodic_match.period_type != "daily" {
            continue;
        }
        if !predicate(&relative_path) {
            continue;
        }
        let candidate = (periodic_match.start_date, relative_path);
        if latest.as_ref().is_none_or(|current| candidate > *current) {
            latest = Some(candidate);
        }
    }
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::{scan_vault, ScanMode};

    #[test]
    fn daily_list_and_show_share_date_resolution_and_event_reports() {
        let temp = TempDir::new().expect("tempdir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize");
        fs::create_dir_all(temp.path().join("Journal/Daily")).expect("daily folder");
        fs::write(
            paths.config_file(),
            "[periodic.daily]\nschedule_heading = \"Schedule\"\n",
        )
        .expect("config");
        fs::write(
            temp.path().join("Journal/Daily/2026-04-03.md"),
            "# Friday\n\n## Schedule\n- 09:00 Team standup\n",
        )
        .expect("daily note");
        scan_vault(&paths, ScanMode::Full).expect("scan");

        let items =
            list_daily_notes(&paths, Some("2026-04-03"), None, false, false).expect("daily list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].date, "2026-04-03");
        assert_eq!(items[0].event_count, 1);
        assert_eq!(items[0].events[0].title, "Team standup");

        let shown = show_periodic_note(&paths, Some("2026-04-03"), "daily").expect("daily show");
        assert_eq!(shown.path, items[0].path);
        assert_eq!(shown.events, items[0].events);
        assert!(shown.content.contains("# Friday"));
        assert_eq!(shown.start_date, "2026-04-03");
        assert_eq!(shown.end_date, "2026-04-03");
    }

    #[test]
    fn explicit_daily_window_rejects_reversed_dates() {
        let config = PeriodicConfig::default();
        assert_eq!(
            resolve_daily_list_window(
                &config,
                Some("2026-04-03"),
                Some("2026-04-04"),
                false,
                false,
            )
            .expect("valid window"),
            ("2026-04-03".to_string(), "2026-04-04".to_string())
        );
        let error = resolve_daily_list_window(
            &config,
            Some("2026-04-04"),
            Some("2026-04-03"),
            false,
            false,
        )
        .expect_err("reversed window");
        assert!(error.to_string().contains("start date must be before"));
    }

    #[test]
    fn latest_uses_configured_folder_and_newest_existing_date() {
        let temp = TempDir::new().expect("tempdir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize");
        fs::create_dir_all(temp.path().join("Journal/Daily")).expect("daily folder");
        fs::write(
            paths.config_file(),
            "[periodic.notes.daily]\nenabled = true\nfolder = \"Journal/Daily\"\nformat = \"YYYY-MM-DD\"\n",
        )
        .expect("config");
        for date in ["2026-09-01", "2026-09-03", "2026-09-08"] {
            fs::write(
                temp.path().join(format!("Journal/Daily/{date}.md")),
                format!("# {date}\n"),
            )
            .expect("daily note");
        }

        let report = read_daily_note(&paths, DailyReadTarget::Latest, true).expect("latest");
        assert_eq!(report.date.as_deref(), Some("2026-09-08"));
        assert_eq!(report.path.as_deref(), Some("Journal/Daily/2026-09-08.md"));
        assert_eq!(report.content.as_deref(), Some("# 2026-09-08\n"));
        assert!(report.exists);
    }

    #[test]
    fn latest_returns_typed_absence_when_folder_has_no_daily_notes() {
        let temp = TempDir::new().expect("tempdir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize");

        let report = read_daily_note(&paths, DailyReadTarget::Latest, true).expect("latest");
        assert!(!report.exists);
        assert_eq!(report.reason.as_deref(), Some("no_daily_notes"));
        assert!(report.date.is_none());
        assert!(report.path.is_none());
        assert!(report.content.is_none());
    }

    #[test]
    fn latest_filter_is_applied_before_selecting_the_newest_note() {
        let temp = TempDir::new().expect("tempdir");
        let paths = VaultPaths::new(temp.path());
        initialize_vulcan_dir(&paths).expect("initialize");
        fs::create_dir_all(temp.path().join("Journal/Daily")).expect("daily folder");
        for date in ["2026-09-03", "2026-09-08"] {
            fs::write(
                temp.path().join(format!("Journal/Daily/{date}.md")),
                format!("# {date}\n"),
            )
            .expect("daily note");
        }

        let report =
            read_latest_daily_note_where(&paths, false, |path| !path.ends_with("2026-09-08.md"))
                .expect("latest readable");
        assert_eq!(report.date.as_deref(), Some("2026-09-03"));
    }
}
