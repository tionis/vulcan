//! A minimal Telegram Bot API client: the methods the chat bot uses, and
//! nothing else.
//!
//! The token is part of every request URL, so errors never carry a URL and
//! the client's `Debug` output omits the token.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt::{Display, Formatter};
use std::time::Duration;

pub const DEFAULT_TELEGRAM_API_BASE: &str = "https://api.telegram.org";
/// Telegram rejects longer message texts.
pub const TELEGRAM_TEXT_LIMIT: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramError {
    /// The token was revoked or is wrong (HTTP 401).
    Unauthorized,
    /// Another process polls this bot (HTTP 409).
    Conflict,
    /// Rate limited: retry after this many seconds (HTTP 429).
    RetryAfter(u64),
    Api {
        code: u16,
        description: String,
    },
    Network(String),
}

impl Display for TelegramError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("the Telegram bot token was rejected"),
            Self::Conflict => formatter.write_str(
                "another process is polling this Telegram bot; only one device may run it",
            ),
            Self::RetryAfter(seconds) => {
                write!(formatter, "rate limited by Telegram for {seconds} s")
            }
            Self::Api { code, description } => {
                write!(formatter, "Telegram API error {code}: {description}")
            }
            Self::Network(detail) => write!(formatter, "Telegram request failed: {detail}"),
        }
    }
}

impl std::error::Error for TelegramError {}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TelegramChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: TelegramChat,
    #[serde(default)]
    pub from: Option<TelegramUser>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TelegramCallbackQuery {
    pub id: String,
    pub from: TelegramUser,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
    #[serde(default)]
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TelegramUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
    #[serde(default)]
    pub callback_query: Option<TelegramCallbackQuery>,
}

/// One inline keyboard button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InlineButton {
    pub text: String,
    pub callback_data: String,
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error_code: Option<u16>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<EnvelopeParameters>,
}

#[derive(Deserialize)]
struct EnvelopeParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

pub struct TelegramClient {
    http: reqwest::blocking::Client,
    base: String,
    token: String,
}

impl std::fmt::Debug for TelegramClient {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramClient")
            .field("base", &self.base)
            .field("token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl TelegramClient {
    pub fn new(base: Option<&str>, token: String) -> Result<Self, TelegramError> {
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|error| TelegramError::Network(error.without_url().to_string()))?;
        Ok(Self {
            http,
            base: base
                .unwrap_or(DEFAULT_TELEGRAM_API_BASE)
                .trim_end_matches('/')
                .to_string(),
            token,
        })
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token)
    }

    fn decode<T: DeserializeOwned>(
        response: reqwest::blocking::Response,
    ) -> Result<T, TelegramError> {
        let status = response.status().as_u16();
        let envelope: Envelope = response
            .json()
            .map_err(|error| TelegramError::Network(error.without_url().to_string()))?;
        if envelope.ok {
            return serde_json::from_value(envelope.result.unwrap_or(Value::Null))
                .map_err(|error| TelegramError::Network(format!("unexpected response: {error}")));
        }
        let code = envelope.error_code.unwrap_or(status);
        Err(match code {
            401 => TelegramError::Unauthorized,
            409 => TelegramError::Conflict,
            429 => TelegramError::RetryAfter(
                envelope
                    .parameters
                    .and_then(|parameters| parameters.retry_after)
                    .unwrap_or(5),
            ),
            _ => TelegramError::Api {
                code,
                description: envelope.description.unwrap_or_default(),
            },
        })
    }

    fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<T, TelegramError> {
        let response = self
            .http
            .post(self.url(method))
            .timeout(timeout)
            .json(body)
            .send()
            .map_err(|error| TelegramError::Network(error.without_url().to_string()))?;
        Self::decode(response)
    }

    pub fn get_me(&self) -> Result<TelegramUser, TelegramError> {
        self.call("getMe", &json!({}), REQUEST_TIMEOUT)
    }

    /// Long-poll for updates for up to `timeout_seconds`.
    pub fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_seconds: u64,
    ) -> Result<Vec<TelegramUpdate>, TelegramError> {
        let mut body = json!({
            "timeout": timeout_seconds,
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(offset) = offset {
            body["offset"] = json!(offset);
        }
        self.call(
            "getUpdates",
            &body,
            Duration::from_secs(timeout_seconds) + REQUEST_TIMEOUT,
        )
    }

    pub fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        keyboard: &[Vec<InlineButton>],
    ) -> Result<TelegramMessage, TelegramError> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": clip_text(text),
            "link_preview_options": { "is_disabled": true },
        });
        if !keyboard.is_empty() {
            body["reply_markup"] = json!({ "inline_keyboard": keyboard });
        }
        self.call("sendMessage", &body, REQUEST_TIMEOUT)
    }

    /// Replace a message's text and drop its buttons.
    pub fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
    ) -> Result<(), TelegramError> {
        self.call::<Value>(
            "editMessageText",
            &json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "text": clip_text(text),
                "reply_markup": { "inline_keyboard": [] },
            }),
            REQUEST_TIMEOUT,
        )
        .map(|_| ())
    }

    pub fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<(), TelegramError> {
        let mut body = json!({ "callback_query_id": callback_query_id });
        if let Some(text) = text {
            body["text"] = json!(text.chars().take(190).collect::<String>());
        }
        self.call::<Value>("answerCallbackQuery", &body, REQUEST_TIMEOUT)
            .map(|_| ())
    }

    pub fn send_document(
        &self,
        chat_id: i64,
        file_name: &str,
        content: Vec<u8>,
        caption: Option<&str>,
    ) -> Result<(), TelegramError> {
        let part = reqwest::blocking::multipart::Part::bytes(content)
            .file_name(file_name.to_string())
            .mime_str("text/markdown")
            .map_err(|error| TelegramError::Network(error.without_url().to_string()))?;
        let mut form = reqwest::blocking::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);
        if let Some(caption) = caption {
            form = form.text("caption", caption.chars().take(1000).collect::<String>());
        }
        let response = self
            .http
            .post(self.url("sendDocument"))
            .timeout(REQUEST_TIMEOUT)
            .multipart(form)
            .send()
            .map_err(|error| TelegramError::Network(error.without_url().to_string()))?;
        Self::decode::<Value>(response).map(|_| ())
    }

    pub fn set_my_commands(&self, commands: &[(&str, &str)]) -> Result<(), TelegramError> {
        let commands = commands
            .iter()
            .map(|(command, description)| json!({ "command": command, "description": description }))
            .collect::<Vec<_>>();
        self.call::<Value>(
            "setMyCommands",
            &json!({ "commands": commands }),
            REQUEST_TIMEOUT,
        )
        .map(|_| ())
    }
}

/// `text` cut to Telegram's limit, on a character boundary.
#[must_use]
pub fn clip_text(text: &str) -> String {
    if text.chars().count() <= TELEGRAM_TEXT_LIMIT {
        return text.to_string();
    }
    let mut clipped = text
        .chars()
        .take(TELEGRAM_TEXT_LIMIT - 1)
        .collect::<String>();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_redacts_the_token() {
        let client = TelegramClient::new(None, "123:secret".to_string()).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(debug.contains(DEFAULT_TELEGRAM_API_BASE));
    }

    #[test]
    fn clips_long_text_on_character_boundaries() {
        let text = "ä".repeat(TELEGRAM_TEXT_LIMIT + 10);
        let clipped = clip_text(&text);
        assert_eq!(clipped.chars().count(), TELEGRAM_TEXT_LIMIT);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip_text("short"), "short");
    }
}
