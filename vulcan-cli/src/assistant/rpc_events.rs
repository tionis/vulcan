use crate::assistant::rpc::AssistantEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PiEvent {
    AgentStart {
        #[serde(default)]
        data: Value,
    },
    AgentEnd {
        #[serde(default)]
        messages: Value,
    },
    TurnStart {
        #[serde(default)]
        data: Value,
    },
    TurnEnd {
        #[serde(default)]
        message: Value,
        #[serde(default)]
        tool_results: Value,
    },
    MessageStart {
        #[serde(default)]
        data: Value,
    },
    MessageUpdate {
        #[serde(default)]
        assistant_event: Option<PiAssistantEvent>,
        #[serde(default)]
        event: Option<PiAssistantEvent>,
    },
    MessageEnd {
        #[serde(default)]
        data: Value,
    },
    ToolExecutionStart {
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolExecutionUpdate {
        name: String,
        #[serde(default)]
        output: Value,
    },
    ToolExecutionEnd {
        name: String,
        #[serde(default)]
        output: Value,
        #[serde(default)]
        error: Option<String>,
    },
    QueueUpdate {
        #[serde(default)]
        steering: Value,
        #[serde(default)]
        follow_up: Value,
    },
    CompactionStart {
        #[serde(default)]
        data: Value,
    },
    CompactionEnd {
        #[serde(default)]
        data: Value,
    },
    AutoRetryStart {
        #[serde(default)]
        data: Value,
    },
    AutoRetryEnd {
        #[serde(default)]
        data: Value,
    },
    ExtensionError {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        error: Option<String>,
    },
    ExtensionUiRequest {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        data: Value,
    },
    #[serde(other)]
    Unknown,
}

impl PiEvent {
    pub(crate) fn assistant_event(&self) -> Option<AssistantEvent> {
        match self {
            Self::AgentEnd { messages } => Some(AssistantEvent::AgentEnd {
                data: messages.clone(),
            }),
            Self::MessageUpdate {
                assistant_event,
                event,
            } => assistant_event
                .as_ref()
                .or(event.as_ref())
                .map(PiAssistantEvent::to_assistant_event),
            Self::ToolExecutionStart { name, input } => Some(AssistantEvent::ToolExecutionStart {
                name: name.clone(),
                input: input.clone(),
            }),
            Self::ToolExecutionEnd {
                name,
                output,
                error,
            } => Some(AssistantEvent::ToolExecutionEnd {
                name: name.clone(),
                output: output.clone(),
                error: error.clone(),
            }),
            Self::ExtensionError { message, error } => Some(AssistantEvent::Error {
                message: message
                    .clone()
                    .or_else(|| error.clone())
                    .unwrap_or_else(|| "assistant extension error".to_string()),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PiAssistantEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolCallStart {
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolCallDelta {
        name: String,
        #[serde(default)]
        delta: Value,
    },
    ToolCallEnd {
        name: String,
        #[serde(default)]
        output: Value,
        #[serde(default)]
        error: Option<String>,
    },
    Done,
    Error {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

impl PiAssistantEvent {
    pub(crate) fn to_assistant_event(&self) -> AssistantEvent {
        match self {
            Self::TextDelta { text } => AssistantEvent::TextDelta { text: text.clone() },
            Self::ThinkingDelta { text } => AssistantEvent::ThinkingDelta { text: text.clone() },
            Self::ToolCallStart { name, input } => AssistantEvent::ToolExecutionStart {
                name: name.clone(),
                input: input.clone(),
            },
            Self::ToolCallEnd {
                name,
                output,
                error,
            } => AssistantEvent::ToolExecutionEnd {
                name: name.clone(),
                output: output.clone(),
                error: error.clone(),
            },
            Self::Error { message } => AssistantEvent::Error {
                message: message.clone(),
            },
            Self::Done => AssistantEvent::AgentEnd { data: Value::Null },
            Self::ToolCallDelta { name, delta } => AssistantEvent::ToolExecutionEnd {
                name: name.clone(),
                output: delta.clone(),
                error: None,
            },
            Self::Unknown => AssistantEvent::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_message_update_with_text_delta() {
        let event: PiEvent = serde_json::from_value(serde_json::json!({
            "type": "message_update",
            "assistant_event": {"type": "text_delta", "text": "hello"}
        }))
        .expect("event should parse");

        assert_eq!(
            event.assistant_event(),
            Some(AssistantEvent::TextDelta {
                text: "hello".to_string()
            })
        );
    }

    #[test]
    fn parses_tool_execution_end() {
        let event: PiEvent = serde_json::from_value(serde_json::json!({
            "type": "tool_execution_end",
            "name": "note_get",
            "output": {"ok": true}
        }))
        .expect("event should parse");

        assert_eq!(
            event.assistant_event(),
            Some(AssistantEvent::ToolExecutionEnd {
                name: "note_get".to_string(),
                output: serde_json::json!({"ok": true}),
                error: None
            })
        );
    }
}
