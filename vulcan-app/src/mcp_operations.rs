//! Records of MCP tool calls, so a call that outlives its request can still be
//! looked up. When a call exceeds the request timeout the client gets an
//! `indeterminate` error naming the operation, and `operation_get` later
//! reports how it ended.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::execution::ExecutionCancellationToken;
use crate::mcp_protocol::{McpMethodError, McpToolErrorCode};

/// Records kept, newest first; older ones are forgotten.
const MAX_RECORDS: usize = 256;
/// How long a finished operation stays visible.
const RETENTION_MS: u64 = 60 * 60 * 1000;

/// Where `operation_get` looks operations up. A transport that keeps
/// operation records provides one; without it the tool is not offered.
pub trait McpOperationLookup: Send + Sync + std::fmt::Debug {
    fn get(&self, operation_id: &str) -> Option<Value>;

    /// The operation as a task of the tasks extension (`DetailedTask`), for
    /// lookups that can serve tasks.
    fn task(&self, _operation_id: &str) -> Option<Value> {
        None
    }

    /// Ask a running operation to stop; `false` when there is no such
    /// operation.
    fn cancel(&self, _operation_id: &str) -> bool {
        false
    }
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
    /// Who may look the record up: the caller that made the call. One
    /// client (stdio) uses the empty owner.
    #[serde(skip)]
    pub owner: String,
    pub operation_id: String,
    pub tool: String,
    pub state: McpOperationState,
    pub started_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_unix_ms: Option<u64>,
    /// The tool's structured result, or its error, once it has finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The whole `tools/call` result, for a task's `completed` status.
    #[serde(skip)]
    pub call_result: Option<Value>,
    /// The JSON-RPC error the call ended with, for a task's `failed` status.
    #[serde(skip)]
    pub call_error: Option<Value>,
    /// Whether a client asked for the operation to stop.
    #[serde(skip)]
    pub cancel_requested: bool,
    #[serde(skip)]
    pub cancellation: Option<ExecutionCancellationToken>,
}

/// How often a client should poll a task.
pub const TASK_POLL_INTERVAL_MS: u64 = 2_000;

impl McpOperationRecord {
    /// This operation as a `DetailedTask` (tasks extension, 2026-07-28):
    /// `working` while it runs, then `completed` with its `tools/call` result
    /// (a tool error included), `failed` with the JSON-RPC error it ended
    /// with, or `cancelled` when a requested cancellation stopped it.
    #[must_use]
    pub fn task(&self) -> Value {
        let mut task = serde_json::Map::new();
        task.insert("taskId".into(), Value::from(self.operation_id.clone()));
        task.insert(
            "createdAt".into(),
            Value::from(iso_timestamp(self.started_unix_ms)),
        );
        task.insert(
            "lastUpdatedAt".into(),
            Value::from(iso_timestamp(
                self.finished_unix_ms.unwrap_or(self.started_unix_ms),
            )),
        );
        task.insert("pollIntervalMs".into(), Value::from(TASK_POLL_INTERVAL_MS));
        let (status, message) = match (self.state, &self.call_result, &self.call_error) {
            (McpOperationState::Running, _, _) => (
                "working",
                if self.cancel_requested {
                    format!(
                        "`{}` is still running; cancellation was requested, and a call that has started stops only if the tool checks for it",
                        self.tool
                    )
                } else {
                    format!("`{}` is running", self.tool)
                },
            ),
            (_, Some(result), _) => {
                task.insert("result".into(), result.clone());
                ("completed", format!("`{}` finished", self.tool))
            }
            (_, None, Some(_)) if self.cancel_requested => {
                ("cancelled", format!("`{}` was cancelled", self.tool))
            }
            (_, None, error) => {
                let error = error.clone().unwrap_or_else(|| {
                    json!({
                        "code": -32603,
                        "message": "the call ended without a response",
                    })
                });
                let message = error["message"]
                    .as_str()
                    .unwrap_or("the call failed")
                    .to_string();
                task.insert("error".into(), error);
                ("failed", message)
            }
        };
        task.insert("status".into(), Value::from(status));
        task.insert("statusMessage".into(), Value::from(message));
        // Records last an hour after they finish; a running one has no
        // set end.
        let ttl = self.finished_unix_ms.map_or(Value::Null, |finished| {
            Value::from(finished.saturating_sub(self.started_unix_ms) + RETENTION_MS)
        });
        task.insert("ttlMs".into(), ttl);
        Value::Object(task)
    }
}

fn iso_timestamp(unix_ms: u64) -> String {
    i64::try_from(unix_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Operation records held by one server process or listener, each visible
/// only to the caller that made the call.
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
        self.begin_owned("", tool)
    }

    /// [`Self::begin`] for a call made by `owner`.
    pub fn begin_owned(&self, owner: &str, tool: &str) -> String {
        let operation_id = ulid::Ulid::new().to_string();
        self.begin_owned_as(owner, tool, &operation_id);
        operation_id
    }

    /// Record a call that already has an operation id, such as one a
    /// durable ledger assigned, so it can also be followed as a task.
    pub fn begin_owned_as(&self, owner: &str, tool: &str, operation_id: &str) {
        let operation_id = operation_id.to_string();
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
            owner: owner.to_string(),
            operation_id: operation_id.clone(),
            tool: tool.to_string(),
            state: McpOperationState::Running,
            started_unix_ms: now,
            finished_unix_ms: None,
            result: None,
            call_result: None,
            call_error: None,
            cancel_requested: false,
            cancellation: None,
        });
    }

    /// Let a client's cancellation reach the call through `token`.
    pub fn attach_cancellation(&self, operation_id: &str, token: ExecutionCancellationToken) {
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.operation_id == operation_id)
        {
            record.cancellation = Some(token);
        }
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
            record.call_result = response
                .and_then(|response| response.get("result"))
                .cloned();
            record.call_error = response.and_then(|response| response.get("error")).cloned();
        }
    }
}

impl McpOperationRegistry {
    /// The record of `operation_id` if `owner` made the call.
    #[must_use]
    pub fn get_owned(&self, owner: &str, operation_id: &str) -> Option<Value> {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records
            .iter()
            .find(|record| record.operation_id == operation_id && record.owner == owner)
            .and_then(|record| serde_json::to_value(record).ok())
    }
}

impl McpOperationRegistry {
    /// The record of `operation_id` as a task, if `owner` made the call.
    #[must_use]
    pub fn task_owned(&self, owner: &str, operation_id: &str) -> Option<Value> {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records
            .iter()
            .find(|record| record.operation_id == operation_id && record.owner == owner)
            .map(McpOperationRecord::task)
    }

    /// Ask `owner`'s operation to stop. A call stops only where it checks its
    /// cancellation token, so it may still finish.
    pub fn cancel_owned(&self, owner: &str, operation_id: &str) -> bool {
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(record) = records
            .iter_mut()
            .find(|record| record.operation_id == operation_id && record.owner == owner)
        else {
            return false;
        };
        if record.state == McpOperationState::Running {
            record.cancel_requested = true;
            if let Some(token) = &record.cancellation {
                token.cancel();
            }
        }
        true
    }
}

impl McpOperationLookup for McpOperationRegistry {
    fn get(&self, operation_id: &str) -> Option<Value> {
        self.get_owned("", operation_id)
    }

    fn task(&self, operation_id: &str) -> Option<Value> {
        self.task_owned("", operation_id)
    }

    fn cancel(&self, operation_id: &str) -> bool {
        self.cancel_owned("", operation_id)
    }
}

/// One caller's view of a shared registry.
#[derive(Debug, Clone)]
pub struct McpOwnedOperations {
    pub registry: std::sync::Arc<McpOperationRegistry>,
    pub owner: String,
}

impl McpOperationLookup for McpOwnedOperations {
    fn get(&self, operation_id: &str) -> Option<Value> {
        self.registry.get_owned(&self.owner, operation_id)
    }

    fn task(&self, operation_id: &str) -> Option<Value> {
        self.registry.task_owned(&self.owner, operation_id)
    }

    fn cancel(&self, operation_id: &str) -> bool {
        self.registry.cancel_owned(&self.owner, operation_id)
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
    fn owned_records_are_visible_only_to_their_owner() {
        let registry = std::sync::Arc::new(McpOperationRegistry::default());
        let id = registry.begin_owned("alice", "note_get");
        let alice = McpOwnedOperations {
            registry: std::sync::Arc::clone(&registry),
            owner: "alice".to_string(),
        };
        let bob = McpOwnedOperations {
            registry: std::sync::Arc::clone(&registry),
            owner: "bob".to_string(),
        };
        assert_eq!(alice.get(&id).unwrap()["tool"], "note_get");
        assert!(alice.get(&id).unwrap().get("owner").is_none());
        assert!(bob.get(&id).is_none());
        assert!(registry.get(&id).is_none());
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
