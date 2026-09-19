use crate::AppError;
use serde::Serialize;
use std::fs;
use vulcan_core::{
    expected_periodic_note_path, load_vault_config, match_periodic_note_path, VaultPaths,
};

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
