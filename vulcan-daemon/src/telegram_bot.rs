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
    InlineButton, TelegramCallbackQuery, TelegramClient, TelegramError, TelegramMessage,
    TelegramUpdate,
};
use crate::worker_gate::HostedWorkerGate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use vulcan_app::chat::state::ChatStateStore;
use vulcan_app::chat::{
    chat_help_text, chat_usage_text, parse_chat_message, run_chat_action, run_chat_command,
    ChatCommand, ChatReply, ChatVault, CHAT_COMMANDS,
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
const MAX_REJECTED_USERS: usize = 32;

/// What `vulcan daemon bot-status` shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramBotStatus {
    /// `starting`, `polling`, `conflict`, `backoff`, `rate_limited`, `stopped`, or `failed`.
    pub state: String,
    pub bot_username: Option<String>,
    pub last_poll_ms: Option<i64>,
    pub last_error: Option<String>,
    pub handled_updates: u64,
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
        let text = message
            .text
            .as_deref()
            .or(message.caption.as_deref())
            .unwrap_or_default();
        // Buttons are stored against the vault the reply came from.
        let (reply, vault) = match parse_chat_message(text) {
            ChatCommand::Empty => (
                ChatReply::text("Only text messages are supported so far."),
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

    fn send_reply(&mut self, chat_id: i64, vault: Option<&str>, reply: &ChatReply) {
        let now = vulcan_core::current_utc_timestamp_ms();
        let keyboard = vault.map_or_else(Vec::new, |vault| {
            reply
                .buttons
                .iter()
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
            if let Err(error) = self.client.send_message(chat_id, text, &keyboard) {
                self.status.last_error = Some(error.to_string());
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
