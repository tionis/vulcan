//! Transport-neutral chat bot commands.
//!
//! A chat adapter (the daemon's Telegram service) turns an incoming message
//! into a [`ChatCommand`] with [`parse_chat_message`], runs it against one
//! vault with [`run_chat_command`], and renders the [`ChatReply`]. Buttons
//! carry a [`ChatAction`] that the adapter keeps on its side and hands to
//! [`run_chat_action`] when pressed. Nothing here knows the chat platform,
//! and every write goes through the same workflows, permission checks,
//! cache refresh, and auto-commit as the CLI.

use crate::commit::AutoCommitPolicy;
use crate::execution::MutationGate;
use crate::mcp_protocol::{McpMethodError, McpNoteAppendArgs, McpTaskCreateArgs};
use crate::notes::{read_note, NoteGetOptions, NoteReadMode};
use crate::scan::{refresh_cache_for_paths, refresh_cache_incrementally};
use crate::tasks::{
    apply_task_add_with_guard, build_task_reminders_report_with_guard,
    build_tasks_query_result_with_guard, TaskAddRequest,
};
use crate::AppError;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use vulcan_core::config::{BotCaptureTarget, BotTaskTarget};
use vulcan_core::search::{search_vault_with_filter, SearchQuery};
use vulcan_core::tasks::inline_task_title;
use vulcan_core::{
    load_vault_config, DataviewJsRenderPolicy, PermissionGuard, ProfilePermissionGuard, VaultPaths,
    Verbosity,
};

/// The most rows a list reply shows.
pub const CHAT_LIST_LIMIT: usize = 15;
/// The most search hits a reply shows.
pub const CHAT_SEARCH_LIMIT: usize = 8;
/// Longest text sent inline; longer notes are sent as a document.
pub const CHAT_INLINE_TEXT_LIMIT: usize = 3500;

/// The bot's commands, for help text and the platform's command menu.
pub const CHAT_COMMANDS: &[(&str, &str)] = &[
    ("task", "Create a task, e.g. /task call Anna tomorrow 10:00"),
    ("daily", "Append a line to today's daily note"),
    ("today", "Open tasks due or scheduled up to today"),
    ("due", "Open tasks due within N days, e.g. /due 7"),
    ("reminders", "Upcoming reminders, e.g. /reminders 2d"),
    ("find", "Search the vault"),
    ("note", "Show a note by path or title"),
    ("vault", "Show or switch the active vault"),
    ("help", "List commands"),
];

/// One parsed chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    /// Plain text: capture it to `bot.default_capture`.
    Capture {
        text: String,
    },
    Daily {
        text: String,
    },
    Task {
        text: String,
    },
    Today,
    Due {
        days: u32,
    },
    Reminders {
        window: String,
    },
    Find {
        query: String,
    },
    Note {
        reference: String,
    },
    /// Handled by the adapter, which owns the chat's active vault.
    Vault {
        name: Option<String>,
    },
    Help,
    /// A known command given without its required argument.
    Usage {
        command: &'static str,
    },
    Unknown {
        command: String,
    },
    Empty,
}

impl ChatCommand {
    /// Whether the command may write to the vault.
    #[must_use]
    pub fn writes(&self) -> bool {
        matches!(
            self,
            Self::Capture { .. } | Self::Daily { .. } | Self::Task { .. }
        )
    }
}

/// Parse a chat message. `/command@BotName args` is accepted as sent in group
/// menus; commands are case-insensitive.
#[must_use]
pub fn parse_chat_message(text: &str) -> ChatCommand {
    let text = text.trim();
    if text.is_empty() {
        return ChatCommand::Empty;
    }
    let Some(rest) = text.strip_prefix('/') else {
        return ChatCommand::Capture {
            text: text.to_string(),
        };
    };
    let (head, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(head, args)| (head, args.trim()));
    let command = head
        .split_once('@')
        .map_or(head, |(command, _)| command)
        .to_ascii_lowercase();
    let required = |name: &'static str, build: fn(String) -> ChatCommand| {
        if args.is_empty() {
            ChatCommand::Usage { command: name }
        } else {
            build(args.to_string())
        }
    };
    match command.as_str() {
        "start" | "help" => ChatCommand::Help,
        "task" | "t" => required("task", |text| ChatCommand::Task { text }),
        "daily" | "d" => required("daily", |text| ChatCommand::Daily { text }),
        "capture" | "c" | "inbox" => required("capture", |text| ChatCommand::Capture { text }),
        "today" => ChatCommand::Today,
        "due" => match args.trim_end_matches('d') {
            "" => ChatCommand::Due { days: 7 },
            days => days
                .parse::<u32>()
                .ok()
                .filter(|days| *days <= 366)
                .map_or(ChatCommand::Usage { command: "due" }, |days| {
                    ChatCommand::Due { days }
                }),
        },
        "reminders" | "r" => ChatCommand::Reminders {
            window: if args.is_empty() {
                "24h".to_string()
            } else {
                args.to_string()
            },
        },
        "find" | "search" | "f" => required("find", |query| ChatCommand::Find { query }),
        "note" | "n" => required("note", |reference| ChatCommand::Note { reference }),
        "vault" | "v" => ChatCommand::Vault {
            name: (!args.is_empty()).then(|| args.to_string()),
        },
        _ => ChatCommand::Unknown { command },
    }
}

/// The help text listing every command.
#[must_use]
pub fn chat_help_text() -> String {
    let mut text = String::from("Send any text to capture it. Commands:\n");
    for (command, description) in CHAT_COMMANDS {
        let _ = writeln!(text, "/{command} - {description}");
    }
    text.trim_end().to_string()
}

/// The usage line of a command given without its argument.
#[must_use]
pub fn chat_usage_text(command: &str) -> String {
    match command {
        "task" => "Usage: /task <what> [when], e.g. /task call Anna tomorrow 10:00".to_string(),
        "daily" => "Usage: /daily <text>".to_string(),
        "capture" => "Usage: /capture <text>".to_string(),
        "due" => "Usage: /due [days], e.g. /due 7".to_string(),
        "find" => "Usage: /find <words>".to_string(),
        "note" => "Usage: /note <path or title>".to_string(),
        other => format!("Usage: /{other}"),
    }
}

/// What a button does when pressed. The adapter stores it and passes it
/// back; it is never decoded from untrusted callback payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatAction {
    /// Complete a task (`path:line` or a `TaskNotes` path), as `tasks complete`.
    CompleteTask { task: String, title: String },
    /// Send a note.
    ShowNote { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatButton {
    pub label: String,
    pub action: ChatAction,
}

/// A file to send instead of, or beside, the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDocument {
    pub file_name: String,
    pub content: String,
}

/// A reply: plain text, one button per row, and an optional document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatReply {
    pub text: String,
    pub buttons: Vec<ChatButton>,
    pub document: Option<ChatDocument>,
    /// Vault-relative paths this command wrote.
    pub changed_paths: Vec<String>,
}

impl ChatReply {
    fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// The vault a command runs against and the authority it runs with.
pub struct ChatVault<'a> {
    pub paths: &'a VaultPaths,
    pub guard: &'a ProfilePermissionGuard,
    /// The permission profile name recorded in auto-commits.
    pub profile: &'a str,
    /// Held around every write: the daemon's per-vault mutation permit.
    pub gate: &'a dyn MutationGate,
    /// Today in the vault's reminder timezone.
    pub today: NaiveDate,
}

/// Run a vault command. [`ChatCommand::Vault`], [`ChatCommand::Help`],
/// usage, and unknown commands are the adapter's.
pub fn run_chat_command(
    vault: &ChatVault<'_>,
    command: &ChatCommand,
) -> Result<ChatReply, AppError> {
    match command {
        ChatCommand::Capture { text } => capture(vault, text),
        ChatCommand::Daily { text } => append_daily(vault, text),
        ChatCommand::Task { text } => create_task(vault, text),
        ChatCommand::Today => due_tasks(vault, 0),
        ChatCommand::Due { days } => due_tasks(vault, *days),
        ChatCommand::Reminders { window } => reminders(vault, window),
        ChatCommand::Find { query } => find(vault, query),
        ChatCommand::Note { reference } => show_note(vault, reference),
        ChatCommand::Help => Ok(ChatReply::text(chat_help_text())),
        ChatCommand::Usage { command } => Ok(ChatReply::text(chat_usage_text(command))),
        ChatCommand::Vault { .. } | ChatCommand::Unknown { .. } | ChatCommand::Empty => Err(
            AppError::operation("this command is handled by the chat adapter"),
        ),
    }
}

/// Run a pressed button's action.
pub fn run_chat_action(vault: &ChatVault<'_>, action: &ChatAction) -> Result<ChatReply, AppError> {
    match action {
        ChatAction::CompleteTask { task, title } => {
            let report = with_write(vault, || {
                crate::mcp_tasks::task_complete(
                    vault.paths,
                    vault.guard,
                    vault.profile,
                    crate::mcp_protocol::McpTaskCompleteArgs {
                        task: task.clone(),
                        date: Some(vault.today.format("%Y-%m-%d").to_string()),
                        dry_run: false,
                        no_commit: false,
                    },
                )
                .map_err(mcp_error)
            })?;
            let mut reply = ChatReply::text(format!("Done: {title}"));
            reply.changed_paths = report.changed_paths;
            Ok(reply)
        }
        ChatAction::ShowNote { path } => show_note(vault, path),
    }
}

/// Hold the vault's mutation permit around a write, after finishing any
/// interrupted mdbase write, as the CLI does before each command.
fn with_write<T>(
    vault: &ChatVault<'_>,
    write: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    let _permit = vault.gate.enter()?;
    let _ = crate::mdbase::recover_interrupted_mdbase_write_with_guard(vault.paths, vault.guard);
    write()
}

fn finish_write(vault: &ChatVault<'_>, action: &str, changed: &[String]) -> Result<(), AppError> {
    if changed.is_empty() {
        return Ok(());
    }
    refresh_cache_for_paths(vault.paths, changed)?;
    AutoCommitPolicy::for_mutation(vault.paths, false)
        .commit(
            vault.paths,
            action,
            changed,
            Some(vault.profile),
            Verbosity::Quiet,
        )
        .map(|_| ())
        .map_err(AppError::operation)
}

/// Reads see the vault as it is on disk: a cheap freshness proof, else an
/// incremental scan.
fn ensure_fresh(vault: &ChatVault<'_>) -> Result<(), AppError> {
    if matches!(
        vulcan_core::prove_note_store_fresh(vault.paths),
        Ok(vulcan_core::NoteStoreFreshness::Fresh)
    ) {
        return Ok(());
    }
    refresh_cache_incrementally(vault.paths).map(|_| ())
}

fn capture(vault: &ChatVault<'_>, text: &str) -> Result<ChatReply, AppError> {
    match load_vault_config(vault.paths).config.bot.default_capture {
        BotCaptureTarget::Inbox => capture_inbox(vault, text),
        BotCaptureTarget::Daily => append_daily(vault, text),
    }
}

fn capture_inbox(vault: &ChatVault<'_>, text: &str) -> Result<ChatReply, AppError> {
    let path = with_write(vault, || {
        let inbox = crate::inbox::inbox_path(&load_vault_config(vault.paths).config.inbox)?;
        vault
            .guard
            .check_write_path(&inbox)
            .map_err(AppError::operation)?;
        let path = crate::inbox::write_inbox_capture(
            vault.paths,
            text,
            Some(vault.profile),
            Verbosity::Quiet,
        )?;
        finish_write(vault, "bot-capture", std::slice::from_ref(&path))?;
        Ok(path)
    })?;
    Ok(ChatReply {
        text: format!("Captured to {path}"),
        changed_paths: vec![path],
        ..ChatReply::default()
    })
}

fn append_daily(vault: &ChatVault<'_>, text: &str) -> Result<ChatReply, AppError> {
    let report = with_write(vault, || {
        crate::mcp_notes::note_append(
            vault.paths,
            vault.guard,
            vault.profile,
            McpNoteAppendArgs {
                note: None,
                text: text.to_string(),
                mode: None,
                heading: None,
                periodic: Some("daily".to_string()),
                date: Some(vault.today.format("%Y-%m-%d").to_string()),
                vars: BTreeMap::new(),
                check: false,
                no_commit: false,
            },
        )
        .map_err(mcp_error)
    })?;
    Ok(ChatReply {
        text: format!("Added to {}", report.path),
        changed_paths: vec![report.path],
        ..ChatReply::default()
    })
}

fn create_task(vault: &ChatVault<'_>, text: &str) -> Result<ChatReply, AppError> {
    match load_vault_config(vault.paths).config.bot.task_target {
        BotTaskTarget::Tasknotes => {
            let report = with_write(vault, || {
                let report = apply_task_add_with_guard(
                    vault.paths,
                    &TaskAddRequest {
                        text: text.to_string(),
                        no_nlp: false,
                        status: None,
                        priority: None,
                        due: None,
                        scheduled: None,
                        contexts: Vec::new(),
                        projects: Vec::new(),
                        tags: Vec::new(),
                        template: None,
                        reminders: Vec::new(),
                        dry_run: false,
                    },
                    Some(vault.guard),
                )?;
                finish_write(vault, "bot-task", &report.changed_paths)?;
                Ok(report)
            })?;
            let mut lines = vec![format!("Task: {}", report.title)];
            if let Some(scheduled) = &report.scheduled {
                lines.push(format!("Scheduled: {scheduled}"));
            }
            if let Some(due) = &report.due {
                lines.push(format!("Due: {due}"));
            }
            lines.push(report.path.clone());
            Ok(ChatReply {
                text: lines.join("\n"),
                changed_paths: report.changed_paths,
                ..ChatReply::default()
            })
        }
        BotTaskTarget::Inline => {
            let report = with_write(vault, || {
                crate::mcp_tasks::task_create(
                    vault.paths,
                    vault.guard,
                    vault.profile,
                    McpTaskCreateArgs {
                        text: text.to_string(),
                        note: None,
                        due: None,
                        priority: None,
                        dry_run: false,
                        no_commit: false,
                    },
                )
                .map_err(mcp_error)
            })?;
            Ok(ChatReply {
                text: format!("Task added: {}", report.task),
                changed_paths: report.changed_paths,
                ..ChatReply::default()
            })
        }
    }
}

/// Open tasks due or scheduled up to `days` days from today (inclusive),
/// from inline tasks and `TaskNotes` alike.
fn due_tasks(vault: &ChatVault<'_>, days: u32) -> Result<ChatReply, AppError> {
    ensure_fresh(vault)?;
    let bound = vault
        .today
        .checked_add_days(chrono::Days::new(u64::from(days) + 1))
        .unwrap_or(vault.today)
        .format("%Y-%m-%d");
    let query = format!(
        "NOT (done OR status.type is cancelled)\n\
         (due before {bound}) OR (scheduled before {bound})\n\
         sort by due"
    );
    let result = build_tasks_query_result_with_guard(vault.paths, &query, vault.guard)?;
    let heading = if days == 0 {
        "Due today".to_string()
    } else {
        format!("Due within {days} days")
    };
    if result.tasks.is_empty() {
        return Ok(ChatReply::text(format!("{heading}: nothing open.")));
    }
    let today = vault.today.format("%Y-%m-%d").to_string();
    let mut lines = vec![format!("{heading} ({}):", result.tasks.len())];
    let mut buttons = Vec::new();
    for task in result.tasks.iter().take(CHAT_LIST_LIMIT) {
        let Some(row) = TaskRow::from_value(task) else {
            continue;
        };
        let date = row
            .due
            .as_deref()
            .or(row.scheduled.as_deref())
            .unwrap_or("");
        let marker = if !date.is_empty() && date.get(..10).unwrap_or(date) < today.as_str() {
            " (overdue)"
        } else {
            ""
        };
        lines.push(format!(
            "- {} {}{marker}",
            row.title,
            date.get(..10).unwrap_or(date)
        ));
        buttons.push(ChatButton {
            label: format!("Done: {}", truncate(&row.title, 40)),
            action: ChatAction::CompleteTask {
                task: row.reference,
                title: row.title,
            },
        });
    }
    if result.tasks.len() > CHAT_LIST_LIMIT {
        lines.push(format!(
            "... and {} more",
            result.tasks.len() - CHAT_LIST_LIMIT
        ));
    }
    Ok(ChatReply {
        text: lines.join("\n"),
        buttons,
        ..ChatReply::default()
    })
}

fn reminders(vault: &ChatVault<'_>, window: &str) -> Result<ChatReply, AppError> {
    ensure_fresh(vault)?;
    let report = build_task_reminders_report_with_guard(vault.paths, window, Some(vault.guard))?;
    if report.reminders.is_empty() {
        return Ok(ChatReply::text(format!("No reminders within {window}.")));
    }
    let mut lines = vec![format!("Reminders within {window}:")];
    for reminder in report.reminders.iter().take(CHAT_LIST_LIMIT) {
        let overdue = if reminder.overdue { " (overdue)" } else { "" };
        lines.push(format!(
            "- {} {}{overdue}",
            reminder.notify_at, reminder.title
        ));
    }
    if report.reminders.len() > CHAT_LIST_LIMIT {
        lines.push(format!(
            "... and {} more",
            report.reminders.len() - CHAT_LIST_LIMIT
        ));
    }
    Ok(ChatReply::text(lines.join("\n")))
}

fn find(vault: &ChatVault<'_>, query: &str) -> Result<ChatReply, AppError> {
    ensure_fresh(vault)?;
    let report = search_vault_with_filter(
        vault.paths,
        &SearchQuery {
            text: query.to_string(),
            limit: Some(CHAT_SEARCH_LIMIT),
            ..SearchQuery::default()
        },
        Some(&vault.guard.read_filter()),
    )
    .map_err(AppError::operation)?;
    if report.hits.is_empty() {
        return Ok(ChatReply::text(format!("No matches for \"{query}\".")));
    }
    let mut lines = vec![format!("Matches for \"{query}\":")];
    let mut buttons = Vec::new();
    let mut seen = Vec::new();
    for hit in &report.hits {
        let snippet = hit.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
        lines.push(format!(
            "- {}: {}",
            hit.document_path,
            truncate(&snippet, 120)
        ));
        if !seen.contains(&hit.document_path) {
            seen.push(hit.document_path.clone());
            buttons.push(ChatButton {
                label: truncate(&hit.document_path, 48),
                action: ChatAction::ShowNote {
                    path: hit.document_path.clone(),
                },
            });
        }
    }
    Ok(ChatReply {
        text: lines.join("\n"),
        buttons,
        ..ChatReply::default()
    })
}

fn show_note(vault: &ChatVault<'_>, reference: &str) -> Result<ChatReply, AppError> {
    ensure_fresh(vault)?;
    let guard: &dyn PermissionGuard = vault.guard;
    let report = read_note(
        vault.paths,
        NoteGetOptions {
            note: reference,
            mode: NoteReadMode::Markdown,
            section_id: None,
            heading: None,
            block_ref: None,
            lines: None,
            match_pattern: None,
            context: 0,
            no_frontmatter: false,
            raw: false,
            evaluate: false,
            dataview_js: DataviewJsRenderPolicy::Off,
            guard: Some(guard),
        },
    )?;
    if report.content.chars().count() <= CHAT_INLINE_TEXT_LIMIT {
        return Ok(ChatReply::text(format!(
            "{}\n\n{}",
            report.path, report.content
        )));
    }
    let file_name = report
        .path
        .rsplit('/')
        .next()
        .unwrap_or(&report.path)
        .to_string();
    Ok(ChatReply {
        text: format!("{} (sent as a file: too long to show)", report.path),
        document: Some(ChatDocument {
            file_name,
            content: report.content,
        }),
        ..ChatReply::default()
    })
}

/// The fields a reply needs from a Tasks query row.
struct TaskRow {
    reference: String,
    title: String,
    due: Option<String>,
    scheduled: Option<String>,
}

impl TaskRow {
    fn from_value(task: &Value) -> Option<Self> {
        let path = task.get("path")?.as_str()?.to_string();
        let text = |key: &str| {
            task.get(key)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .filter(|value| !value.is_empty())
        };
        let inline = task.get("taskSource").and_then(Value::as_str) != Some("file");
        let (reference, title) = if inline {
            let line = task.get("line")?.as_i64()?;
            (
                format!("{path}:{line}"),
                inline_task_title(&text("text").unwrap_or_default()),
            )
        } else {
            let title = text("title")
                .or_else(|| text("text"))
                .unwrap_or_else(|| path.clone());
            (path, title)
        };
        Some(Self {
            reference,
            title,
            due: text("due"),
            scheduled: text("scheduled"),
        })
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn mcp_error(error: McpMethodError) -> AppError {
    match error {
        McpMethodError::JsonRpc { message, .. } | McpMethodError::Tool { message, .. } => {
            AppError::operation(message)
        }
    }
}

#[cfg(test)]
mod tests;
