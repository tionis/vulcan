//! Reminder delivery for chat adapters (`docs/specs/chat-bot.md`).
//!
//! [`plan_chat_reminders`] lists a vault's open reminders as one user sees
//! them, rendered as message text. [`reminder_steps`] compares that plan with
//! the adapter's ledger and says what to do: send, skip as expired, or
//! update a message whose task changed elsewhere. The adapter performs the
//! steps and records them in the ledger
//! ([`super::state::ChatStateStore::put_reminder`]).

use super::state::{ReminderLedgerEntry, ReminderState};
use super::{ensure_fresh, finish_write, mcp_error, with_write, ChatVault};
use crate::tasks::{
    apply_task_snooze_with_guard, build_task_reminders_report_with_guard, ReminderSettings,
    TaskReminderItem, TaskSnoozeRequest,
};
use crate::AppError;
use std::collections::BTreeMap;
use vulcan_core::config::{BotReminderPrivacy, BotRemindersConfig};
use vulcan_core::expression::functions::{parse_date_like_string, parse_duration_string};
use vulcan_core::{load_vault_config, PermissionGuard};

/// The action code of the Done button.
pub const DONE_ACTION: &str = "done";
/// The snooze option that moves a reminder to the next day.
pub const TOMORROW_ACTION: &str = "tomorrow";
/// Ask when to remind again; the adapter prompts and snoozes to the reply.
pub const LATER_ACTION: &str = "later";
const MAX_SNOOZE_OPTIONS: usize = 3;

/// How far ahead the plan looks. Reminders further out are not needed for
/// delivery, only to tell a rescheduled task from a removed one.
const PLAN_HORIZON: &str = "400d";
/// A reminder sent more than this after its time is marked late.
pub const LATE_AFTER_MS: i64 = 5 * 60 * 1_000;
/// A send that failed, or was interrupted by a crash, is retried after this.
pub const RESEND_AFTER_MS: i64 = 60 * 1_000;

/// One open reminder occurrence, ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedReminder {
    pub occurrence_id: String,
    pub task_key: String,
    /// The task reference task commands accept (`path` or `path:line`).
    pub task: String,
    pub path: String,
    pub notify_ms: i64,
    /// The notify time in the vault's reminder timezone.
    pub when: String,
    /// The message without the time line.
    heading: String,
    /// Button actions to offer: `done` and the snooze options, or none when
    /// the user may not write the task's note.
    pub actions: Vec<String>,
}

impl PlannedReminder {
    /// The message text, marked when it is sent late.
    #[must_use]
    pub fn message(&self, late: bool) -> String {
        let late = if late { " (late)" } else { "" };
        format!("{}\n{}{late}", self.heading, self.when)
    }
}

/// A vault's open reminders as one user sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderPlan {
    pub reminders: Vec<PlannedReminder>,
    /// Missed reminders older than this are not sent.
    pub catch_up_ms: i64,
}

/// Plan the open reminders of `vault` visible to its guard.
pub fn plan_chat_reminders(vault: &ChatVault<'_>) -> Result<ReminderPlan, AppError> {
    ensure_fresh(vault)?;
    let config = load_vault_config(vault.paths).config;
    let settings = ReminderSettings::from_config(&config.tasks.reminders)?;
    let catch_up_ms = parse_duration_string(config.bot.reminders.catch_up_window.trim())
        .filter(|window| *window >= 0)
        .ok_or_else(|| {
            AppError::operation(format!(
                "bot.reminders.catch_up_window is not a duration: {}",
                config.bot.reminders.catch_up_window
            ))
        })?;
    let privacy = PrivacyRules::new(&config.bot.reminders)?;
    let snooze_options = snooze_options(&config.bot.reminders)?;
    let report =
        build_task_reminders_report_with_guard(vault.paths, PLAN_HORIZON, Some(vault.guard))?;
    let reminders = report
        .reminders
        .into_iter()
        .filter_map(|item| {
            let notify_ms = parse_date_like_string(&item.notify_at)?;
            // Already due when the task was made, e.g. a default reminder on
            // a task scheduled for today: not a reminder anyone missed.
            if item.created_ms.is_some_and(|created| notify_ms < created) {
                return None;
            }
            let actions = if vault.guard.check_write_path(&item.path).is_ok() {
                std::iter::once(DONE_ACTION.to_string())
                    .chain(snooze_options.iter().cloned())
                    .chain(std::iter::once(LATER_ACTION.to_string()))
                    .collect()
            } else {
                Vec::new()
            };
            let heading = match privacy.for_path(&item.path) {
                BotReminderPrivacy::Full => format!("⏰ {}\n{}", item.title, item.path),
                BotReminderPrivacy::TitleOnly => format!("⏰ {}", item.title),
            };
            Some(PlannedReminder {
                occurrence_id: item.occurrence_id,
                task_key: item.task_key,
                task: item.task,
                path: item.path,
                notify_ms,
                when: message_time(settings.timezone.local_datetime(notify_ms), vault.today),
                heading,
                actions,
            })
        })
        .collect();
    Ok(ReminderPlan {
        reminders,
        catch_up_ms,
    })
}

/// The configured snooze options, validated.
fn snooze_options(config: &BotRemindersConfig) -> Result<Vec<String>, AppError> {
    if config.snooze_options.len() > MAX_SNOOZE_OPTIONS {
        return Err(AppError::operation(format!(
            "bot.reminders.snooze_options allows at most {MAX_SNOOZE_OPTIONS} options"
        )));
    }
    config
        .snooze_options
        .iter()
        .map(|option| {
            let option = option.trim();
            if option == TOMORROW_ACTION
                || parse_duration_string(option).is_some_and(|duration| duration > 0)
                    && !option.contains(',')
            {
                Ok(option.to_string())
            } else {
                Err(AppError::operation(format!(
                    "bot.reminders.snooze_options entry `{option}` is neither a duration nor `tomorrow`"
                )))
            }
        })
        .collect()
}

/// The button label of a reminder action.
#[must_use]
pub fn reminder_action_label(action: &str) -> String {
    match action {
        DONE_ACTION => "Done".to_string(),
        TOMORROW_ACTION => "Tomorrow".to_string(),
        LATER_ACTION => "Later…".to_string(),
        other => format!("Snooze {other}"),
    }
}

/// Run a reminder button's `action` (`done` or a snooze option) on the
/// task of occurrence `occurrence_id` and return the outcome to show. When
/// the occurrence is no longer open, nothing is written and the outcome
/// says so.
pub fn run_reminder_action(
    vault: &ChatVault<'_>,
    occurrence_id: &str,
    action: &str,
    now_ms: i64,
) -> Result<String, AppError> {
    ensure_fresh(vault)?;
    let config = load_vault_config(vault.paths).config;
    let settings = ReminderSettings::from_config(&config.tasks.reminders)?;
    let report =
        build_task_reminders_report_with_guard(vault.paths, PLAN_HORIZON, Some(vault.guard))?;
    let Some(item) = report
        .reminders
        .into_iter()
        .find(|item| item.occurrence_id == occurrence_id)
    else {
        return Ok("This task already changed; nothing was done.".to_string());
    };
    if action == DONE_ACTION {
        return complete(vault, &item);
    }
    if action == LATER_ACTION {
        return Err(AppError::operation(
            "the Later button is handled by the chat adapter",
        ));
    }
    let until = if action == TOMORROW_ACTION {
        settings
            .timezone
            .date_at(now_ms)
            .succ_opt()
            .unwrap_or_default()
            .and_time(settings.default_time)
    } else {
        let duration = parse_duration_string(action)
            .filter(|duration| *duration > 0)
            .ok_or_else(|| AppError::operation(format!("unknown reminder action `{action}`")))?;
        // Rounded up to the next whole minute, the precision of `⏰`.
        let at = now_ms.saturating_add(duration);
        let at = at + (60_000 - at.rem_euclid(60_000)) % 60_000;
        settings.timezone.local_datetime(at)
    };
    snooze(vault, &item, until)
}

/// Snooze the task of occurrence `occurrence_id` to the time in `text`, a
/// reply to the reminder. `Ok(None)` when `text` is not a time.
pub fn run_reminder_snooze_text(
    vault: &ChatVault<'_>,
    occurrence_id: &str,
    text: &str,
    now_ms: i64,
) -> Result<Option<String>, AppError> {
    ensure_fresh(vault)?;
    let config = load_vault_config(vault.paths).config;
    let settings = ReminderSettings::from_config(&config.tasks.reminders)?;
    let Some(when) =
        super::when::parse_chat_when(text, &config.tasknotes, settings.timezone, now_ms)
    else {
        return Ok(None);
    };
    let report =
        build_task_reminders_report_with_guard(vault.paths, PLAN_HORIZON, Some(vault.guard))?;
    let Some(item) = report
        .reminders
        .into_iter()
        .find(|item| item.occurrence_id == occurrence_id)
    else {
        return Ok(Some(
            "This task already changed; nothing was done.".to_string(),
        ));
    };
    snooze(vault, &item, when.at_or(settings.default_time)).map(Some)
}

fn snooze(
    vault: &ChatVault<'_>,
    item: &TaskReminderItem,
    until: chrono::NaiveDateTime,
) -> Result<String, AppError> {
    let report = with_write(vault, || {
        apply_task_snooze_with_guard(
            vault.paths,
            &TaskSnoozeRequest {
                task: item.task.clone(),
                until,
                dry_run: false,
            },
            Some(vault.guard),
        )
    })?;
    finish_write(vault, "task snooze", &report.changed_paths)?;
    Ok(format!(
        "Snoozed until {}.",
        message_time(until, vault.today)
    ))
}

/// A time in a message that stays in the chat: `Sun 8 Nov 12:00`, never
/// relative, since "today" is wrong by tomorrow.
fn message_time(at: chrono::NaiveDateTime, today: chrono::NaiveDate) -> String {
    format!(
        "{} {}",
        super::calendar_date(at.date(), today),
        at.format("%H:%M")
    )
}

fn complete(vault: &ChatVault<'_>, item: &TaskReminderItem) -> Result<String, AppError> {
    with_write(vault, || {
        crate::mcp_tasks::task_complete(
            vault.paths,
            vault.guard,
            vault.profile,
            crate::mcp_protocol::McpTaskCompleteArgs {
                task: item.task.clone(),
                // A recurring task completes the instance the reminder
                // belongs to.
                date: Some(
                    item.occurrence
                        .clone()
                        .unwrap_or_else(|| vault.today.format("%Y-%m-%d").to_string()),
                ),
                dry_run: false,
                no_commit: false,
            },
        )
        .map_err(mcp_error)
    })?;
    Ok("Done.".to_string())
}

struct PrivacyRules {
    default: BotReminderPrivacy,
    overrides: Vec<(globset::GlobMatcher, BotReminderPrivacy)>,
}

impl PrivacyRules {
    fn new(config: &BotRemindersConfig) -> Result<Self, AppError> {
        let overrides = config
            .privacy_overrides
            .iter()
            .map(|rule| {
                globset::GlobBuilder::new(&rule.glob)
                    .literal_separator(true)
                    .backslash_escape(false)
                    .build()
                    .map(|glob| (glob.compile_matcher(), rule.privacy))
                    .map_err(|error| {
                        AppError::operation(format!(
                            "bot.reminders.privacy_overrides glob `{}` is invalid: {error}",
                            rule.glob
                        ))
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            default: config.privacy,
            overrides,
        })
    }

    fn for_path(&self, path: &str) -> BotReminderPrivacy {
        self.overrides
            .iter()
            .find(|(glob, _)| glob.is_match(path))
            .map_or(self.default, |(_, privacy)| *privacy)
    }
}

/// What an adapter does next for one chat and vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderStep {
    /// Record the occurrence as sending, send it, then record it as open
    /// with the message id.
    Send {
        reminder: PlannedReminder,
        late: bool,
    },
    /// Record the occurrence as expired without sending it.
    Expire { reminder: PlannedReminder },
    /// The task was completed, cancelled, rescheduled, or removed elsewhere:
    /// replace the message with `text` (if one was sent) and record the
    /// entry as resolved.
    Resolve {
        entry: ReminderLedgerEntry,
        text: String,
    },
}

/// Compare `plan` with the ledger of the same chat and vault at `now_ms`.
#[must_use]
pub fn reminder_steps(
    plan: &ReminderPlan,
    ledger: &[ReminderLedgerEntry],
    now_ms: i64,
) -> Vec<ReminderStep> {
    let recorded = ledger
        .iter()
        .map(|entry| (entry.occurrence_id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut steps = Vec::new();
    for reminder in plan.reminders.iter().filter(|r| r.notify_ms <= now_ms) {
        let retry = match recorded.get(reminder.occurrence_id.as_str()) {
            None => true,
            Some(entry) => {
                entry.state == ReminderState::Sending
                    && now_ms - entry.updated_ms >= RESEND_AFTER_MS
            }
        };
        if !retry {
            continue;
        }
        let behind = now_ms - reminder.notify_ms;
        steps.push(if behind > plan.catch_up_ms {
            ReminderStep::Expire {
                reminder: reminder.clone(),
            }
        } else {
            ReminderStep::Send {
                reminder: reminder.clone(),
                late: behind > LATE_AFTER_MS,
            }
        });
    }

    let planned = plan
        .reminders
        .iter()
        .map(|reminder| reminder.occurrence_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for entry in ledger.iter().filter(|entry| {
        matches!(entry.state, ReminderState::Open | ReminderState::Sending)
            && !planned.contains(entry.occurrence_id.as_str())
    }) {
        let next = plan
            .reminders
            .iter()
            .find(|reminder| reminder.task_key == entry.task_key);
        let outcome = next.map_or_else(
            || "No longer open: completed, cancelled, or removed.".to_string(),
            |next| format!("Next reminder: {}.", next.when),
        );
        steps.push(ReminderStep::Resolve {
            entry: entry.clone(),
            text: format!("{}\n\n{outcome}", entry.text),
        });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(id: &str, key: &str, notify_ms: i64) -> PlannedReminder {
        PlannedReminder {
            occurrence_id: id.to_string(),
            task_key: key.to_string(),
            task: "A.md:3".to_string(),
            path: "A.md".to_string(),
            notify_ms,
            when: format!("t{notify_ms}"),
            heading: format!("⏰ {key}"),
            actions: Vec::new(),
        }
    }

    fn entry(id: &str, key: &str, state: ReminderState, updated_ms: i64) -> ReminderLedgerEntry {
        ReminderLedgerEntry {
            chat_id: 7,
            vault: "notes".to_string(),
            occurrence_id: id.to_string(),
            task_key: key.to_string(),
            task: "A.md:3".to_string(),
            path: "A.md".to_string(),
            text: format!("⏰ {key}"),
            notify_ms: 0,
            state,
            message_id: (state == ReminderState::Open).then_some(42),
            updated_ms,
            actions: Vec::new(),
        }
    }

    const HOUR: i64 = 60 * 60 * 1_000;

    #[test]
    fn sends_due_reminders_once_and_marks_late_and_expired_ones() {
        let now = 100 * HOUR;
        let plan = ReminderPlan {
            reminders: vec![
                planned("on-time", "a", now - 1_000),
                planned("late", "b", now - HOUR),
                planned("missed", "c", now - 30 * HOUR),
                planned("future", "d", now + HOUR),
                planned("sent", "e", now - HOUR),
            ],
            catch_up_ms: 24 * HOUR,
        };
        let ledger = [entry("sent", "e", ReminderState::Open, now - HOUR)];
        let steps = reminder_steps(&plan, &ledger, now);
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert!(
            matches!(&steps[0], ReminderStep::Send { reminder, late: false } if reminder.occurrence_id == "on-time")
        );
        assert!(
            matches!(&steps[1], ReminderStep::Send { reminder, late: true } if reminder.occurrence_id == "late")
        );
        assert!(
            matches!(&steps[2], ReminderStep::Expire { reminder } if reminder.occurrence_id == "missed")
        );
        let ReminderStep::Send { reminder, late } = &steps[1] else {
            unreachable!()
        };
        assert_eq!(
            reminder.message(*late),
            format!("⏰ b\nt{} (late)", now - HOUR)
        );
    }

    #[test]
    fn interrupted_sends_are_retried_after_a_pause() {
        let now = 100 * HOUR;
        let plan = ReminderPlan {
            reminders: vec![planned("x", "a", now - 2_000)],
            catch_up_ms: 24 * HOUR,
        };
        let fresh = [entry("x", "a", ReminderState::Sending, now - 1_000)];
        assert!(reminder_steps(&plan, &fresh, now).is_empty());
        let stale = [entry(
            "x",
            "a",
            ReminderState::Sending,
            now - RESEND_AFTER_MS,
        )];
        assert!(matches!(
            reminder_steps(&plan, &stale, now).as_slice(),
            [ReminderStep::Send { .. }]
        ));
    }

    #[test]
    fn resolves_open_entries_whose_task_changed() {
        let now = 100 * HOUR;
        let plan = ReminderPlan {
            reminders: vec![planned("moved-new", "moved", now + HOUR)],
            catch_up_ms: 24 * HOUR,
        };
        let ledger = [
            entry("done", "done", ReminderState::Open, now - HOUR),
            entry("moved-old", "moved", ReminderState::Open, now - HOUR),
            entry("gone", "gone", ReminderState::Expired, now - HOUR),
        ];
        let steps = reminder_steps(&plan, &ledger, now);
        assert_eq!(steps.len(), 2, "{steps:?}");
        let texts = steps
            .iter()
            .map(|step| match step {
                ReminderStep::Resolve { text, .. } => text.clone(),
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts[0],
            "⏰ done\n\nNo longer open: completed, cancelled, or removed."
        );
        assert_eq!(
            texts[1],
            format!("⏰ moved\n\nNext reminder: t{}.", now + HOUR)
        );
    }
}
