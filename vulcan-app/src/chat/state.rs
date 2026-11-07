//! Device-local chat bot state: the platform's update offset, each chat's
//! active vault, the actions behind reply buttons, and the reminder ledger.
//!
//! It lives in the device state directory (`daemon/bot.sqlite`), outside
//! `cache.db` and outside sync. Losing it is harmless: the bot re-reads
//! pending updates once and old buttons stop working.

use super::{ChatAction, ChatUndo};
use crate::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

const STATE_FILE: &str = "daemon/bot.sqlite";
const STATE_VERSION: i64 = 4;
/// `/undo` reverts a capture for this long.
pub const UNDO_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
/// Expired and resolved reminder entries are forgotten after this.
pub const REMINDER_RETENTION_MS: i64 = 60 * 24 * 60 * 60 * 1_000;
/// Buttons older than this no longer work.
pub const ACTION_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// An action stored for a reply button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChatAction {
    pub chat_id: i64,
    pub vault: String,
    pub action: ChatAction,
}

pub struct ChatStateStore {
    connection: Connection,
}

impl ChatStateStore {
    #[must_use]
    pub fn path(state_root: &Path) -> PathBuf {
        state_root.join(STATE_FILE)
    }

    pub fn open(state_root: &Path) -> Result<Self, AppError> {
        let path = Self::path(state_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        let connection = Connection::open(&path).map_err(AppError::operation)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(10))
            .map_err(AppError::operation)?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(AppError::operation)?;
        if version > STATE_VERSION {
            return Err(AppError::operation(format!(
                "the chat bot state is version {version}; this Vulcan reads version {STATE_VERSION}"
            )));
        }
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS meta (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS chat_vaults (
                     platform TEXT NOT NULL,
                     chat_id INTEGER NOT NULL,
                     vault TEXT NOT NULL,
                     PRIMARY KEY (platform, chat_id)
                 );
                 CREATE TABLE IF NOT EXISTS actions (
                     token TEXT PRIMARY KEY,
                     platform TEXT NOT NULL,
                     chat_id INTEGER NOT NULL,
                     vault TEXT NOT NULL,
                     action TEXT NOT NULL,
                     created_ms INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS actions_by_age ON actions (created_ms);
                 CREATE TABLE IF NOT EXISTS reminders (
                     platform TEXT NOT NULL,
                     chat_id INTEGER NOT NULL,
                     vault TEXT NOT NULL,
                     occurrence_id TEXT NOT NULL,
                     task_key TEXT NOT NULL,
                     task TEXT NOT NULL,
                     path TEXT NOT NULL,
                     text TEXT NOT NULL,
                     notify_ms INTEGER NOT NULL,
                     state TEXT NOT NULL,
                     message_id INTEGER,
                     updated_ms INTEGER NOT NULL,
                     actions TEXT NOT NULL DEFAULT '',
                     PRIMARY KEY (platform, chat_id, vault, occurrence_id)
                 );
                 CREATE TABLE IF NOT EXISTS undo (
                     platform TEXT NOT NULL,
                     chat_id INTEGER NOT NULL,
                     vault TEXT NOT NULL,
                     entries TEXT NOT NULL,
                     created_ms INTEGER NOT NULL,
                     PRIMARY KEY (platform, chat_id)
                 );",
            )
            .map_err(AppError::operation)?;
        if version == 2 {
            connection
                .execute_batch("ALTER TABLE reminders ADD COLUMN actions TEXT NOT NULL DEFAULT '';")
                .map_err(AppError::operation)?;
        }
        connection
            .execute_batch(&format!("PRAGMA user_version = {STATE_VERSION};"))
            .map_err(AppError::operation)?;
        Ok(Self { connection })
    }

    /// The next update offset to request from `platform`.
    pub fn offset(&self, platform: &str) -> Result<Option<i64>, AppError> {
        self.connection
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![format!("{platform}.offset")],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(AppError::operation)
            .map(|value| value.and_then(|value| value.parse().ok()))
    }

    pub fn set_offset(&self, platform: &str, offset: i64) -> Result<(), AppError> {
        self.connection
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![format!("{platform}.offset"), offset.to_string()],
            )
            .map(|_| ())
            .map_err(AppError::operation)
    }

    /// The vault a chat switched to with `/vault`, if any.
    pub fn chat_vault(&self, platform: &str, chat_id: i64) -> Result<Option<String>, AppError> {
        self.connection
            .query_row(
                "SELECT vault FROM chat_vaults WHERE platform = ?1 AND chat_id = ?2",
                params![platform, chat_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(AppError::operation)
    }

    pub fn set_chat_vault(
        &self,
        platform: &str,
        chat_id: i64,
        vault: &str,
    ) -> Result<(), AppError> {
        self.connection
            .execute(
                "INSERT INTO chat_vaults (platform, chat_id, vault) VALUES (?1, ?2, ?3)
                 ON CONFLICT (platform, chat_id) DO UPDATE SET vault = excluded.vault",
                params![platform, chat_id, vault],
            )
            .map(|_| ())
            .map_err(AppError::operation)
    }

    /// Store a button's action and return its token: a short opaque id that
    /// fits any platform's callback payload. Expired actions are pruned.
    pub fn store_action(
        &self,
        platform: &str,
        chat_id: i64,
        vault: &str,
        action: &ChatAction,
        now_ms: i64,
    ) -> Result<String, AppError> {
        self.connection
            .execute(
                "DELETE FROM actions WHERE created_ms < ?1",
                params![now_ms - ACTION_RETENTION_MS],
            )
            .map_err(AppError::operation)?;
        let token = ulid::Ulid::new().to_string();
        let json = serde_json::to_string(action).map_err(AppError::operation)?;
        self.connection
            .execute(
                "INSERT INTO actions (token, platform, chat_id, vault, action, created_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![token, platform, chat_id, vault, json, now_ms],
            )
            .map_err(AppError::operation)?;
        Ok(token)
    }

    /// The action behind `token`, if it exists, has not expired, and belongs
    /// to `chat_id`. A token pressed in another chat resolves to nothing.
    pub fn action(
        &self,
        platform: &str,
        chat_id: i64,
        token: &str,
        now_ms: i64,
    ) -> Result<Option<StoredChatAction>, AppError> {
        let row = self
            .connection
            .query_row(
                "SELECT vault, action FROM actions
                 WHERE token = ?1 AND platform = ?2 AND chat_id = ?3 AND created_ms >= ?4",
                params![token, platform, chat_id, now_ms - ACTION_RETENTION_MS],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(AppError::operation)?;
        let Some((vault, json)) = row else {
            return Ok(None);
        };
        let action = serde_json::from_str(&json).map_err(AppError::operation)?;
        Ok(Some(StoredChatAction {
            chat_id,
            vault,
            action,
        }))
    }

    /// Forget an action once it ran, so a second press does nothing.
    pub fn remove_action(&self, token: &str) -> Result<(), AppError> {
        self.connection
            .execute("DELETE FROM actions WHERE token = ?1", params![token])
            .map(|_| ())
            .map_err(AppError::operation)
    }

    /// The reminder ledger of one chat and vault. Old expired and resolved
    /// entries are pruned first.
    pub fn reminder_entries(
        &self,
        platform: &str,
        chat_id: i64,
        vault: &str,
        now_ms: i64,
    ) -> Result<Vec<ReminderLedgerEntry>, AppError> {
        self.connection
            .execute(
                "DELETE FROM reminders WHERE state IN ('expired', 'resolved') AND updated_ms < ?1",
                params![now_ms - REMINDER_RETENTION_MS],
            )
            .map_err(AppError::operation)?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT occurrence_id, task_key, task, path, text, notify_ms, state, message_id,
                        updated_ms, actions, vault, rowid
                 FROM reminders WHERE platform = ?1 AND chat_id = ?2 AND vault = ?3
                 ORDER BY notify_ms",
            )
            .map_err(AppError::operation)?;
        let rows = statement
            .query_map(params![platform, chat_id, vault], |row| {
                ledger_row(chat_id, row)
            })
            .map_err(AppError::operation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AppError::operation)
    }

    /// The entry with ledger row `row_id`, if it belongs to `chat_id`.
    pub fn reminder_by_row(
        &self,
        platform: &str,
        chat_id: i64,
        row_id: i64,
    ) -> Result<Option<(i64, ReminderLedgerEntry)>, AppError> {
        self.connection
            .query_row(
                "SELECT occurrence_id, task_key, task, path, text, notify_ms, state, message_id,
                        updated_ms, actions, vault, rowid
                 FROM reminders WHERE platform = ?1 AND chat_id = ?2 AND rowid = ?3",
                params![platform, chat_id, row_id],
                |row| Ok((row.get(11)?, ledger_row(chat_id, row)?)),
            )
            .optional()
            .map_err(AppError::operation)
    }

    /// Insert or replace a ledger entry and return its row id, which stays
    /// the same across updates.
    pub fn put_reminder(
        &self,
        platform: &str,
        entry: &ReminderLedgerEntry,
    ) -> Result<i64, AppError> {
        self.connection
            .execute(
                "INSERT INTO reminders (platform, chat_id, vault, occurrence_id, task_key, task,
                                        path, text, notify_ms, state, message_id, updated_ms,
                                        actions)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT (platform, chat_id, vault, occurrence_id) DO UPDATE SET
                     task_key = excluded.task_key, task = excluded.task, path = excluded.path,
                     text = excluded.text, notify_ms = excluded.notify_ms,
                     state = excluded.state, message_id = excluded.message_id,
                     updated_ms = excluded.updated_ms, actions = excluded.actions",
                params![
                    platform,
                    entry.chat_id,
                    entry.vault,
                    entry.occurrence_id,
                    entry.task_key,
                    entry.task,
                    entry.path,
                    entry.text,
                    entry.notify_ms,
                    entry.state.as_str(),
                    entry.message_id,
                    entry.updated_ms,
                    entry.actions.join(","),
                ],
            )
            .map_err(AppError::operation)?;
        self.connection
            .query_row(
                "SELECT rowid FROM reminders
                 WHERE platform = ?1 AND chat_id = ?2 AND vault = ?3 AND occurrence_id = ?4",
                params![platform, entry.chat_id, entry.vault, entry.occurrence_id],
                |row| row.get(0),
            )
            .map_err(AppError::operation)
    }

    /// Remember a chat's last capture for `/undo`, replacing the previous.
    pub fn set_undo(
        &self,
        platform: &str,
        chat_id: i64,
        vault: &str,
        undo: &[ChatUndo],
        now_ms: i64,
    ) -> Result<(), AppError> {
        let entries = serde_json::to_string(undo).map_err(AppError::operation)?;
        self.connection
            .execute(
                "INSERT INTO undo (platform, chat_id, vault, entries, created_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (platform, chat_id) DO UPDATE SET
                     vault = excluded.vault, entries = excluded.entries,
                     created_ms = excluded.created_ms",
                params![platform, chat_id, vault, entries, now_ms],
            )
            .map(|_| ())
            .map_err(AppError::operation)
    }

    /// A chat's last capture, if it is recent enough to undo.
    pub fn undo(
        &self,
        platform: &str,
        chat_id: i64,
        now_ms: i64,
    ) -> Result<Option<(String, Vec<ChatUndo>)>, AppError> {
        let row = self
            .connection
            .query_row(
                "SELECT vault, entries FROM undo
                 WHERE platform = ?1 AND chat_id = ?2 AND created_ms >= ?3",
                params![platform, chat_id, now_ms - UNDO_RETENTION_MS],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(AppError::operation)?;
        row.map(|(vault, entries)| {
            serde_json::from_str(&entries)
                .map(|entries| (vault, entries))
                .map_err(AppError::operation)
        })
        .transpose()
    }

    pub fn clear_undo(&self, platform: &str, chat_id: i64) -> Result<(), AppError> {
        self.connection
            .execute(
                "DELETE FROM undo WHERE platform = ?1 AND chat_id = ?2",
                params![platform, chat_id],
            )
            .map(|_| ())
            .map_err(AppError::operation)
    }

    /// Counts of ledger entries by state, for status output.
    pub fn reminder_counts(&self, platform: &str) -> Result<Vec<(String, u64)>, AppError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT state, COUNT(*) FROM reminders WHERE platform = ?1
                 GROUP BY state ORDER BY state",
            )
            .map_err(AppError::operation)?;
        let rows = statement
            .query_map(params![platform], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(AppError::operation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AppError::operation)
    }
}

fn ledger_row(chat_id: i64, row: &rusqlite::Row<'_>) -> rusqlite::Result<ReminderLedgerEntry> {
    let actions: String = row.get(9)?;
    Ok(ReminderLedgerEntry {
        chat_id,
        vault: row.get(10)?,
        occurrence_id: row.get(0)?,
        task_key: row.get(1)?,
        task: row.get(2)?,
        path: row.get(3)?,
        text: row.get(4)?,
        notify_ms: row.get(5)?,
        state: ReminderState::parse(&row.get::<_, String>(6)?),
        message_id: row.get(7)?,
        updated_ms: row.get(8)?,
        actions: actions
            .split(',')
            .filter(|action| !action.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    })
}

/// Where one reminder delivery stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderState {
    /// Recorded before sending; no message id yet. Sent again after a crash
    /// or a failed send, since a duplicate beats a lost reminder.
    Sending,
    /// Delivered and still open.
    Open,
    /// Missed by more than the catch-up window; never sent.
    Expired,
    /// The task changed elsewhere and the message was updated.
    Resolved,
}

impl ReminderState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sending => "sending",
            Self::Open => "open",
            Self::Expired => "expired",
            Self::Resolved => "resolved",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "sending" => Self::Sending,
            "open" => Self::Open,
            "expired" => Self::Expired,
            _ => Self::Resolved,
        }
    }
}

/// One reminder occurrence delivered (or skipped) to one chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderLedgerEntry {
    pub chat_id: i64,
    pub vault: String,
    pub occurrence_id: String,
    pub task_key: String,
    /// The task reference task commands accept (`path` or `path:line`).
    pub task: String,
    pub path: String,
    /// The message text as sent.
    pub text: String,
    pub notify_ms: i64,
    pub state: ReminderState,
    pub message_id: Option<i64>,
    pub updated_ms: i64,
    /// The button actions offered with the message (`done`, `1h`,
    /// `tomorrow`, …); a press names one by index.
    pub actions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_offsets_vaults_and_chat_scoped_actions() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChatStateStore::open(temp.path()).unwrap();
        assert_eq!(store.offset("telegram").unwrap(), None);
        store.set_offset("telegram", 42).unwrap();
        store.set_offset("telegram", 43).unwrap();
        assert_eq!(store.offset("telegram").unwrap(), Some(43));

        assert_eq!(store.chat_vault("telegram", 7).unwrap(), None);
        store.set_chat_vault("telegram", 7, "notes").unwrap();
        store.set_chat_vault("telegram", 7, "rpg").unwrap();
        assert_eq!(
            store.chat_vault("telegram", 7).unwrap().as_deref(),
            Some("rpg")
        );

        let action = ChatAction::ShowNote {
            path: "A.md".to_string(),
        };
        let token = store
            .store_action("telegram", 7, "notes", &action, 1_000)
            .unwrap();
        assert!(token.len() <= 32);
        let stored = store.action("telegram", 7, &token, 2_000).unwrap().unwrap();
        assert_eq!(stored.vault, "notes");
        assert_eq!(stored.action, action);
        assert_eq!(
            store.action("telegram", 8, &token, 2_000).unwrap(),
            None,
            "another chat cannot use the token"
        );
        assert_eq!(
            store
                .action("telegram", 7, &token, 1_000 + ACTION_RETENTION_MS + 1)
                .unwrap(),
            None,
            "expired"
        );
        store.remove_action(&token).unwrap();
        assert_eq!(store.action("telegram", 7, &token, 2_000).unwrap(), None);

        // State survives reopening.
        drop(store);
        let store = ChatStateStore::open(temp.path()).unwrap();
        assert_eq!(store.offset("telegram").unwrap(), Some(43));
    }

    #[test]
    fn reminder_ledger_round_trips_and_prunes_finished_entries() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChatStateStore::open(temp.path()).unwrap();
        let mut entry = ReminderLedgerEntry {
            chat_id: 7,
            vault: "notes".to_string(),
            occurrence_id: "occ".to_string(),
            task_key: "id:1".to_string(),
            task: "A.md:1".to_string(),
            path: "A.md".to_string(),
            text: "⏰ Call".to_string(),
            notify_ms: 1_000,
            state: ReminderState::Sending,
            message_id: None,
            updated_ms: 2_000,
            actions: vec!["done".to_string(), "1h".to_string()],
        };
        let row = store.put_reminder("telegram", &entry).unwrap();
        entry.state = ReminderState::Open;
        entry.message_id = Some(55);
        assert_eq!(
            store.put_reminder("telegram", &entry).unwrap(),
            row,
            "stable row id"
        );
        assert_eq!(
            store.reminder_by_row("telegram", 7, row).unwrap(),
            Some((row, entry.clone()))
        );
        assert_eq!(
            store.reminder_by_row("telegram", 8, row).unwrap(),
            None,
            "other chat"
        );
        assert_eq!(
            store
                .reminder_entries("telegram", 7, "notes", 3_000)
                .unwrap(),
            vec![entry.clone()]
        );
        assert!(store
            .reminder_entries("telegram", 8, "notes", 3_000)
            .unwrap()
            .is_empty());
        assert_eq!(
            store.reminder_counts("telegram").unwrap(),
            vec![("open".to_string(), 1)]
        );

        // Open entries are kept however old; finished ones are pruned.
        let later = 2_000 + REMINDER_RETENTION_MS + 1;
        assert_eq!(
            store
                .reminder_entries("telegram", 7, "notes", later)
                .unwrap()
                .len(),
            1
        );
        entry.state = ReminderState::Resolved;
        store.put_reminder("telegram", &entry).unwrap();
        assert!(store
            .reminder_entries("telegram", 7, "notes", later)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn version_two_state_gains_the_actions_column() {
        let temp = tempfile::tempdir().unwrap();
        drop(ChatStateStore::open(temp.path()).unwrap());
        let raw = Connection::open(ChatStateStore::path(temp.path())).unwrap();
        raw.execute_batch("ALTER TABLE reminders DROP COLUMN actions; PRAGMA user_version = 2;")
            .unwrap();
        drop(raw);
        let store = ChatStateStore::open(temp.path()).unwrap();
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, STATE_VERSION);
        assert!(store
            .reminder_entries("telegram", 1, "notes", 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn keeps_one_recent_undo_per_chat() {
        let temp = tempfile::tempdir().unwrap();
        let store = ChatStateStore::open(temp.path()).unwrap();
        let entry = |path: &str| ChatUndo {
            path: path.to_string(),
            before: None,
            after_hash: "h".to_string(),
        };
        assert_eq!(store.undo("telegram", 7, 0).unwrap(), None);
        store
            .set_undo("telegram", 7, "notes", &[entry("A.md")], 1_000)
            .unwrap();
        store
            .set_undo("telegram", 7, "rpg", &[entry("B.md")], 2_000)
            .unwrap();
        assert_eq!(
            store.undo("telegram", 7, 3_000).unwrap(),
            Some(("rpg".to_string(), vec![entry("B.md")]))
        );
        assert_eq!(store.undo("telegram", 8, 3_000).unwrap(), None);
        assert_eq!(
            store
                .undo("telegram", 7, 2_000 + UNDO_RETENTION_MS + 1)
                .unwrap(),
            None,
            "too old"
        );
        store.clear_undo("telegram", 7).unwrap();
        assert_eq!(store.undo("telegram", 7, 3_000).unwrap(), None);
    }
}
