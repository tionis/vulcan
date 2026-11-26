//! A vault's agenda for one day: what is late, what is due, what comes up this
//! week, what happens today, and what got done yesterday. The morning digest
//! renders it; anything else that needs "my day" (an `/agenda` command, a
//! calendar view, an assistant) reads the same structure.
//!
//! Every read goes through the caller's permission profile, so an agenda never
//! shows a task, reminder, event or inbox note the profile cannot read.

use crate::tasks::reminders::ReminderSettings;
use crate::tasks::{build_task_reminders_report_with_guard, build_tasks_query_result_with_guard};
use crate::AppError;
use jiff::civil::Date;
use jiff::Span;
use serde::Serialize;
use serde_json::Value;
use vulcan_core::expression::functions::parse_date_like_string;
use vulcan_core::tasks::inline_task_title;
use vulcan_core::{load_vault_config, PermissionGuard, ProfilePermissionGuard, VaultPaths};

/// Days past due after which an open task counts as slipping.
pub const SLIPPING_AFTER_DAYS: i64 = 7;

/// Days after today that "this week" covers.
pub const WEEK_DAYS: i64 = 7;

/// One vault's agenda for `today`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VaultAgenda {
    /// Today in the vault's reminder timezone (`YYYY-MM-DD`).
    pub today: String,
    /// Open tasks due or scheduled before today, oldest first.
    pub overdue: Vec<AgendaTask>,
    /// Open tasks due or scheduled today.
    pub today_tasks: Vec<AgendaTask>,
    /// Open tasks due or scheduled in the next [`WEEK_DAYS`] days.
    pub this_week: Vec<AgendaTask>,
    /// Reminders still to come today.
    pub reminders: Vec<AgendaReminder>,
    /// Events written in today's daily note.
    pub events: Vec<AgendaEvent>,
    /// Titles of tasks completed yesterday.
    pub done_yesterday: Vec<String>,
    /// Open entries in the inbox note, when the profile can read it.
    pub inbox: Option<usize>,
}

impl VaultAgenda {
    /// Whether there is nothing worth a message.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.overdue.is_empty()
            && self.today_tasks.is_empty()
            && self.this_week.is_empty()
            && self.reminders.is_empty()
            && self.events.is_empty()
            && self.done_yesterday.is_empty()
            && self.inbox.unwrap_or(0) == 0
    }

    /// Overdue tasks that have slipped for [`SLIPPING_AFTER_DAYS`] or more.
    pub fn slipping(&self) -> impl Iterator<Item = &AgendaTask> {
        self.overdue
            .iter()
            .filter(|task| task.days_late >= SLIPPING_AFTER_DAYS)
    }
}

/// An open task on the agenda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgendaTask {
    /// What the task commands take: `path:line` for an inline task, the note
    /// path for a task note.
    pub reference: String,
    pub title: String,
    /// The due date, else the scheduled date, as written (`YYYY-MM-DD`,
    /// optionally with a time).
    pub date: String,
    /// Days past that date: positive when late, zero today, negative ahead.
    pub days_late: i64,
}

/// A reminder still to come today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgendaReminder {
    /// Wall-clock `HH:MM` in the reminder timezone.
    pub time: String,
    pub title: String,
}

/// An event from today's daily note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgendaEvent {
    /// `HH:MM`, or `None` for an all-day event.
    pub time: Option<String>,
    pub title: String,
}

/// The agenda of the vault at `paths` at `now_ms`, as `guard` may see it.
/// The caller refreshes the cache first if it may be stale.
pub fn build_vault_agenda(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    now_ms: i64,
) -> Result<VaultAgenda, AppError> {
    let config = load_vault_config(paths).config;
    let settings = ReminderSettings::from_config(&config.tasks.reminders)?;
    let today = settings.timezone.date_at(now_ms);
    let day = |offset: i64| {
        today
            .checked_add(Span::new().days(offset))
            .map_err(|_| AppError::operation(format!("date out of range: {today}")))
    };
    let after_week = day(WEEK_DAYS + 1)?;

    let mut agenda = VaultAgenda {
        today: today.to_string(),
        ..VaultAgenda::default()
    };
    let open = build_tasks_query_result_with_guard(
        paths,
        &format!(
            "NOT (done OR status.type is cancelled)\n\
             (due before {after_week}) OR (scheduled before {after_week})\n\
             sort by due"
        ),
        guard,
    )?;
    for task in open
        .tasks
        .iter()
        .filter_map(|task| agenda_task(task, today))
    {
        match task.days_late {
            late if late > 0 => agenda.overdue.push(task),
            0 => agenda.today_tasks.push(task),
            _ => agenda.this_week.push(task),
        }
    }
    agenda.overdue.sort_by_key(|task| -task.days_late);

    let yesterday = day(-1)?;
    let done = build_tasks_query_result_with_guard(paths, &format!("done on {yesterday}"), guard)?;
    agenda.done_yesterday = done
        .tasks
        .iter()
        .filter_map(|task| agenda_task(task, today))
        .map(|task| task.title)
        .collect();

    let tomorrow_ms = settings
        .timezone
        .instant_ms(day(1)?.to_datetime(jiff::civil::Time::midnight()))
        .unwrap_or(now_ms);
    let minutes_left = ((tomorrow_ms - now_ms) / 60_000).max(1);
    let reminders =
        build_task_reminders_report_with_guard(paths, &format!("{minutes_left}m"), Some(guard))?;
    agenda.reminders = reminders
        .reminders
        .iter()
        .filter(|reminder| !reminder.overdue)
        .filter_map(|reminder| {
            let at = parse_date_like_string(&reminder.notify_at)?;
            Some(AgendaReminder {
                time: settings
                    .timezone
                    .local_datetime(at)
                    .strftime("%H:%M")
                    .to_string(),
                title: reminder.title.clone(),
            })
        })
        .collect();

    let today_text = today.to_string();
    agenda.events = vulcan_core::list_daily_note_events(paths, &today_text, &today_text)
        .map_err(AppError::operation)?
        .into_iter()
        .filter(|note| guard.check_read_path(&note.path).is_ok())
        .flat_map(|note| note.events)
        .map(|event| AgendaEvent {
            time: (event.start_time != "all-day").then_some(event.start_time),
            title: event.title,
        })
        .collect();

    agenda.inbox = crate::inbox::inbox_path(&config.inbox)
        .ok()
        .filter(|path| guard.check_read_path(path).is_ok())
        .and_then(|path| std::fs::read_to_string(paths.vault_root().join(path)).ok())
        .map(|text| crate::inbox::count_inbox_entries(&text, config.inbox.heading.as_deref()));
    Ok(agenda)
}

fn agenda_task(task: &Value, today: Date) -> Option<AgendaTask> {
    let text = |key: &str| {
        task.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    let path = text("path")?;
    let (reference, title) = if task.get("taskSource").and_then(Value::as_str) == Some("file") {
        let title = text("title")
            .or_else(|| text("text"))
            .unwrap_or(path.clone());
        (path, title)
    } else {
        let line = task.get("line")?.as_i64()?;
        (
            format!("{path}:{line}"),
            inline_task_title(&text("text").unwrap_or_default()),
        )
    };
    let date = text("due")
        .or_else(|| text("scheduled"))
        .unwrap_or_default();
    let days_late = date
        .get(..10)
        .and_then(|day| Date::strptime("%Y-%m-%d", day).ok())
        .and_then(|day| day.until(today).ok())
        .map_or(0, |span| i64::from(span.get_days()));
    Some(AgendaTask {
        reference,
        title,
        date,
        days_late,
    })
}
