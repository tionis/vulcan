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
    build_tasks_query_result_with_guard, ReminderTimezone, TaskAddRequest,
};
use crate::AppError;
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use vulcan_core::config::{BotCaptureTarget, BotTaskTarget};
use vulcan_core::expression::functions::parse_date_like_string;
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
    ("undo", "Revert the last capture from this chat"),
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
    /// Handled by the adapter, which keeps each chat's last capture; it
    /// runs [`run_chat_undo`].
    Undo,
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
            Self::Capture { .. } | Self::Daily { .. } | Self::Task { .. } | Self::Undo
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
        "undo" => ChatCommand::Undo,
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
    /// Revert the capture whose undo record has this [`chat_undo_id`], if it
    /// is still the chat's latest. Handled by the adapter, which keeps the
    /// undo record.
    UndoCapture { id: String },
}

/// A short, stable id of an undo record, so an Undo button only reverts the
/// capture it was sent with.
#[must_use]
pub fn chat_undo_id(undo: &[ChatUndo]) -> String {
    let mut hasher = blake3::Hasher::new();
    for entry in undo {
        hasher.update(entry.path.as_bytes());
        hasher.update(&[0]);
        hasher.update(entry.after_hash.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex()[..16].to_string()
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
    /// How to revert this command's writes with [`run_chat_undo`]; empty
    /// when it wrote nothing or cannot be undone. The adapter keeps it for
    /// the chat's `/undo`.
    pub undo: Vec<ChatUndo>,
    /// What a reply to this message acts on: the adapter records it with
    /// the sent message's id.
    pub reply_target: Option<ChatReplyTarget>,
}

/// What replying to a bot message with a time does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatReplyTarget {
    /// Reschedule this task (`path` or `path:line`).
    Task { task: String },
    /// Snooze the reminder in this ledger row (a "Later…" prompt).
    Reminder { row: i64 },
}

/// The hint shown when a reply is not a time.
pub const CHAT_WHEN_HINT: &str = "Reply with a time, e.g. in 2h, 15:00, or tomorrow 9:00.";

/// One file as a capture left it: restore `before` (or remove the file
/// when it did not exist) while its content still hashes to `after_hash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatUndo {
    pub path: String,
    pub before: Option<String>,
    pub after_hash: String,
}

/// Notes larger than this are not kept for `/undo`.
const UNDO_LIMIT_BYTES: usize = 1024 * 1024;

/// What a file held before a write.
enum Prior {
    Absent,
    Content(String),
}

/// The content of `path` before a write, for undo; `None` when it is too
/// large to keep or unreadable.
fn undo_before(paths: &VaultPaths, path: &str) -> Option<Prior> {
    match crate::notes::read_note_for_update(paths, path) {
        Ok(Some(text)) if text.len() > UNDO_LIMIT_BYTES => None,
        Ok(Some(text)) => Some(Prior::Content(text)),
        Ok(None) => Some(Prior::Absent),
        Err(_) => None,
    }
}

/// The undo entry for `path` after a write, given what it held before.
fn undo_after(paths: &VaultPaths, path: &str, before: Option<Prior>) -> Option<ChatUndo> {
    let bytes =
        vulcan_core::paths::secure_read(paths.vault_root(), std::path::Path::new(path)).ok()?;
    Some(ChatUndo {
        path: path.to_string(),
        before: match before? {
            Prior::Absent => None,
            Prior::Content(text) => Some(text),
        },
        after_hash: blake3::hash(&bytes).to_hex().to_string(),
    })
}

impl ChatReply {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
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
        ChatCommand::Vault { .. }
        | ChatCommand::Undo
        | ChatCommand::Unknown { .. }
        | ChatCommand::Empty => Err(AppError::operation(
            "this command is handled by the chat adapter",
        )),
    }
}

/// Revert a capture: every file must still be exactly as the capture
/// left it, or nothing is changed.
pub fn run_chat_undo(vault: &ChatVault<'_>, undo: &[ChatUndo]) -> Result<ChatReply, AppError> {
    if undo.is_empty() {
        return Ok(ChatReply::text("Nothing to undo."));
    }
    let root = vault.paths.vault_root();
    let changed = with_write(vault, || {
        let _lock = vulcan_core::write_lock::acquire_write_lock(vault.paths)
            .map_err(AppError::operation)?;
        for entry in undo {
            vault
                .guard
                .check_write_path(&entry.path)
                .map_err(AppError::operation)?;
            let unchanged =
                vulcan_core::paths::secure_read(root, std::path::Path::new(&entry.path))
                    .is_ok_and(|bytes| blake3::hash(&bytes).to_hex().as_str() == entry.after_hash);
            if !unchanged {
                return Ok(Err(entry.path.clone()));
            }
        }
        for entry in undo {
            let path = std::path::Path::new(&entry.path);
            match &entry.before {
                Some(before) => {
                    if crate::notes::note_path_is_mdbase_managed(
                        vault.paths,
                        &entry.path,
                        Some(vault.profile),
                    )? {
                        return Err(AppError::operation(format!(
                            "{} is managed by an mdbase collection; undo it by hand",
                            entry.path
                        )));
                    }
                    vulcan_core::paths::secure_replace(root, path, before)
                        .map_err(AppError::operation)?;
                }
                None => {
                    vulcan_core::paths::secure_remove(root, path).map_err(AppError::operation)?;
                }
            }
        }
        let changed = undo
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        Ok(Ok(changed))
    })?;
    match changed {
        Ok(changed) => {
            finish_write(vault, "bot-undo", &changed)?;
            Ok(ChatReply {
                text: format!("Undone: {}", changed.join(", ")),
                changed_paths: changed,
                ..ChatReply::default()
            })
        }
        Err(path) => Ok(ChatReply::text(format!(
            "{path} changed since the capture; nothing was undone."
        ))),
    }
}

/// A file sent to the bot: a photo, document, or voice message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatAttachment {
    /// The sender's file name, or a generated one such as
    /// `photo-20261107-101500.jpg`. It is sanitized before use.
    pub file_name: String,
    pub bytes: Vec<u8>,
    /// Text sent with the file, captured beside the embed.
    pub caption: Option<String>,
}

/// Save an attachment under `bot.attachment_folder` and capture an embed
/// of it, with its caption, to the default capture target.
pub fn run_chat_attachment(
    vault: &ChatVault<'_>,
    attachment: &ChatAttachment,
) -> Result<ChatReply, AppError> {
    let config = load_vault_config(vault.paths).config;
    let folder = match config.bot.attachment_folder.trim() {
        "" | "." | "./" => String::new(),
        folder => vulcan_core::paths::normalize_relative_input_path(
            folder,
            vulcan_core::paths::RelativePathOptions {
                expected_extension: None,
                append_extension_if_missing: false,
            },
        )
        .map_err(|_| {
            AppError::operation(format!(
                "bot.attachment_folder must be a folder inside the vault: {folder}"
            ))
        })?,
    };
    let name = sanitize_file_name(&attachment.file_name);
    let path = with_write(vault, || {
        let root = vault.paths.vault_root();
        let (stem, extension) = name
            .rsplit_once('.')
            .map_or((name.as_str(), String::new()), |(stem, extension)| {
                (stem, format!(".{extension}"))
            });
        let path = (0..100)
            .map(|attempt| {
                let file = if attempt == 0 {
                    name.clone()
                } else {
                    format!("{stem}-{attempt}{extension}")
                };
                if folder.is_empty() {
                    file
                } else {
                    format!("{folder}/{file}")
                }
            })
            .find(|path| !root.join(path).exists())
            .ok_or_else(|| AppError::operation("could not find a free attachment file name"))?;
        vault
            .guard
            .check_write_path(&path)
            .map_err(AppError::operation)?;
        vulcan_core::paths::secure_create_atomic(
            root,
            std::path::Path::new(&path),
            &attachment.bytes,
        )
        .map_err(AppError::operation)?;
        finish_write(vault, "bot-attachment", std::slice::from_ref(&path))?;
        Ok(path)
    })?;
    let saved = ChatUndo {
        path: path.clone(),
        before: None,
        after_hash: blake3::hash(&attachment.bytes).to_hex().to_string(),
    };
    let embed = format!("![[{path}]]");
    let text = match attachment.caption.as_deref().map(str::trim) {
        Some(caption) if !caption.is_empty() => format!("{caption} {embed}"),
        _ => embed,
    };
    let mut reply = capture(vault, &text)?;
    reply.text = format!("Saved {path}\n{}", reply.text);
    reply.changed_paths.insert(0, path);
    // Undo removes the file only together with the capture that embeds it.
    if !reply.undo.is_empty() {
        reply.undo.insert(0, saved);
    }
    Ok(reply)
}

/// A file name safe on every platform: letters, digits, `.`, `-`, and `_`,
/// at most 100 characters, never hidden or empty.
fn sanitize_file_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut clean = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    clean = clean.trim_start_matches(['.', '-']).to_string();
    if clean.chars().count() > 100 {
        let extension = clean
            .rsplit_once('.')
            .map(|(_, extension)| extension.chars().take(10).collect::<String>())
            .unwrap_or_default();
        let stem = clean.chars().take(88).collect::<String>();
        clean = if extension.is_empty() {
            stem
        } else {
            format!("{stem}.{extension}")
        };
    }
    if clean.is_empty() || clean.starts_with('.') {
        format!("file{clean}")
    } else {
        clean
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
        ChatAction::UndoCapture { .. } => Err(AppError::operation(
            "undo buttons are handled by the chat adapter",
        )),
    }
}

/// Move `task` to the time in `text`, a reply to the message that created
/// it. A `TaskNotes` task keeps its date field: `scheduled` when it has only
/// that, `due` otherwise; an inline task moves its due date.
pub fn run_chat_reschedule(
    vault: &ChatVault<'_>,
    task: &str,
    text: &str,
    now_ms: i64,
) -> Result<ChatReply, AppError> {
    let config = load_vault_config(vault.paths).config;
    let timezone = ReminderTimezone::from_config(&config.tasks.reminders)?;
    let Some(when) = when::parse_chat_when(text, &config.tasknotes, timezone, now_ms) else {
        return Ok(ChatReply::text(CHAT_WHEN_HINT));
    };
    let value = match when {
        when::ChatWhen::At(at) => at.format("%Y-%m-%dT%H:%M").to_string(),
        when::ChatWhen::Day(day) => day.format("%Y-%m-%d").to_string(),
    };
    let shown =
        crate::tasks::build_task_show_report_with_guard(vault.paths, task, vault.guard).ok();
    let report = with_write(vault, || {
        let report = match &shown {
            Some(note) if note.scheduled.is_some() && note.due.is_none() => {
                crate::tasks::apply_task_set_with_guard(
                    vault.paths,
                    &crate::tasks::TaskSetRequest {
                        task: task.to_string(),
                        property: "scheduled".to_string(),
                        value: value.clone(),
                        dry_run: false,
                    },
                    Some(vault.guard),
                )?
            }
            _ => crate::tasks::apply_task_reschedule_with_guard(
                vault.paths,
                &crate::tasks::TaskRescheduleRequest {
                    task: task.to_string(),
                    due: value.clone(),
                    dry_run: false,
                },
                Some(vault.guard),
            )?,
        };
        finish_write(vault, "bot-reschedule", &report.changed_paths)?;
        Ok(report)
    })?;
    let field = match &shown {
        Some(note) if note.scheduled.is_some() && note.due.is_none() => "Scheduled",
        _ => "Due",
    };
    let title = shown.map_or_else(|| task.to_string(), |note| note.title);
    let mut lines = vec![format!(
        "Moved: {title}\n{field}: {}",
        friendly_task_date(&value, vault.today)
    )];
    if shown_has_reminders(vault, task) {
        lines.extend(task_reminder_lines(vault, task));
    }
    Ok(ChatReply {
        text: lines.join("\n"),
        changed_paths: report.changed_paths,
        reply_target: Some(ChatReplyTarget::Task {
            task: task.to_string(),
        }),
        ..ChatReply::default()
    })
}

fn shown_has_reminders(vault: &ChatVault<'_>, task: &str) -> bool {
    crate::tasks::build_task_show_report_with_guard(vault.paths, task, vault.guard)
        .is_ok_and(|note| !note.reminders.is_empty())
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
pub(crate) fn ensure_fresh(vault: &ChatVault<'_>) -> Result<(), AppError> {
    if matches!(
        vulcan_core::prove_note_store_fresh(vault.paths),
        Ok(vulcan_core::NoteStoreFreshness::Fresh)
    ) {
        return Ok(());
    }
    refresh_cache_incrementally(vault.paths).map(|_| ())
}

fn capture(vault: &ChatVault<'_>, text: &str) -> Result<ChatReply, AppError> {
    let linked = links::link_with_title(vault, text);
    let text = linked.as_deref().unwrap_or(text);
    match load_vault_config(vault.paths).config.bot.default_capture {
        BotCaptureTarget::Inbox => capture_inbox(vault, text),
        BotCaptureTarget::Daily => append_daily(vault, text),
    }
}

fn capture_inbox(vault: &ChatVault<'_>, text: &str) -> Result<ChatReply, AppError> {
    let (path, undo) = with_write(vault, || {
        let inbox = crate::inbox::inbox_path(&load_vault_config(vault.paths).config.inbox)?;
        vault
            .guard
            .check_write_path(&inbox)
            .map_err(AppError::operation)?;
        let before = undo_before(vault.paths, &inbox);
        let path = crate::inbox::write_inbox_capture(
            vault.paths,
            text,
            Some(vault.profile),
            Verbosity::Quiet,
        )?;
        let undo = (path == inbox)
            .then(|| undo_after(vault.paths, &path, before))
            .flatten();
        finish_write(vault, "bot-capture", std::slice::from_ref(&path))?;
        Ok((path, undo))
    })?;
    Ok(ChatReply {
        text: format!("Captured to {path}"),
        changed_paths: vec![path],
        undo: undo.into_iter().collect(),
        ..ChatReply::default()
    })
}

fn append_daily(vault: &ChatVault<'_>, text: &str) -> Result<ChatReply, AppError> {
    let date = vault.today.format("%Y-%m-%d").to_string();
    let expected = vulcan_core::periodic::expected_periodic_note_path(
        &load_vault_config(vault.paths).config.periodic,
        "daily",
        &date,
    );
    let (report, undo) = with_write(vault, || {
        let before = expected
            .as_deref()
            .and_then(|path| undo_before(vault.paths, path));
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
                date: Some(date.clone()),
                vars: BTreeMap::new(),
                check: false,
                no_commit: false,
            },
        )
        .map_err(mcp_error)
        .map(|report| {
            let undo = (expected.as_deref() == Some(report.path.as_str()))
                .then(|| undo_after(vault.paths, &report.path, before))
                .flatten();
            (report, undo)
        })
    })?;
    Ok(ChatReply {
        text: format!("Added to {}", report.path),
        changed_paths: vec![report.path],
        undo: undo.into_iter().collect(),
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
            let mut lines = vec![format!("Task added: {}", report.title)];
            if let Some(scheduled) = &report.scheduled {
                lines.push(format!(
                    "Scheduled: {}",
                    friendly_task_date(scheduled, vault.today)
                ));
            }
            if let Some(due) = &report.due {
                lines.push(format!("Due: {}", friendly_task_date(due, vault.today)));
            }
            lines.extend(task_reminder_lines(vault, &report.path));
            lines.push(report.path.clone());
            // `tasks add` always creates a new note, so undo removes it.
            let undo = undo_after(vault.paths, &report.path, Some(Prior::Absent));
            Ok(ChatReply {
                text: lines.join("\n"),
                changed_paths: report.changed_paths,
                undo: undo.into_iter().collect(),
                reply_target: Some(ChatReplyTarget::Task {
                    task: report.path.clone(),
                }),
                ..ChatReply::default()
            })
        }
        BotTaskTarget::Inline => {
            let inbox = crate::inbox::inbox_path(&load_vault_config(vault.paths).config.inbox).ok();
            let (report, undo) = with_write(vault, || {
                let before = inbox
                    .as_deref()
                    .and_then(|path| undo_before(vault.paths, path));
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
                .map(|report| {
                    let undo = match (inbox.as_deref(), report.changed_paths.as_slice()) {
                        (Some(inbox), [changed]) if changed == inbox => {
                            undo_after(vault.paths, inbox, before)
                        }
                        _ => None,
                    };
                    (report, undo)
                })
            })?;
            Ok(ChatReply {
                text: format!("Task added: {}", inline_task_title(&report.task)),
                changed_paths: report.changed_paths,
                undo: undo.into_iter().collect(),
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
        let when = if date.is_empty() {
            String::new()
        } else {
            format!(" · {}", friendly_task_date(date, vault.today))
        };
        lines.push(format!("• {}{when}{marker}", row.title));
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

/// The daily digest of `vault`, named `vault_name`: open tasks due or
/// scheduled up to today, with Done buttons, and the reminders still to
/// come today. `None` when there is nothing to report.
pub fn chat_digest(
    vault: &ChatVault<'_>,
    vault_name: &str,
    now_ms: i64,
) -> Result<Option<ChatReply>, AppError> {
    let due = due_tasks(vault, 0)?;
    let has_tasks = !due.buttons.is_empty();
    let timezone =
        ReminderTimezone::from_config(&load_vault_config(vault.paths).config.tasks.reminders)?;
    let local_now = timezone.local_datetime(now_ms);
    let midnight = local_now.date().succ_opt().unwrap_or(local_now.date());
    let minutes_left = (midnight.and_time(chrono::NaiveTime::MIN) - local_now)
        .num_minutes()
        .max(1);
    let report = build_task_reminders_report_with_guard(
        vault.paths,
        &format!("{minutes_left}m"),
        Some(vault.guard),
    )?;
    let upcoming = report
        .reminders
        .iter()
        .filter(|reminder| !reminder.overdue)
        .filter_map(|reminder| {
            let local = timezone.local_datetime(parse_date_like_string(&reminder.notify_at)?);
            Some(format!("• {} {}", local.format("%H:%M"), reminder.title))
        })
        .take(CHAT_LIST_LIMIT)
        .collect::<Vec<_>>();
    if !has_tasks && upcoming.is_empty() {
        return Ok(None);
    }
    let mut lines = vec![format!(
        "☀️ {} · {vault_name}",
        day_heading(vault.today, vault.today)
    )];
    if has_tasks {
        lines.push(String::new());
        lines.push(due.text);
    }
    if !upcoming.is_empty() {
        lines.push(String::new());
        lines.push("Reminders today:".to_string());
        lines.extend(upcoming);
    }
    Ok(Some(ChatReply {
        text: lines.join("\n"),
        buttons: due.buttons,
        ..ChatReply::default()
    }))
}

fn reminders(vault: &ChatVault<'_>, window: &str) -> Result<ChatReply, AppError> {
    ensure_fresh(vault)?;
    let report = build_task_reminders_report_with_guard(vault.paths, window, Some(vault.guard))?;
    if report.reminders.is_empty() {
        return Ok(ChatReply::text(format!(
            "No reminders in the next {window}."
        )));
    }
    let timezone =
        ReminderTimezone::from_config(&load_vault_config(vault.paths).config.tasks.reminders)?;
    let mut lines = vec![format!("Reminders in the next {window}:")];
    let mut current_day = None;
    for reminder in report.reminders.iter().take(CHAT_LIST_LIMIT) {
        let Some(local) =
            parse_date_like_string(&reminder.notify_at).map(|ms| timezone.local_datetime(ms))
        else {
            lines.push(format!("• {} {}", reminder.notify_at, reminder.title));
            continue;
        };
        if current_day != Some(local.date()) {
            current_day = Some(local.date());
            lines.push(String::new());
            lines.push(day_heading(local.date(), vault.today));
        }
        let overdue = if reminder.overdue { " (overdue)" } else { "" };
        lines.push(format!(
            "• {} {}{overdue}",
            local.format("%H:%M"),
            reminder.title
        ));
    }
    if report.reminders.len() > CHAT_LIST_LIMIT {
        lines.push(String::new());
        lines.push(format!(
            "... and {} more",
            report.reminders.len() - CHAT_LIST_LIMIT
        ));
    }
    Ok(ChatReply::text(lines.join("\n")))
}

/// `Today, Sat 7 Nov`, `Tomorrow, Sun 8 Nov`, `Tue 10 Nov`, or with the year when it differs.
fn day_heading(date: NaiveDate, today: NaiveDate) -> String {
    let label = calendar_date(date, today);
    match relative_day(date, today) {
        Some(relative) => format!("{relative}, {label}"),
        None => label,
    }
}

/// `Sun 8 Nov`, with the year when it is not `today`'s.
pub(crate) fn calendar_date(date: NaiveDate, today: NaiveDate) -> String {
    let format = if date.year() == today.year() {
        "%a %-d %b"
    } else {
        "%a %-d %b %Y"
    };
    date.format(format).to_string()
}

fn relative_day(date: NaiveDate, today: NaiveDate) -> Option<&'static str> {
    match (date - today).num_days() {
        -1 => Some("Yesterday"),
        0 => Some("Today"),
        1 => Some("Tomorrow"),
        _ => None,
    }
}

/// A day relative to `today` where that is shorter: `Today`, `Tomorrow`,
/// `Yesterday`, else `Sun 8 Nov`.
#[must_use]
pub fn friendly_day(date: NaiveDate, today: NaiveDate) -> String {
    relative_day(date, today).map_or_else(|| calendar_date(date, today), ToString::to_string)
}

/// A task date or date-time as written in frontmatter or inline fields
/// (`2026-11-08`, `2026-11-08T12:00`), as `Tomorrow` or `Tomorrow 12:00`.
/// Anything else is returned unchanged.
#[must_use]
pub fn friendly_task_date(value: &str, today: NaiveDate) -> String {
    let value = value.trim();
    let Some(date) = value
        .get(..10)
        .and_then(|day| NaiveDate::parse_from_str(day, "%Y-%m-%d").ok())
    else {
        return value.to_string();
    };
    let time = value
        .get(11..16)
        .filter(|_| {
            value
                .as_bytes()
                .get(10)
                .is_some_and(|c| matches!(c, b'T' | b' '))
        })
        .and_then(|time| chrono::NaiveTime::parse_from_str(time, "%H:%M").ok());
    match time {
        Some(time) => format!("{} {}", friendly_day(date, today), time.format("%H:%M")),
        None => friendly_day(date, today),
    }
}

/// `Reminder: Tomorrow 12:00` lines for the task note at `path`, in the
/// reminder timezone. A reminder that was already due when the task was
/// created is not delivered, so it is shown as passed.
fn task_reminder_lines(vault: &ChatVault<'_>, path: &str) -> Vec<String> {
    let Ok(timezone) =
        ReminderTimezone::from_config(&load_vault_config(vault.paths).config.tasks.reminders)
    else {
        return Vec::new();
    };
    let Ok(report) = build_task_reminders_report_with_guard(vault.paths, "400d", Some(vault.guard))
    else {
        return Vec::new();
    };
    report
        .reminders
        .iter()
        .filter(|reminder| reminder.path == path)
        .filter_map(|reminder| {
            let at = parse_date_like_string(&reminder.notify_at)?;
            let local = timezone.local_datetime(at);
            let when = format!(
                "{} {}",
                friendly_day(local.date(), vault.today),
                local.format("%H:%M")
            );
            Some(if reminder.overdue {
                format!("Reminder: {when} (already passed, not sent)")
            } else {
                format!("Reminder: {when}")
            })
        })
        .collect()
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

mod links;
pub mod reminders;
pub mod state;
pub mod when;

#[cfg(test)]
mod tests;
