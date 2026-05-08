use crate::assistant::rpc::AssistantEvent;
use crate::CliError;
use serde::Serialize;
use serde_json::Value;
use std::io::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderOptions {
    pub(crate) show_thinking: bool,
    pub(crate) json_events: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AssistantRenderReport {
    pub(crate) text: String,
    pub(crate) event_count: usize,
    pub(crate) tool_calls: Vec<AssistantToolCallSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AssistantToolCallSummary {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

pub(crate) struct AssistantRenderer<W> {
    writer: W,
    options: RenderOptions,
    text: String,
    event_count: usize,
    tool_calls: Vec<AssistantToolCallSummary>,
}

impl<W: Write> AssistantRenderer<W> {
    pub(crate) fn new(writer: W, options: RenderOptions) -> Self {
        Self {
            writer,
            options,
            text: String::new(),
            event_count: 0,
            tool_calls: Vec::new(),
        }
    }

    pub(crate) fn render_event(&mut self, event: &AssistantEvent) -> Result<(), CliError> {
        self.event_count += 1;
        if self.options.json_events {
            writeln!(
                self.writer,
                "{}",
                serde_json::to_string(event).map_err(CliError::operation)?
            )
            .map_err(CliError::operation)?;
            return Ok(());
        }

        match event {
            AssistantEvent::TextDelta { text } => {
                self.text.push_str(text);
                write!(self.writer, "{text}").map_err(CliError::operation)?;
                self.writer.flush().map_err(CliError::operation)?;
            }
            AssistantEvent::ThinkingDelta { text } if self.options.show_thinking => {
                write!(self.writer, "{text}").map_err(CliError::operation)?;
                self.writer.flush().map_err(CliError::operation)?;
            }
            AssistantEvent::ToolExecutionStart { name, .. } => {
                writeln!(self.writer, "\n[tool:{name}]").map_err(CliError::operation)?;
            }
            AssistantEvent::ToolExecutionEnd { name, error, .. } => {
                self.tool_calls.push(AssistantToolCallSummary {
                    name: name.clone(),
                    error: error.clone(),
                });
            }
            AssistantEvent::Error { message } => {
                writeln!(self.writer, "\nerror: {message}").map_err(CliError::operation)?;
            }
            AssistantEvent::ThinkingDelta { .. }
            | AssistantEvent::AgentEnd { .. }
            | AssistantEvent::Unknown => {}
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<AssistantRenderReport, CliError> {
        if !self.options.json_events && !self.text.ends_with('\n') {
            writeln!(self.writer).map_err(CliError::operation)?;
        }
        Ok(AssistantRenderReport {
            text: self.text,
            event_count: self.event_count,
            tool_calls: self.tool_calls,
        })
    }
}

pub(crate) fn event_output_value(event: &AssistantEvent) -> Value {
    serde_json::to_value(event).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_accumulates_text_and_tool_summary() {
        let output = Vec::new();
        let mut renderer = AssistantRenderer::new(
            output,
            RenderOptions {
                show_thinking: false,
                json_events: false,
            },
        );

        renderer
            .render_event(&AssistantEvent::ThinkingDelta {
                text: "hidden".to_string(),
            })
            .expect("thinking should render");
        renderer
            .render_event(&AssistantEvent::TextDelta {
                text: "hello".to_string(),
            })
            .expect("text should render");
        renderer
            .render_event(&AssistantEvent::ToolExecutionEnd {
                name: "search".to_string(),
                output: Value::Null,
                error: None,
            })
            .expect("tool should render");
        let report = renderer.finish().expect("renderer should finish");

        assert_eq!(report.text, "hello");
        assert_eq!(report.event_count, 3);
        assert_eq!(
            report.tool_calls,
            vec![AssistantToolCallSummary {
                name: "search".to_string(),
                error: None
            }]
        );
    }

    #[test]
    fn json_event_mode_writes_serialized_events() {
        let output = Vec::new();
        let mut renderer = AssistantRenderer::new(
            output,
            RenderOptions {
                show_thinking: false,
                json_events: true,
            },
        );
        renderer
            .render_event(&AssistantEvent::TextDelta {
                text: "hello".to_string(),
            })
            .expect("event should render");

        let report = renderer.finish().expect("renderer should finish");
        assert_eq!(report.text, "");
        assert_eq!(
            event_output_value(&AssistantEvent::TextDelta {
                text: "hello".to_string()
            }),
            serde_json::json!({"type":"text_delta","text":"hello"})
        );
    }
}
