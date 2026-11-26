//! The tasks extension (`io.modelcontextprotocol/tasks`, 2026-07-28) over
//! Vulcan's operation records. A task-capable tool called by a modern client
//! that declares the extension, and still running after [`task_after`],
//! answers with a task instead of making the client wait; the task's id is
//! the call's operation id, so `tasks/get`, `tasks/cancel` and
//! `operation_get` all read the same record.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::mcp_operations::McpOperationLookup;
use crate::mcp_protocol::McpMethodError;

/// The tasks extension's identifier, for `capabilities.extensions`.
pub const MCP_TASKS_EXTENSION: &str = "io.modelcontextprotocol/tasks";
/// JSON-RPC error for a request that needs a capability the client did not
/// declare.
pub const MCP_MISSING_CLIENT_CAPABILITY: i64 = -32021;

/// Tools whose calls may become tasks: those that can run for a long time.
pub const TASK_CAPABLE_TOOLS: &[&str] = &[
    "index_scan",
    "sync_refresh",
    "changeset_land",
    "web_fetch",
    "query",
    "search",
];

/// How long a task-capable call runs before it becomes a task.
/// `VULCAN_MCP_TASK_AFTER_MS` overrides the 5 seconds, for tests.
#[must_use]
pub fn task_after() -> Duration {
    std::env::var("VULCAN_MCP_TASK_AFTER_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(Duration::from_secs(5), Duration::from_millis)
}

/// Whether a request's `_meta` declares the tasks extension.
#[must_use]
pub fn declares_tasks(params: Option<&Value>) -> bool {
    params
        .and_then(|params| params.pointer("/_meta/io.modelcontextprotocol~1clientCapabilities"))
        .and_then(|capabilities| capabilities.get("extensions"))
        .and_then(|extensions| extensions.get(MCP_TASKS_EXTENSION))
        .is_some_and(Value::is_object)
}

/// Whether this request may be answered with a task: a modern `tools/call`
/// of a task-capable tool from a client that declares the extension.
#[must_use]
pub fn task_eligible(request: &Value) -> bool {
    request.get("method").and_then(Value::as_str) == Some("tools/call")
        && request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .is_some_and(|name| TASK_CAPABLE_TOOLS.contains(&name))
        && declares_tasks(request.get("params"))
}

/// The `tools/call` response that hands the client a task (`CreateTaskResult`).
#[must_use]
pub fn task_created_response(id: Value, task: Value) -> Value {
    let mut result = match task {
        Value::Object(task) => task,
        _ => Map::new(),
    };
    // `CreateTaskResult` is the task itself, never its outcome.
    result.remove("result");
    result.remove("error");
    result.insert("resultType".into(), Value::from("task"));
    let mut response = Map::new();
    response.insert("jsonrpc".into(), Value::from("2.0"));
    response.insert("id".into(), id);
    response.insert("result".into(), Value::Object(result));
    Value::Object(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TaskParams {
    task_id: String,
    #[serde(default)]
    input_responses: Option<Value>,
}

/// `tasks/get`, `tasks/update` and `tasks/cancel`.
pub fn handle_task_method(
    method: &str,
    params: Option<&Value>,
    operations: Option<&dyn McpOperationLookup>,
) -> Result<Value, McpMethodError> {
    if !declares_tasks(params) {
        return Err(McpMethodError::JsonRpc {
            code: MCP_MISSING_CLIENT_CAPABILITY,
            message: format!("`{method}` needs the {MCP_TASKS_EXTENSION} extension"),
            data: Some(json!({
                "requiredCapabilities": { "extensions": { MCP_TASKS_EXTENSION: {} } }
            })),
        });
    }
    let Some(operations) = operations else {
        return Err(McpMethodError::method_not_found(format!(
            "this transport keeps no tasks: Method not found: {method}"
        )));
    };
    let params: TaskParams = crate::mcp_core::parse(params)?;
    let not_found = || McpMethodError::JsonRpc {
        code: -32602,
        message: "Failed to retrieve task: Task not found".to_string(),
        data: Some(json!({ "taskId": params.task_id })),
    };
    match method {
        "tasks/get" => operations.task(&params.task_id).ok_or_else(not_found),
        // Vulcan's tasks never ask for input, so there is nothing to apply.
        "tasks/update" => {
            let _ = params.input_responses;
            operations
                .task(&params.task_id)
                .map(|_| json!({}))
                .ok_or_else(not_found)
        }
        _ => {
            if operations.cancel(&params.task_id) {
                Ok(json!({}))
            } else {
                Err(not_found())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_operations::McpOperationRegistry;

    fn declaring(extra: Value) -> Value {
        let mut params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": { MCP_TASKS_EXTENSION: {} }
                }
            }
        });
        if let (Some(params), Value::Object(extra)) = (params.as_object_mut(), extra) {
            params.extend(extra);
        }
        params
    }

    #[test]
    fn only_task_capable_calls_from_declaring_clients_become_tasks() {
        let call = |name: &str, params: Value| {
            let mut params = params;
            params["name"] = Value::from(name);
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params})
        };
        assert!(task_eligible(&call("index_scan", declaring(json!({})))));
        assert!(!task_eligible(&call("note_get", declaring(json!({})))));
        assert!(!task_eligible(&call(
            "index_scan",
            json!({"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }})
        )));
        assert!(!task_eligible(&call("index_scan", json!({}))));
    }

    #[test]
    fn tasks_follow_their_operation_from_working_to_completed() {
        let registry = McpOperationRegistry::default();
        let id = registry.begin("index_scan");
        let get = |registry: &McpOperationRegistry| {
            handle_task_method(
                "tasks/get",
                Some(&declaring(json!({"taskId": id}))),
                Some(registry),
            )
            .unwrap()
        };
        let working = get(&registry);
        assert_eq!(working["taskId"], id.as_str());
        assert_eq!(working["status"], "working");
        assert!(working["ttlMs"].is_null());
        assert!(working["createdAt"].as_str().unwrap().ends_with('Z'));
        let created = task_created_response(json!(7), working.clone());
        assert_eq!(created["result"]["resultType"], "task");
        assert_eq!(created["result"]["taskId"], id.as_str());

        let outcome = json!({"content": [], "structuredContent": {"ok": true}, "isError": false});
        registry.finish(
            &id,
            Some(&json!({"jsonrpc": "2.0", "id": 7, "result": outcome})),
        );
        let completed = get(&registry);
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["result"], outcome);
        assert!(completed["ttlMs"].as_u64().unwrap() >= 3_600_000);
        assert!(task_created_response(json!(7), completed)["result"]
            .get("result")
            .is_none());

        let missing = handle_task_method(
            "tasks/get",
            Some(&declaring(json!({"taskId": "nope"}))),
            Some(&registry),
        );
        assert!(matches!(
            missing,
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        let undeclared =
            handle_task_method("tasks/get", Some(&json!({"taskId": id})), Some(&registry));
        assert!(matches!(
            undeclared,
            Err(McpMethodError::JsonRpc {
                code: MCP_MISSING_CLIENT_CAPABILITY,
                ..
            })
        ));
    }

    #[test]
    fn a_cancelled_task_that_fails_reports_cancelled_and_one_that_finishes_completed() {
        let registry = McpOperationRegistry::default();
        let token = crate::execution::ExecutionCancellationToken::default();
        let stopped = registry.begin("web_fetch");
        registry.attach_cancellation(&stopped, token.clone());
        let ack = handle_task_method(
            "tasks/cancel",
            Some(&declaring(json!({"taskId": stopped}))),
            Some(&registry),
        )
        .unwrap();
        assert_eq!(ack, json!({}));
        assert!(token.is_cancelled());
        assert_eq!(
            registry.task_owned("", &stopped).unwrap()["status"],
            "working"
        );
        registry.finish(
            &stopped,
            Some(&json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32800, "message": "cancelled"}})),
        );
        assert_eq!(
            registry.task_owned("", &stopped).unwrap()["status"],
            "cancelled"
        );

        let finished = registry.begin("index_scan");
        assert!(registry.cancel_owned("", &finished));
        registry.finish(
            &finished,
            Some(&json!({"jsonrpc": "2.0", "id": 2, "result": {"content": [], "isError": false}})),
        );
        assert_eq!(
            registry.task_owned("", &finished).unwrap()["status"],
            "completed"
        );

        let failed = registry.begin("index_scan");
        registry.finish(&failed, None);
        let failed = registry.task_owned("", &failed).unwrap();
        assert_eq!(failed["status"], "failed");
        assert_eq!(failed["error"]["code"], -32603);

        let updated = handle_task_method(
            "tasks/update",
            Some(&declaring(json!({"taskId": stopped, "inputResponses": {}}))),
            Some(&registry),
        )
        .unwrap();
        assert_eq!(updated, json!({}));
    }
}
