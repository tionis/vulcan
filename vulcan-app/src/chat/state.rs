//! Device-local chat bot state: the platform's update offset, each chat's
//! active vault, and the actions behind reply buttons.
//!
//! It lives in the device state directory (`daemon/bot.sqlite`), outside
//! `cache.db` and outside sync. Losing it is harmless: the bot re-reads
//! pending updates once and old buttons stop working.

use super::ChatAction;
use crate::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

const STATE_FILE: &str = "daemon/bot.sqlite";
const STATE_VERSION: i64 = 1;
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
                 PRAGMA user_version = 1;",
            )
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
}
