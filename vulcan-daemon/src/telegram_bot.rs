//! The Telegram chat bot service (`docs/specs/chat-bot.md`).
//!
//! It long-polls the Bot API, answers allowlisted users in private chats,
//! and runs their messages as `vulcan_app::chat` commands against the
//! user's active vault, with the user's permission profile capped by the
//! vault registration's profile. Writes hold the host's per-vault mutation
//! permit. Messages from anyone else get no reply at all.

use crate::registry::{DaemonBotUser, DaemonTelegramBotConfig, WikiRegistration, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::telegram_api::{
    InlineButton, TelegramCallbackQuery, TelegramClient, TelegramError, TelegramFileRef,
    TelegramMessage, TelegramUpdate, TELEGRAM_FILE_LIMIT,
};
use crate::worker_gate::HostedWorkerGate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vulcan_app::chat::reminders::{
    reminder_action_label, reminder_steps, run_reminder_action, run_reminder_snooze_text,
    PlannedReminder, ReminderPlan, ReminderStep, LATER_ACTION,
};
use vulcan_app::chat::state::{ChatStateStore, ReminderLedgerEntry, ReminderState};
use vulcan_app::chat::{
    chat_digest, chat_help_text, chat_undo_id, chat_usage_text, parse_chat_message,
    run_chat_action, run_chat_attachment, run_chat_command, run_chat_reschedule, run_chat_undo,
    ChatAction, ChatAttachment, ChatButton, ChatCommand, ChatReply, ChatReplyTarget, ChatVault,
    CHAT_COMMANDS, CHAT_WHEN_HINT,
};
use vulcan_app::execution::MutationGate;
use vulcan_app::tasks::ReminderSettings;
use vulcan_core::{load_vault_config, ProfilePermissionGuard, VaultPaths};

const PLATFORM: &str = "telegram";
const STATUS_FILE: &str = "daemon/telegram-bot.json";
/// Long-poll duration. Short enough that shutdown never waits long.
const POLL_SECONDS: u64 = 8;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const CONFLICT_BACKOFF: Duration = Duration::from_secs(30);
const CALLBACK_PREFIX: &str = "a:";
/// Reminder buttons: `r:<ledger row>:<offered action index>`.
const REMINDER_PREFIX: &str = "r:";
const MAX_REJECTED_USERS: usize = 32;
/// How often reminder plans are rebuilt from the vaults. Due reminders are
/// checked on every poll cycle in between.
const REPLAN_MS: i64 = if cfg!(test) { 200 } else { 60 * 1_000 };

/// What `vulcan daemon bot-status` shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramBotStatus {
    /// `starting`, `polling`, `conflict`, `backoff`, `rate_limited`, `stopped`, or `failed`.
    pub state: String,
    pub bot_username: Option<String>,
    pub last_poll_ms: Option<i64>,
    pub last_error: Option<String>,
    pub handled_updates: u64,
    #[serde(default)]
    pub reminders_sent: u64,
    /// Attempts by users not on the allowlist, by Telegram user id.
    pub rejected_users: BTreeMap<i64, u64>,
}

#[must_use]
pub fn telegram_bot_status_path(state_root: &Path) -> PathBuf {
    state_root.join(STATUS_FILE)
}

pub fn read_telegram_bot_status(state_root: &Path) -> Result<Option<TelegramBotStatus>, String> {
    match std::fs::read(telegram_bot_status_path(state_root)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Configuration and last-known runtime state, for `vulcan daemon bot-status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramBotReport {
    pub configured: bool,
    pub token_env: Option<String>,
    /// Whether the token variable is set in this process's environment.
    pub token_set: bool,
    pub users: Vec<TelegramBotUserSummary>,
    /// Written by the running service; absent before its first start.
    pub status: Option<TelegramBotStatus>,
    /// Reminder ledger entries by state (`open`, `sending`, `expired`,
    /// `resolved`).
    pub reminders: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramBotUserSummary {
    pub telegram_id: i64,
    pub vaults: Vec<String>,
    pub default_vault: Option<String>,
    pub profile: String,
    pub reminders: bool,
}

pub fn telegram_bot_report(
    config: &crate::registry::DaemonConfig,
    state_root: &Path,
) -> Result<TelegramBotReport, String> {
    let telegram = config.bot.telegram.as_ref();
    Ok(TelegramBotReport {
        configured: telegram.is_some(),
        token_env: telegram.map(|telegram| telegram.token_env.clone()),
        token_set: telegram.and_then(telegram_bot_token).is_some(),
        users: telegram
            .map(|telegram| {
                telegram
                    .users
                    .iter()
                    .map(|user| TelegramBotUserSummary {
                        telegram_id: user.telegram_id,
                        vaults: user.vaults.iter().map(ToString::to_string).collect(),
                        default_vault: user.default_vault.as_ref().map(ToString::to_string),
                        profile: user.profile.clone(),
                        reminders: user.reminders,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        status: read_telegram_bot_status(state_root)?,
        reminders: if ChatStateStore::path(state_root).is_file() {
            ChatStateStore::open(state_root)
                .and_then(|state| state.reminder_counts(PLATFORM))
                .map_err(|error| error.to_string())?
                .into_iter()
                .collect()
        } else {
            BTreeMap::new()
        },
    })
}

/// The token for `config`, from the process environment (which includes
/// `daemon.env`). `None` when the variable is unset or empty.
#[must_use]
pub fn telegram_bot_token(config: &DaemonTelegramBotConfig) -> Option<String> {
    std::env::var(&config.token_env)
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

struct Bot<'a> {
    client: TelegramClient,
    registry: &'a WikiRegistry,
    state_root: &'a Path,
    gate: Option<&'a HostedWorkerGate>,
    state: ChatStateStore,
    status: TelegramBotStatus,
    /// Each reminder recipient's plan per vault, refreshed every minute.
    plans: Vec<RecipientPlan>,
    next_plan_ms: i64,
    next_digest_ms: i64,
}

struct RecipientPlan {
    chat_id: i64,
    vault: String,
    plan: ReminderPlan,
}

/// Run the bot until `stop` is cancelled. Returns an error when the token
/// is rejected; transient failures back off and continue.
pub fn run_telegram_bot(
    config: &DaemonTelegramBotConfig,
    token: String,
    registry: &WikiRegistry,
    state_root: &Path,
    gate: Option<&HostedWorkerGate>,
    stop: &ShutdownSignal,
) -> Result<(), String> {
    let client = TelegramClient::new(config.api_base.as_deref(), token)
        .map_err(|error| error.to_string())?;
    let state = ChatStateStore::open(state_root).map_err(|error| error.to_string())?;
    let mut bot = Bot {
        client,
        registry,
        state_root,
        gate,
        state,
        status: read_telegram_bot_status(state_root)
            .ok()
            .flatten()
            .unwrap_or_default(),
        plans: Vec::new(),
        next_plan_ms: 0,
        next_digest_ms: 0,
    };
    bot.status.state = "starting".to_string();
    bot.status.last_error = None;
    bot.save_status();

    let me = loop {
        match bot.client.get_me() {
            Ok(me) => break me,
            Err(TelegramError::Unauthorized) => return bot.fail(&TelegramError::Unauthorized),
            Err(error) => {
                bot.note_error("backoff", &error);
                if stop.wait_timeout(MAX_BACKOFF) {
                    bot.stopped();
                    return Ok(());
                }
            }
        }
    };
    bot.status.bot_username = me.username;
    // Best effort: the command menu is a convenience.
    let _ = bot.client.set_my_commands(CHAT_COMMANDS);

    let mut backoff = Duration::from_secs(1);
    while !stop.is_cancelled() {
        let offset = bot
            .state
            .offset(PLATFORM)
            .map_err(|error| error.to_string())?;
        match bot.client.get_updates(offset, POLL_SECONDS) {
            Ok(updates) => {
                backoff = Duration::from_secs(1);
                bot.status.state = "polling".to_string();
                bot.status.last_poll_ms = Some(vulcan_core::current_utc_timestamp_ms());
                bot.status.last_error = None;
                for update in updates {
                    let next = update.update_id + 1;
                    bot.handle_update(&update);
                    // The offset moves only after an update was handled, so
                    // a crash re-delivers it rather than losing it.
                    bot.state
                        .set_offset(PLATFORM, next)
                        .map_err(|error| error.to_string())?;
                    bot.status.handled_updates += 1;
                }
                // Only while this device holds the poll: a second device in
                // conflict must not deliver the same reminders.
                bot.deliver_reminders();
                bot.deliver_digests();
                bot.save_status();
            }
            Err(TelegramError::Unauthorized) => return bot.fail(&TelegramError::Unauthorized),
            Err(TelegramError::Conflict) => {
                bot.note_error("conflict", &TelegramError::Conflict);
                if stop.wait_timeout(CONFLICT_BACKOFF) {
                    break;
                }
            }
            Err(TelegramError::RetryAfter(seconds)) => {
                bot.note_error("rate_limited", &TelegramError::RetryAfter(seconds));
                if stop.wait_timeout(Duration::from_secs(seconds.clamp(1, 300))) {
                    break;
                }
            }
            Err(error) => {
                bot.note_error("backoff", &error);
                if stop.wait_timeout(backoff) {
                    break;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
    bot.stopped();
    Ok(())
}

impl Bot<'_> {
    fn save_status(&self) {
        let path = telegram_bot_status_path(self.state_root);
        if let (Some(parent), Ok(bytes)) = (path.parent(), serde_json::to_vec_pretty(&self.status))
        {
            let _ = std::fs::create_dir_all(parent);
            let temporary = path.with_extension("json.tmp");
            if std::fs::write(&temporary, bytes).is_ok() {
                let _ = std::fs::rename(&temporary, &path);
            }
        }
    }

    fn note_error(&mut self, state: &str, error: &TelegramError) {
        self.status.state = state.to_string();
        self.status.last_error = Some(error.to_string());
        self.save_status();
    }

    fn fail(&mut self, error: &TelegramError) -> Result<(), String> {
        self.note_error("failed", error);
        Err(error.to_string())
    }

    fn stopped(&mut self) {
        self.status.state = "stopped".to_string();
        self.save_status();
    }

    /// The allowlisted user `id`, from the current registry, so allowlist
    /// changes apply without a restart.
    fn user(&self, id: i64) -> Option<DaemonBotUser> {
        self.registry
            .poll()
            .ok()?
            .bot
            .telegram?
            .users
            .into_iter()
            .find(|user| user.telegram_id == id)
    }

    fn reject(&mut self, id: i64) {
        // Bounded, so a flood of strangers cannot grow the status file.
        if self.status.rejected_users.len() < MAX_REJECTED_USERS
            || self.status.rejected_users.contains_key(&id)
        {
            *self.status.rejected_users.entry(id).or_default() += 1;
        }
    }

    fn handle_update(&mut self, update: &TelegramUpdate) {
        if let Some(message) = &update.message {
            self.handle_message(message);
        } else if let Some(callback) = &update.callback_query {
            self.handle_callback(callback);
        }
    }

    fn handle_message(&mut self, message: &TelegramMessage) {
        let Some(from) = &message.from else {
            return;
        };
        if message.chat.kind != "private" || from.is_bot {
            return;
        }
        let Some(user) = self.user(from.id) else {
            self.reject(from.id);
            return;
        };
        let chat_id = message.chat.id;
        if let (Some(replied), Some(text)) = (&message.reply_to_message, message.text.as_deref()) {
            if !text.trim_start().starts_with('/')
                && self.handle_reply(&user, chat_id, replied.message_id, text)
            {
                return;
            }
        }
        if let Some((file, file_name)) = attachment_of(message) {
            let (reply, vault) = self.save_attachment(&user, chat_id, message, file, file_name);
            self.remember_undo(chat_id, vault.as_deref(), &reply);
            self.send_reply(chat_id, vault.as_deref(), &reply);
            return;
        }
        let text = message
            .text
            .as_deref()
            .or(message.caption.as_deref())
            .unwrap_or_default();
        // Buttons are stored against the vault the reply came from.
        let (reply, vault) = match parse_chat_message(text) {
            ChatCommand::Empty => (
                ChatReply::text("Send text, a photo, a file, or a voice message."),
                None,
            ),
            ChatCommand::Help => (ChatReply::text(chat_help_text()), None),
            ChatCommand::Usage { command } => (ChatReply::text(chat_usage_text(command)), None),
            ChatCommand::Unknown { command } => (
                ChatReply::text(format!(
                    "Unknown command /{command}. Send /help for the list."
                )),
                None,
            ),
            ChatCommand::Vault { name } => {
                (self.switch_vault(&user, chat_id, name.as_deref()), None)
            }
            ChatCommand::Undo => (self.undo(&user, chat_id), None),
            command => match self.active_vault(&user, chat_id) {
                Ok((wiki, vault_id)) => {
                    match self.with_vault(&user, &wiki, |vault| run_chat_command(vault, &command)) {
                        Ok(reply) => (reply, Some(vault_id)),
                        Err(error) => (ChatReply::text(format!("Error: {error}")), None),
                    }
                }
                Err(error) => (ChatReply::text(error), None),
            },
        };
        self.remember_undo(chat_id, vault.as_deref(), &reply);
        self.send_reply(chat_id, vault.as_deref(), &reply);
    }

    fn handle_callback(&mut self, callback: &TelegramCallbackQuery) {
        let Some(user) = self.user(callback.from.id) else {
            self.reject(callback.from.id);
            return;
        };
        let Some(message) = &callback.message else {
            let _ = self.client.answer_callback_query(&callback.id, None);
            return;
        };
        let chat_id = message.chat.id;
        let now = vulcan_core::current_utc_timestamp_ms();
        if let Some(press) = callback
            .data
            .as_deref()
            .and_then(|data| data.strip_prefix(REMINDER_PREFIX))
        {
            self.handle_reminder_press(&user, callback, chat_id, press, now);
            return;
        }
        let stored = callback
            .data
            .as_deref()
            .and_then(|data| data.strip_prefix(CALLBACK_PREFIX))
            .and_then(|token| {
                self.state
                    .action(PLATFORM, chat_id, token, now)
                    .ok()
                    .flatten()
                    .map(|stored| (token.to_string(), stored))
            });
        let Some((token, stored)) = stored else {
            let _ = self
                .client
                .answer_callback_query(&callback.id, Some("This button has expired."));
            return;
        };
        if let vulcan_app::chat::ChatAction::UndoCapture { id } = &stored.action {
            self.press_undo(&user, callback, chat_id, &token, id, now);
            return;
        }
        let wiki = match self.user_vault(&user, &stored.vault) {
            Ok(wiki) => wiki,
            Err(error) => {
                let _ = self
                    .client
                    .answer_callback_query(&callback.id, Some(&error));
                return;
            }
        };
        let completes = matches!(
            stored.action,
            vulcan_app::chat::ChatAction::CompleteTask { .. }
        );
        let result = self.with_vault(&user, &wiki, |vault| run_chat_action(vault, &stored.action));
        let _ = self.client.answer_callback_query(&callback.id, None);
        match result {
            Ok(reply) => {
                if completes {
                    // A task completes once; a second press finds nothing.
                    let _ = self.state.remove_action(&token);
                }
                self.send_reply(chat_id, Some(&stored.vault), &reply);
            }
            Err(error) => {
                self.send_reply(chat_id, None, &ChatReply::text(format!("Error: {error}")));
            }
        }
    }

    /// A reminder button: accepted only from the chat the reminder went to,
    /// for an open entry, and for an action it offered.
    fn handle_reminder_press(
        &mut self,
        user: &DaemonBotUser,
        callback: &TelegramCallbackQuery,
        chat_id: i64,
        press: &str,
        now: i64,
    ) {
        let found = press
            .split_once(':')
            .and_then(|(row, index)| Some((row.parse::<i64>().ok()?, index.parse::<usize>().ok()?)))
            .and_then(|(row, index)| {
                let (_, entry) = self.state.reminder_by_row(PLATFORM, chat_id, row).ok()??;
                let action = entry.actions.get(index)?.clone();
                (entry.state == ReminderState::Open).then_some((row, entry, action))
            });
        let Some((row, entry, action)) = found else {
            let _ = self
                .client
                .answer_callback_query(&callback.id, Some("This reminder is no longer open."));
            return;
        };
        if action == LATER_ACTION {
            let _ = self.client.answer_callback_query(&callback.id, None);
            self.prompt_later(chat_id, row, &entry, now);
            return;
        }
        let result = self.user_vault(user, &entry.vault).and_then(|wiki| {
            self.with_vault(user, &wiki, |vault| {
                run_reminder_action(vault, &entry.occurrence_id, &action, now)
            })
        });
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = self
                    .client
                    .answer_callback_query(&callback.id, Some(&format!("Error: {error}")));
                return;
            }
        };
        let _ = self.client.answer_callback_query(&callback.id, None);
        self.resolve_reminder(chat_id, entry, &outcome, now);
    }

    /// Append `outcome` to an open reminder's message, drop its buttons, and
    /// record it as resolved.
    fn resolve_reminder(
        &mut self,
        chat_id: i64,
        mut entry: ReminderLedgerEntry,
        outcome: &str,
        now: i64,
    ) {
        let text = format!("{}\n\n{outcome}", entry.text);
        if let Some(message_id) = entry.message_id {
            if let Err(error) = self.client.edit_message_text(chat_id, message_id, &text) {
                self.status.last_error = Some(error.to_string());
            }
        }
        entry.state = ReminderState::Resolved;
        entry.text = text;
        entry.updated_ms = now;
        if let Err(error) = self.state.put_reminder(PLATFORM, &entry) {
            self.status.last_error = Some(format!("reminder ledger: {error}"));
        }
        // The snoozed or next occurrence is planned without waiting a minute.
        self.next_plan_ms = 0;
    }

    /// Ask when to remind again; the reply snoozes reminder `row`.
    fn prompt_later(&mut self, chat_id: i64, row: i64, entry: &ReminderLedgerEntry, now: i64) {
        let title = entry
            .text
            .lines()
            .next()
            .unwrap_or_default()
            .trim_start_matches("⏰ ");
        let question = format!("When should I remind you about {title}?");
        match self
            .client
            .send_prompt(chat_id, &question, "in 2h, 15:00, tomorrow 9:00")
        {
            Ok(prompt) => {
                if let Err(error) = self.state.set_reply_target(
                    PLATFORM,
                    chat_id,
                    prompt.message_id,
                    &entry.vault,
                    &ChatReplyTarget::Reminder { row },
                    now,
                ) {
                    self.status.last_error = Some(format!("reply state: {error}"));
                }
            }
            Err(error) => self.status.last_error = Some(error.to_string()),
        }
    }

    /// A text reply to one of the bot's messages: a time for a reminder, a
    /// "Later…" prompt, or a created task. `false` when the replied message
    /// is none of these, so the text is handled as usual.
    fn handle_reply(
        &mut self,
        user: &DaemonBotUser,
        chat_id: i64,
        replied_id: i64,
        text: &str,
    ) -> bool {
        let now = vulcan_core::current_utc_timestamp_ms();
        let reminder = match self
            .state
            .reminder_by_message(PLATFORM, chat_id, replied_id)
        {
            Ok(Some((_, entry))) => Some(entry),
            _ => match self.state.reply_target(PLATFORM, chat_id, replied_id, now) {
                Ok(Some((vault, ChatReplyTarget::Task { task }))) => {
                    let reply = self.user_vault(user, &vault).and_then(|wiki| {
                        self.with_vault(user, &wiki, |chat| {
                            run_chat_reschedule(chat, &task, text, now)
                        })
                    });
                    match reply {
                        Ok(reply) => self.send_reply(chat_id, Some(&vault), &reply),
                        Err(error) => self.send_reply(
                            chat_id,
                            None,
                            &ChatReply::text(format!("Error: {error}")),
                        ),
                    }
                    return true;
                }
                Ok(Some((_, ChatReplyTarget::Reminder { row }))) => self
                    .state
                    .reminder_by_row(PLATFORM, chat_id, row)
                    .ok()
                    .flatten()
                    .map(|(_, entry)| entry),
                _ => return false,
            },
        };
        let Some(entry) = reminder.filter(|entry| entry.state == ReminderState::Open) else {
            self.send_reply(
                chat_id,
                None,
                &ChatReply::text("That reminder is no longer open."),
            );
            return true;
        };
        let result = self.user_vault(user, &entry.vault).and_then(|wiki| {
            self.with_vault(user, &wiki, |vault| {
                run_reminder_snooze_text(vault, &entry.occurrence_id, text, now)
            })
        });
        match result {
            Ok(Some(outcome)) => {
                self.send_reply(chat_id, None, &ChatReply::text(outcome.clone()));
                self.resolve_reminder(chat_id, entry, &outcome, now);
            }
            Ok(None) => self.send_reply(chat_id, None, &ChatReply::text(CHAT_WHEN_HINT)),
            Err(error) => {
                self.send_reply(chat_id, None, &ChatReply::text(format!("Error: {error}")));
            }
        }
        true
    }

    /// An Undo button: reverts the capture it was sent with, only while that
    /// is still the chat's latest.
    fn press_undo(
        &mut self,
        user: &DaemonBotUser,
        callback: &TelegramCallbackQuery,
        chat_id: i64,
        token: &str,
        id: &str,
        now: i64,
    ) {
        let latest = self
            .state
            .undo(PLATFORM, chat_id, now)
            .ok()
            .flatten()
            .is_some_and(|(_, entries)| chat_undo_id(&entries) == id);
        let _ = self.state.remove_action(token);
        if !latest {
            let _ = self.client.answer_callback_query(
                &callback.id,
                Some("Only the latest capture can be undone."),
            );
            return;
        }
        let _ = self.client.answer_callback_query(&callback.id, None);
        let reply = self.undo(user, chat_id);
        self.send_reply(chat_id, None, &reply);
    }

    fn remember_undo(&mut self, chat_id: i64, vault: Option<&str>, reply: &ChatReply) {
        if let (Some(vault), false) = (vault, reply.undo.is_empty()) {
            let now = vulcan_core::current_utc_timestamp_ms();
            if let Err(error) = self
                .state
                .set_undo(PLATFORM, chat_id, vault, &reply.undo, now)
            {
                self.status.last_error = Some(format!("undo state: {error}"));
            }
        }
    }

    /// Revert this chat's last capture, in the vault it was made in.
    fn undo(&mut self, user: &DaemonBotUser, chat_id: i64) -> ChatReply {
        let now = vulcan_core::current_utc_timestamp_ms();
        let Some((vault, entries)) = self.state.undo(PLATFORM, chat_id, now).ok().flatten() else {
            return ChatReply::text("Nothing to undo.");
        };
        let result = self
            .user_vault(user, &vault)
            .and_then(|wiki| self.with_vault(user, &wiki, |chat| run_chat_undo(chat, &entries)));
        match result {
            Ok(reply) => {
                // Undone, or blocked by a later edit: either way it is spent.
                let _ = self.state.clear_undo(PLATFORM, chat_id);
                reply
            }
            Err(error) => ChatReply::text(format!("Error: {error}")),
        }
    }

    /// Download a sent file and save it into the active vault.
    fn save_attachment(
        &mut self,
        user: &DaemonBotUser,
        chat_id: i64,
        message: &TelegramMessage,
        file: &TelegramFileRef,
        file_name: String,
    ) -> (ChatReply, Option<String>) {
        let (wiki, vault_id) = match self.active_vault(user, chat_id) {
            Ok(found) => found,
            Err(error) => return (ChatReply::text(error), None),
        };
        if file
            .file_size
            .is_some_and(|size| size > TELEGRAM_FILE_LIMIT)
        {
            return (
                ChatReply::text("Files over 20 MB cannot be received by bots."),
                None,
            );
        }
        let bytes = match self
            .client
            .download_file(&file.file_id, TELEGRAM_FILE_LIMIT)
        {
            Ok(bytes) => bytes,
            Err(error) => return (ChatReply::text(format!("Error: {error}")), None),
        };
        let attachment = ChatAttachment {
            file_name,
            bytes,
            caption: message.caption.clone(),
        };
        match self.with_vault(user, &wiki, |vault| run_chat_attachment(vault, &attachment)) {
            Ok(reply) => (reply, Some(vault_id)),
            Err(error) => (ChatReply::text(format!("Error: {error}")), None),
        }
    }

    fn switch_vault(
        &mut self,
        user: &DaemonBotUser,
        chat_id: i64,
        name: Option<&str>,
    ) -> ChatReply {
        let current = self
            .active_vault(user, chat_id)
            .map(|(_, id)| id)
            .unwrap_or_default();
        let Some(name) = name else {
            let vaults = user
                .vaults
                .iter()
                .map(|vault| {
                    let marker = if vault.as_str() == current {
                        " (active)"
                    } else {
                        ""
                    };
                    format!("- {vault}{marker}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            return ChatReply::text(format!("Vaults:\n{vaults}\nSwitch with /vault <name>."));
        };
        match self.user_vault(user, name) {
            Ok(_) => match self.state.set_chat_vault(PLATFORM, chat_id, name) {
                Ok(()) => ChatReply::text(format!("Active vault: {name}")),
                Err(error) => ChatReply::text(format!("Error: {error}")),
            },
            Err(error) => ChatReply::text(error),
        }
    }

    /// The registration of `name` if the user may use it.
    fn user_vault(&self, user: &DaemonBotUser, name: &str) -> Result<WikiRegistration, String> {
        if !user.vaults.iter().any(|vault| vault.as_str() == name) {
            return Err(format!("Vault {name} is not available to you."));
        }
        self.registry
            .poll()
            .map_err(|error| error.to_string())?
            .vaults
            .into_iter()
            .find(|wiki| wiki.id.as_str() == name)
            .ok_or_else(|| format!("Vault {name} is not registered on this device."))
    }

    fn active_vault(
        &self,
        user: &DaemonBotUser,
        chat_id: i64,
    ) -> Result<(WikiRegistration, String), String> {
        let chosen = self
            .state
            .chat_vault(PLATFORM, chat_id)
            .ok()
            .flatten()
            .filter(|name| user.vaults.iter().any(|vault| vault.as_str() == name));
        let name = chosen
            .or_else(|| user.initial_vault().map(ToString::to_string))
            .ok_or_else(|| "No vault is configured for you.".to_string())?;
        self.user_vault(user, &name).map(|wiki| (wiki, name))
    }

    /// Run `work` against `wiki` with the user's authority.
    fn with_vault<T>(
        &self,
        user: &DaemonBotUser,
        wiki: &WikiRegistration,
        work: impl FnOnce(&ChatVault<'_>) -> Result<T, vulcan_app::AppError>,
    ) -> Result<T, String> {
        let paths = VaultPaths::new(&wiki.path);
        let guard = user_guard(&paths, wiki, user)?;
        let config = load_vault_config(&paths).config;
        let today = ReminderSettings::from_config(&config.tasks.reminders)
            .map_err(|error| error.to_string())?
            .timezone
            .date_at(vulcan_core::current_utc_timestamp_ms());
        let vault_gate = self.gate.map(|gate| gate.for_file_writes(wiki));
        let gate: &dyn MutationGate = match vault_gate.as_ref() {
            Some(gate) => gate,
            None => &vulcan_app::execution::Ungated,
        };
        let vault = ChatVault {
            paths: &paths,
            guard: &guard,
            profile: &user.profile,
            gate,
            today,
        };
        work(&vault).map_err(|error| sanitize_error(&error.to_string(), &wiki.path))
    }

    /// Rebuild plans when due, then send, expire, and resolve reminders.
    fn deliver_reminders(&mut self) {
        let now = vulcan_core::current_utc_timestamp_ms();
        if now >= self.next_plan_ms {
            self.plans = self.plan_reminders();
            self.next_plan_ms = now + REPLAN_MS;
        }
        let plans = std::mem::take(&mut self.plans);
        for recipient in &plans {
            let ledger = match self.state.reminder_entries(
                PLATFORM,
                recipient.chat_id,
                &recipient.vault,
                now,
            ) {
                Ok(ledger) => ledger,
                Err(error) => {
                    self.status.last_error = Some(format!("reminder ledger: {error}"));
                    continue;
                }
            };
            for step in reminder_steps(&recipient.plan, &ledger, now) {
                self.perform_reminder_step(recipient, step, now);
            }
        }
        self.plans = plans;
    }

    /// Send each user with a digest time their daily digest of every vault,
    /// once per local day, at or after that time.
    fn deliver_digests(&mut self) {
        let now = vulcan_core::current_utc_timestamp_ms();
        if now < self.next_digest_ms {
            return;
        }
        self.next_digest_ms = now + REPLAN_MS;
        let Some(users) = self
            .registry
            .poll()
            .ok()
            .and_then(|config| config.bot.telegram)
            .map(|telegram| telegram.users)
        else {
            return;
        };
        for user in &users {
            let Some(at) = user
                .digest
                .as_deref()
                .and_then(|at| chrono::NaiveTime::parse_from_str(at, "%H:%M").ok())
            else {
                continue;
            };
            for vault in &user.vaults {
                let Ok(wiki) = self.user_vault(user, vault.as_str()) else {
                    continue;
                };
                let paths = VaultPaths::new(&wiki.path);
                let Ok(settings) = ReminderSettings::from_config(
                    &load_vault_config(&paths).config.tasks.reminders,
                ) else {
                    continue;
                };
                let local = settings.timezone.local_datetime(now);
                let day = local.date().format("%Y-%m-%d").to_string();
                if local.time() < at
                    || self
                        .state
                        .digest_day(PLATFORM, user.telegram_id, vault.as_str())
                        .ok()
                        .flatten()
                        .as_deref()
                        == Some(day.as_str())
                {
                    continue;
                }
                match self.with_vault(user, &wiki, |chat| chat_digest(chat, vault.as_str(), now)) {
                    Ok(Some(reply)) => {
                        self.send_reply(user.telegram_id, Some(vault.as_str()), &reply);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        self.status.last_error = Some(format!("digest for {vault}: {error}"));
                    }
                }
                // Once a day either way: a failing vault must not resend
                // every minute.
                if let Err(error) =
                    self.state
                        .set_digest_day(PLATFORM, user.telegram_id, vault.as_str(), &day)
                {
                    self.status.last_error = Some(format!("digest state: {error}"));
                }
            }
        }
    }

    /// Plans for every allowlisted user with reminders on, per vault they
    /// may use. A vault that fails to plan is skipped, so its open messages
    /// are left alone rather than resolved.
    fn plan_reminders(&mut self) -> Vec<RecipientPlan> {
        let Some(users) = self
            .registry
            .poll()
            .ok()
            .and_then(|config| config.bot.telegram)
            .map(|telegram| telegram.users)
        else {
            return Vec::new();
        };
        let mut plans = Vec::new();
        for user in users.iter().filter(|user| user.reminders) {
            for vault in &user.vaults {
                let planned = self.user_vault(user, vault.as_str()).and_then(|wiki| {
                    self.with_vault(
                        user,
                        &wiki,
                        vulcan_app::chat::reminders::plan_chat_reminders,
                    )
                });
                match planned {
                    Ok(plan) => plans.push(RecipientPlan {
                        chat_id: user.telegram_id,
                        vault: vault.to_string(),
                        plan,
                    }),
                    Err(error) => {
                        self.status.last_error = Some(format!("reminders for {vault}: {error}"));
                    }
                }
            }
        }
        plans
    }

    fn perform_reminder_step(&mut self, recipient: &RecipientPlan, step: ReminderStep, now: i64) {
        let chat_id = recipient.chat_id;
        let entry =
            |reminder: &PlannedReminder, text: String, state: ReminderState| ReminderLedgerEntry {
                chat_id,
                vault: recipient.vault.clone(),
                occurrence_id: reminder.occurrence_id.clone(),
                task_key: reminder.task_key.clone(),
                task: reminder.task.clone(),
                path: reminder.path.clone(),
                text,
                notify_ms: reminder.notify_ms,
                state,
                message_id: None,
                updated_ms: now,
                actions: Vec::new(),
            };
        let result = match step {
            ReminderStep::Expire { reminder } => self
                .state
                .put_reminder(
                    PLATFORM,
                    &entry(&reminder, reminder.message(true), ReminderState::Expired),
                )
                .map(|_| ()),
            ReminderStep::Send { reminder, late } => {
                // Recorded first: a crash before the message id is stored
                // sends again rather than losing the reminder.
                let mut sent = entry(&reminder, reminder.message(late), ReminderState::Sending);
                sent.actions.clone_from(&reminder.actions);
                self.state.put_reminder(PLATFORM, &sent).and_then(|row| {
                    // The press names the ledger row and an offered action;
                    // everything else comes from the ledger.
                    let keyboard = vec![sent
                        .actions
                        .iter()
                        .enumerate()
                        .map(|(index, action)| InlineButton {
                            text: reminder_action_label(action),
                            callback_data: format!("{REMINDER_PREFIX}{row}:{index}"),
                        })
                        .collect::<Vec<_>>()];
                    let keyboard = if sent.actions.is_empty() {
                        Vec::new()
                    } else {
                        keyboard
                    };
                    match self.client.send_message(chat_id, &sent.text, &keyboard) {
                        Ok(message) => {
                            sent.state = ReminderState::Open;
                            sent.message_id = Some(message.message_id);
                            self.status.reminders_sent += 1;
                        }
                        // The user blocked the bot or never started a chat
                        // with it: retrying every minute would not help.
                        Err(TelegramError::Api { code: 403, .. }) => {
                            sent.state = ReminderState::Expired;
                            self.status.last_error =
                                Some(format!("user {chat_id} cannot receive messages"));
                        }
                        Err(error) => {
                            self.status.last_error = Some(error.to_string());
                            return Ok(());
                        }
                    }
                    self.state.put_reminder(PLATFORM, &sent).map(|_| ())
                })
            }
            ReminderStep::Resolve { mut entry, text } => {
                if let Some(message_id) = entry.message_id {
                    match self.client.edit_message_text(chat_id, message_id, &text) {
                        Ok(()) | Err(TelegramError::Api { .. }) => {}
                        // Try again on a later cycle.
                        Err(error) => {
                            self.status.last_error = Some(error.to_string());
                            return;
                        }
                    }
                }
                entry.state = ReminderState::Resolved;
                entry.text = text;
                entry.updated_ms = now;
                self.state.put_reminder(PLATFORM, &entry).map(|_| ())
            }
        };
        if let Err(error) = result {
            self.status.last_error = Some(format!("reminder ledger: {error}"));
        }
    }

    fn send_reply(&mut self, chat_id: i64, vault: Option<&str>, reply: &ChatReply) {
        let now = vulcan_core::current_utc_timestamp_ms();
        let undo = (!reply.undo.is_empty()).then(|| ChatButton {
            label: "Undo".to_string(),
            action: ChatAction::UndoCapture {
                id: chat_undo_id(&reply.undo),
            },
        });
        let keyboard = vault.map_or_else(Vec::new, |vault| {
            reply
                .buttons
                .iter()
                .chain(undo.as_ref())
                .filter_map(|button| {
                    let token = self
                        .state
                        .store_action(PLATFORM, chat_id, vault, &button.action, now)
                        .ok()?;
                    Some(vec![InlineButton {
                        text: button.label.clone(),
                        callback_data: format!("{CALLBACK_PREFIX}{token}"),
                    }])
                })
                .collect::<Vec<_>>()
        });
        if !reply.text.is_empty() || !keyboard.is_empty() {
            let text = if reply.text.is_empty() {
                "…"
            } else {
                &reply.text
            };
            match self.client.send_message(chat_id, text, &keyboard) {
                Ok(sent) => {
                    if let (Some(target), Some(vault)) = (&reply.reply_target, vault) {
                        if let Err(error) = self.state.set_reply_target(
                            PLATFORM,
                            chat_id,
                            sent.message_id,
                            vault,
                            target,
                            now,
                        ) {
                            self.status.last_error = Some(format!("reply state: {error}"));
                        }
                    }
                }
                Err(error) => self.status.last_error = Some(error.to_string()),
            }
        }
        if let Some(document) = &reply.document {
            if let Err(error) = self.client.send_document(
                chat_id,
                &document.file_name,
                document.content.clone().into_bytes(),
                None,
            ) {
                self.status.last_error = Some(error.to_string());
            }
        }
    }
}

/// The file a message carries, with the name to save it under: the
/// sender's for documents, otherwise one made from the kind and time.
fn attachment_of(message: &TelegramMessage) -> Option<(&TelegramFileRef, String)> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let named = |file: &TelegramFileRef, kind: &str, extension: &str| {
        file.file_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("{kind}-{stamp}.{extension}"))
    };
    if let Some(photo) = message.photo.as_ref().and_then(|sizes| sizes.last()) {
        return Some((photo, format!("photo-{stamp}.jpg")));
    }
    if let Some(file) = &message.document {
        return Some((file, named(file, "file", "bin")));
    }
    if let Some(file) = &message.voice {
        return Some((file, format!("voice-{stamp}.ogg")));
    }
    if let Some(file) = &message.audio {
        return Some((file, named(file, "audio", "mp3")));
    }
    message
        .video
        .as_ref()
        .map(|file| (file, named(file, "video", "mp4")))
}

/// The user's permission guard for `wiki`: their profile, resolved from
/// built-in and device-local profiles only, which must stay within the
/// registration's profile.
fn user_guard(
    paths: &VaultPaths,
    wiki: &WikiRegistration,
    user: &DaemonBotUser,
) -> Result<ProfilePermissionGuard, String> {
    let ceiling_name = wiki
        .permissions_profile
        .as_deref()
        .unwrap_or("unrestricted");
    let ceiling = vulcan_core::resolve_device_permission_profile(paths, Some(ceiling_name))
        .map_err(|error| error.to_string())?;
    let requested = vulcan_core::resolve_device_permission_profile(paths, Some(&user.profile))
        .map_err(|error| error.to_string())?;
    if !requested.grant.is_subset_of(&ceiling.grant) {
        return Err(format!(
            "your profile `{}` exceeds this device's limit `{ceiling_name}` for vault {}",
            user.profile, wiki.id
        ));
    }
    Ok(ProfilePermissionGuard::new(paths, requested))
}

/// An error message without the vault's absolute path.
fn sanitize_error(message: &str, vault_root: &Path) -> String {
    let root = vault_root.display().to_string();
    let message = message
        .replace(&format!("{root}/"), "")
        .replace(&root, "<vault>");
    message.chars().take(500).collect()
}

#[cfg(test)]
mod tests;
