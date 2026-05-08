use crate::assistant::rpc_events::PiEvent;
use crate::CliError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::{BufRead, Write};
use ulid::Ulid;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RpcCommand {
    #[serde(rename = "type")]
    pub(crate) kind: &'static str,
    pub(crate) id: String,
    pub(crate) command: String,
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub(crate) data: Map<String, Value>,
}

impl RpcCommand {
    pub(crate) fn new(command: impl Into<String>, data: Map<String, Value>) -> Self {
        Self {
            kind: "command",
            id: Ulid::new().to_string(),
            command: command.into(),
            data,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct RpcResponse {
    pub(crate) id: String,
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) success: bool,
    #[serde(default)]
    pub(crate) data: Value,
    #[serde(default)]
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub(crate) struct PiSessionState {
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) thinking_level: Option<String>,
    #[serde(default)]
    pub(crate) is_streaming: bool,
    #[serde(default)]
    pub(crate) is_compacting: bool,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) session_name: Option<String>,
    #[serde(default)]
    pub(crate) message_count: Option<u64>,
    #[serde(default)]
    pub(crate) pending_message_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub(crate) struct CompactionResult {
    #[serde(default)]
    pub(crate) summary: Option<String>,
    #[serde(default)]
    pub(crate) first_kept_entry_id: Option<String>,
    #[serde(default)]
    pub(crate) tokens_before: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub(crate) struct PiModel {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    pub(crate) reasoning: Option<bool>,
    #[serde(default)]
    pub(crate) context_window: Option<u64>,
    #[serde(default)]
    pub(crate) max_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AssistantEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolExecutionStart {
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolExecutionEnd {
        name: String,
        #[serde(default)]
        output: Value,
        #[serde(default)]
        error: Option<String>,
    },
    AgentEnd {
        #[serde(default)]
        data: Value,
    },
    Error {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RpcMessage {
    Response(RpcResponse),
    Event(AssistantEvent),
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RpcCommandResult {
    pub(crate) response: RpcResponse,
    pub(crate) events: Vec<AssistantEvent>,
}

pub(crate) struct ManagedRpcClient<R, W> {
    reader: R,
    writer: W,
}

impl<R: BufRead, W: Write> ManagedRpcClient<R, W> {
    pub(crate) fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    pub(crate) fn command(
        &mut self,
        command: impl Into<String>,
        data: Map<String, Value>,
    ) -> Result<RpcCommandResult, CliError> {
        let command = RpcCommand::new(command, data);
        let id = command.id.clone();
        self.send(&command)?;
        self.read_until_response(&id)
    }

    pub(crate) fn prompt(&mut self, message: &str) -> Result<RpcCommandResult, CliError> {
        self.command(
            "prompt",
            map_with("prompt", Value::String(message.to_string())),
        )
    }

    pub(crate) fn steer(&mut self, message: &str) -> Result<RpcCommandResult, CliError> {
        self.command(
            "steer",
            map_with("message", Value::String(message.to_string())),
        )
    }

    pub(crate) fn follow_up(&mut self, message: &str) -> Result<RpcCommandResult, CliError> {
        self.command(
            "follow_up",
            map_with("message", Value::String(message.to_string())),
        )
    }

    pub(crate) fn abort(&mut self) -> Result<RpcCommandResult, CliError> {
        self.command("abort", Map::new())
    }

    pub(crate) fn get_state(&mut self) -> Result<PiSessionState, CliError> {
        let result = self.command("get_state", Map::new())?;
        if !result.response.success {
            return Err(CliError::operation(
                result
                    .response
                    .error
                    .unwrap_or_else(|| "get_state failed".to_string()),
            ));
        }
        serde_json::from_value(result.response.data).map_err(CliError::operation)
    }

    pub(crate) fn set_model(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<RpcCommandResult, CliError> {
        self.command(
            "set_model",
            map_from_pairs([
                ("provider", Value::String(provider.to_string())),
                ("model_id", Value::String(model_id.to_string())),
            ]),
        )
    }

    pub(crate) fn get_available_models(&mut self) -> Result<Vec<PiModel>, CliError> {
        let result = self.command("get_available_models", Map::new())?;
        if !result.response.success {
            return Err(CliError::operation(
                result
                    .response
                    .error
                    .unwrap_or_else(|| "get_available_models failed".to_string()),
            ));
        }
        serde_json::from_value(result.response.data).map_err(CliError::operation)
    }

    pub(crate) fn compact(&mut self) -> Result<CompactionResult, CliError> {
        let result = self.command("compact", Map::new())?;
        if !result.response.success {
            return Err(CliError::operation(
                result
                    .response
                    .error
                    .unwrap_or_else(|| "compact failed".to_string()),
            ));
        }
        serde_json::from_value(result.response.data).map_err(CliError::operation)
    }

    pub(crate) fn new_session(&mut self) -> Result<bool, CliError> {
        let result = self.command("new_session", Map::new())?;
        if !result.response.success {
            return Err(CliError::operation(
                result
                    .response
                    .error
                    .unwrap_or_else(|| "new_session failed".to_string()),
            ));
        }
        Ok(result
            .response
            .data
            .get("cancelled")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub(crate) fn send(&mut self, command: &RpcCommand) -> Result<(), CliError> {
        serde_json::to_writer(&mut self.writer, command).map_err(CliError::operation)?;
        self.writer.write_all(b"\n").map_err(CliError::operation)?;
        self.writer.flush().map_err(CliError::operation)
    }

    fn read_until_response(&mut self, id: &str) -> Result<RpcCommandResult, CliError> {
        let mut events = Vec::new();
        loop {
            let Some(message) = read_message(&mut self.reader)? else {
                return Err(CliError::operation(format!(
                    "managed assistant engine exited before response {id}"
                )));
            };
            match message {
                RpcMessage::Response(response) if response.id == id => {
                    return Ok(RpcCommandResult { response, events });
                }
                RpcMessage::Response(_) | RpcMessage::Unknown(_) => {}
                RpcMessage::Event(event) => events.push(event),
            }
        }
    }
}

fn map_with(key: &str, value: Value) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(key.to_string(), value);
    map
}

fn map_from_pairs<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

pub(crate) fn read_message(reader: &mut impl BufRead) -> Result<Option<RpcMessage>, CliError> {
    let mut line = Vec::new();
    let bytes = reader
        .read_until(b'\n', &mut line)
        .map_err(CliError::operation)?;
    if bytes == 0 {
        return Ok(None);
    }
    while line.ends_with(b"\n") || line.ends_with(b"\r") {
        line.pop();
    }
    if line.is_empty() {
        return read_message(reader);
    }
    let value = serde_json::from_slice::<Value>(&line).map_err(CliError::operation)?;
    Ok(Some(parse_message(value)))
}

fn parse_message(value: Value) -> RpcMessage {
    if value.get("id").is_some()
        && (value.get("success").is_some()
            || value
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "response"))
    {
        let fallback = value.clone();
        return serde_json::from_value::<RpcResponse>(value)
            .map(RpcMessage::Response)
            .unwrap_or(RpcMessage::Unknown(fallback));
    }
    if let Ok(pi_event) = serde_json::from_value::<PiEvent>(value.clone()) {
        if let Some(event) = pi_event.assistant_event() {
            return RpcMessage::Event(event);
        }
    }
    if let Some(event) = value.get("event").cloned() {
        let fallback = event.clone();
        return serde_json::from_value::<AssistantEvent>(event)
            .map(RpcMessage::Event)
            .unwrap_or(RpcMessage::Unknown(fallback));
    }
    serde_json::from_value::<AssistantEvent>(value.clone())
        .map(RpcMessage::Event)
        .unwrap_or(RpcMessage::Unknown(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn read_message_splits_only_on_line_feed() {
        let raw = br#"{"type":"text_delta","text":"alpha\u2028beta"}
{"type":"response","id":"01","command":"prompt","success":true,"data":{"ok":true}}
"#;
        let mut reader = BufReader::new(Cursor::new(raw));

        assert_eq!(
            read_message(&mut reader).expect("message should parse"),
            Some(RpcMessage::Event(AssistantEvent::TextDelta {
                text: "alpha\u{2028}beta".to_string()
            }))
        );
        assert!(matches!(
            read_message(&mut reader).expect("response should parse"),
            Some(RpcMessage::Response(response)) if response.id == "01"
        ));
    }

    #[test]
    fn client_collects_events_until_matching_response() {
        let input = br#"{"type":"text_delta","text":"hello"}
{"type":"response","id":"01KTEST","command":"prompt","success":true,"data":{"tokens":4}}
"#;
        let reader = BufReader::new(Cursor::new(input));
        let writer = Vec::new();
        let mut client = ManagedRpcClient::new(reader, writer);
        let command = RpcCommand {
            kind: "command",
            id: "01KTEST".to_string(),
            command: "prompt".to_string(),
            data: Map::new(),
        };

        client.send(&command).expect("command should send");
        let result = client
            .read_until_response("01KTEST")
            .expect("response should be read");

        assert_eq!(
            result.events,
            vec![AssistantEvent::TextDelta {
                text: "hello".to_string()
            }]
        );
        assert!(result.response.success);
    }

    #[test]
    fn parses_pi_message_update_events() {
        let raw = br#"{"type":"message_update","assistant_event":{"type":"text_delta","text":"hi"}}
"#;
        let mut reader = BufReader::new(Cursor::new(raw));

        assert_eq!(
            read_message(&mut reader).expect("message should parse"),
            Some(RpcMessage::Event(AssistantEvent::TextDelta {
                text: "hi".to_string()
            }))
        );
    }

    #[test]
    fn typed_payloads_parse_state_model_and_compaction() {
        let state: PiSessionState = serde_json::from_value(serde_json::json!({
            "model": "gpt",
            "thinking_level": "high",
            "is_streaming": false,
            "message_count": 3
        }))
        .expect("state should parse");
        assert_eq!(state.model.as_deref(), Some("gpt"));
        assert_eq!(state.message_count, Some(3));

        let compaction: CompactionResult = serde_json::from_value(serde_json::json!({
            "summary": "short",
            "tokens_before": 100
        }))
        .expect("compaction should parse");
        assert_eq!(compaction.summary.as_deref(), Some("short"));
        assert_eq!(compaction.tokens_before, Some(100));

        let model: PiModel = serde_json::from_value(serde_json::json!({
            "id": "gpt",
            "provider": "openai",
            "reasoning": true,
            "context_window": 128_000
        }))
        .expect("model should parse");
        assert_eq!(model.id, "gpt");
        assert_eq!(model.provider.as_deref(), Some("openai"));
    }
}
