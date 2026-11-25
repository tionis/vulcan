//! Records of MCP tool calls, so a call that outlives its request can still be
//! looked up. When a call exceeds the request timeout the client gets an
//! `indeterminate` error naming the operation, and `operation_get` later
//! reports how it ended.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::mcp_protocol::{McpMethodError, McpToolErrorCode};

/// Records kept, newest first; older ones are forgotten.
const MAX_RECORDS: usize = 256;
/// How long a finished operation stays visible.
const RETENTION_MS: u64 = 60 * 60 * 1000;

/// Where `operation_get` looks operations up. A transport that keeps
/// operation records provides one; without it the tool is not offered.
pub trait McpOperationLookup: Send + Sync + std::fmt::Debug {
    fn get(&self, operation_id: &str) -> Option<Value>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOperationState {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpOperationRecord {
    pub operation_id: String,
    pub tool: String,
    pub state: McpOperationState,
    pub started_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_unix_ms: Option<u64>,
    /// The tool's structured result, or its error, once it has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

/// Operation records held by one server process, for the client it serves.
#[derive(Debug, Default)]
pub struct McpOperationRegistry {
    records: Mutex<VecDeque<McpOperationRecord>>,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

impl McpOperationRegistry {
    /// Record that a call of `tool` started; returns its operation id.
    pub fn begin(&self, tool: &str) -> String {
        let operation_id = ulid::Ulid::new().to_string();
        let now = now_unix_ms();
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let expired = |record: &McpOperationRecord| {
            record
                .finished_unix_ms
                .is_some_and(|finished| now.saturating_sub(finished) > RETENTION_MS)
        };
        records.retain(|record| !expired(record));
        if records.len() >= MAX_RECORDS {
            records.pop_back();
        }
        records.push_front(McpOperationRecord {
            operation_id: operation_id.clone(),
            tool: tool.to_string(),
            state: McpOperationState::Running,
            started_unix_ms: now,
            finished_unix_ms: None,
            result: None,
        });
        operation_id
    }

    /// Record how a call ended, from its JSON-RPC response: a tool result
    /// (failed when `isError`) or a protocol error.
    pub fn finish(&self, operation_id: &str, response: Option<&Value>) {
        let (state, result) = match response {
            Some(response) if response.get("result").is_some() => {
                let result = &response["result"];
                let failed = result["isError"].as_bool() == Some(true);
                let structured = result
                    .get("structuredContent")
                    .cloned()
                    .unwrap_or_else(|| result.clone());
                (
                    if failed {
                        McpOperationState::Failed
                    } else {
                        McpOperationState::Succeeded
                    },
                    structured,
                )
            }
            Some(response) => (
                McpOperationState::Failed,
                json!({ "error": response.get("error").cloned().unwrap_or(Value::Null) }),
            ),
            None => (
                McpOperationState::Failed,
                json!({ "error": "the call ended without a response" }),
            ),
        };
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.operation_id == operation_id)
        {
            record.state = state;
            record.finished_unix_ms = Some(now_unix_ms());
            record.result = Some(result);
        }
    }
}

impl McpOperationLookup for McpOperationRegistry {
    fn get(&self, operation_id: &str) -> Option<Value> {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records
            .iter()
            .find(|record| record.operation_id == operation_id)
            .and_then(|record| serde_json::to_value(record).ok())
    }
}

/// The `operation_get` tool.
pub fn operation_get(
    lookup: &dyn McpOperationLookup,
    operation_id: &str,
) -> Result<Value, McpMethodError> {
    lookup.get(operation_id.trim()).ok_or_else(|| {
        McpMethodError::tool_coded(
            McpToolErrorCode::NotFound,
            format!(
                "no operation `{operation_id}`: ids come from an `indeterminate` error, and records last an hour"
            ),
            Some("id".to_string()),
        )
    })
}

/// The `tools/call` response for a call still running at its deadline.
#[must_use]
pub fn indeterminate_response(id: Value, operation_id: &str, timeout: Duration) -> Value {
    let message = format!(
        "still running after {}ms, so its outcome is not known yet; it was not stopped. Call operation_get with id `{operation_id}` to see how it ended before repeating it",
        timeout.as_millis().max(1)
    );
    crate::mcp_dispatch::tool_error_response(
        id,
        message.clone(),
        Some(json!({
            "error": {
                "code": McpToolErrorCode::Indeterminate.as_str(),
                "message": message,
                "hint": "call operation_get with operation_id",
            },
            "operation_id": operation_id,
            "timeout_ms": timeout.as_millis().max(1),
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_record_how_calls_ended_and_forget_the_oldest() {
        let registry = McpOperationRegistry::default();
        let ok = registry.begin("note_get");
        assert_eq!(registry.get(&ok).unwrap()["state"], "running");
        registry.finish(
            &ok,
            Some(&json!({"result": {"structuredContent": {"path": "Home.md"}, "isError": false}})),
        );
        let record = registry.get(&ok).unwrap();
        assert_eq!(record["state"], "succeeded");
        assert_eq!(record["result"]["path"], "Home.md");

        let failed = registry.begin("note_set");
        registry.finish(
            &failed,
            Some(&json!({"result": {"structuredContent": {"error": {"code": "conflict"}}, "isError": true}})),
        );
        assert_eq!(registry.get(&failed).unwrap()["state"], "failed");

        for _ in 0..MAX_RECORDS {
            registry.begin("search");
        }
        assert!(registry.get(&ok).is_none());
        let missing = operation_get(&registry, &ok).unwrap_err();
        assert!(matches!(missing, McpMethodError::Tool { .. }));
    }

    #[test]
    fn an_indeterminate_response_names_the_operation() {
        let response = indeterminate_response(json!(4), "01OP", Duration::from_millis(50));
        let result = &response["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "indeterminate"
        );
        assert_eq!(result["structuredContent"]["operation_id"], "01OP");
    }
}
