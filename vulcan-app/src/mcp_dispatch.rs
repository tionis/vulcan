//! Transport-neutral MCP JSON-RPC request routing shared by stdio and HTTP.

#![allow(clippy::must_use_candidate, clippy::needless_pass_by_value)]

use serde_json::{Map, Value};
use std::time::Duration;
use vulcan_core::{write_lock::ReadLockGuard, VaultPaths};

use crate::mcp_catalog::tool_by_name;
use crate::mcp_protocol::{McpMethodError, McpMethodOutcome};

pub trait McpMethodHandler {
    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError>;
    fn list_changed_notifications(&mut self) -> Vec<Value>;
}

/// Classify requests conservatively for scheduling and the canonical read barrier.
/// Unknown and custom tool calls may write even when their names look read-only.
pub fn request_is_read_only(payload: &Value) -> bool {
    if payload.get("method").and_then(Value::as_str) != Some("tools/call") {
        return true;
    }
    let Some(name) = payload
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    // Listing suggestions reads the vault but refreshes their cache rows, so
    // it is scheduled like a write.
    if name == "link_suggestions" {
        return false;
    }
    tool_by_name(name).is_some_and(|tool| tool.annotations.read_only_hint)
}

/// Refuse pending multi-file ordinary writes before dispatch. A read holds the
/// shared vault lock through response construction; mutations fail closed and
/// acquire their write lock in the target application workflow.
pub fn acquire_request_ordinary_write_gate(
    paths: &VaultPaths,
    payload: &Value,
) -> Result<Option<ReadLockGuard>, String> {
    if request_is_read_only(payload) {
        vulcan_core::ordinary_write::acquire_consistent_ordinary_read(paths)
            .map_err(|error| error.to_string())
    } else {
        vulcan_core::ordinary_write::ensure_no_pending_ordinary_write_batch(paths)
            .map_err(|error| error.to_string())?;
        Ok(None)
    }
}

#[derive(Debug)]
pub struct McpHttpProcessResult {
    pub response: Option<Value>,
    pub notifications: Vec<Value>,
    pub accepted_notification: bool,
    /// A worker may still be running after its response deadline. The
    /// transport must retire this session instead of reusing a stale clone.
    pub session_stale: bool,
}

pub fn process_stdio_request<H: McpMethodHandler>(handler: &mut H, request: Value) -> Vec<Value> {
    let Some(request_object) = request.as_object() else {
        return vec![jsonrpc_error(
            Value::Null,
            -32600,
            "Invalid request".to_string(),
            None,
        )];
    };
    if request_object.contains_key("result") || request_object.contains_key("error") {
        return vec![jsonrpc_error(
            request_object.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "Invalid request".to_string(),
            None,
        )];
    }
    if request.is_array() {
        return vec![jsonrpc_error(
            Value::Null,
            -32600,
            "Batch requests are not supported: send one JSON-RPC message at a time".to_string(),
            None,
        )];
    }

    let id = request_object.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = !request_object.contains_key("id");
    let Some(method) = request_object.get("method").and_then(Value::as_str) else {
        if is_notification {
            return Vec::new();
        }
        return vec![jsonrpc_error(
            id,
            -32600,
            "Invalid request".to_string(),
            None,
        )];
    };

    let outcome = match handler.handle_method(method, request_object.get("params")) {
        Ok(outcome) => outcome,
        Err(McpMethodError::JsonRpc {
            code,
            message,
            data,
        }) => {
            if is_notification {
                return Vec::new();
            }
            return vec![jsonrpc_error(id, code, message, data)];
        }
        Err(McpMethodError::Tool {
            message,
            structured,
        }) => {
            if is_notification {
                return Vec::new();
            }
            return vec![tool_error_response(id, message, structured)];
        }
    };

    let mut messages = Vec::new();
    if outcome.emit_list_notifications {
        messages.extend(handler.list_changed_notifications());
    }
    if let Some(response) = outcome.response {
        messages.push(jsonrpc_result(id, response));
    }
    messages
}

pub fn process_http_request<H: McpMethodHandler>(
    handler: &mut H,
    request: &Value,
) -> Result<McpHttpProcessResult, Value> {
    let Some(request_object) = request.as_object() else {
        return Err(jsonrpc_error(
            Value::Null,
            -32600,
            "Invalid request".to_string(),
            None,
        ));
    };
    if request.is_array() {
        return Err(jsonrpc_error(
            Value::Null,
            -32600,
            "Batch requests are not supported: send one JSON-RPC message at a time".to_string(),
            None,
        ));
    }
    if request_object.contains_key("result") || request_object.contains_key("error") {
        return Ok(McpHttpProcessResult {
            response: None,
            notifications: Vec::new(),
            accepted_notification: true,
            session_stale: false,
        });
    }

    let id = request_object.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = !request_object.contains_key("id");
    let Some(method) = request_object.get("method").and_then(Value::as_str) else {
        return Err(jsonrpc_error(
            if is_notification { Value::Null } else { id },
            -32600,
            "Invalid request".to_string(),
            None,
        ));
    };

    let outcome = match handler.handle_method(method, request_object.get("params")) {
        Ok(outcome) => outcome,
        Err(McpMethodError::JsonRpc {
            code,
            message,
            data,
        }) => {
            if is_notification {
                return Err(jsonrpc_error(Value::Null, code, message, data));
            }
            return Ok(McpHttpProcessResult {
                response: Some(jsonrpc_error(id, code, message, data)),
                notifications: Vec::new(),
                accepted_notification: false,
                session_stale: false,
            });
        }
        Err(McpMethodError::Tool {
            message,
            structured,
        }) => {
            if is_notification {
                return Err(jsonrpc_error(Value::Null, -32603, message, structured));
            }
            return Ok(McpHttpProcessResult {
                response: Some(tool_error_response(id, message, structured)),
                notifications: Vec::new(),
                accepted_notification: false,
                session_stale: false,
            });
        }
    };

    let notifications = if outcome.emit_list_notifications {
        handler.list_changed_notifications()
    } else {
        Vec::new()
    };

    Ok(McpHttpProcessResult {
        response: if is_notification {
            None
        } else {
            outcome
                .response
                .map(|response| jsonrpc_result(id, response))
        },
        notifications,
        accepted_notification: is_notification,
        session_stale: false,
    })
}

pub fn jsonrpc_result(id: Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub fn jsonrpc_error(id: Value, code: i64, message: String, data: Option<Value>) -> Value {
    let mut error = Map::new();
    error.insert("code".to_string(), Value::Number(code.into()));
    error.insert("message".to_string(), Value::String(message));
    if let Some(data) = data {
        error.insert("data".to_string(), data);
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

pub fn tool_error_response(id: Value, message: String, structured: Option<Value>) -> Value {
    let structured = crate::mcp_protocol::tool_error_structured(&message, structured);
    jsonrpc_result(
        id,
        serde_json::json!({
            "content": [{
                "type": "text",
                "text": message,
            }],
            "structuredContent": structured,
            "isError": true,
        }),
    )
}

pub fn timeout_response_for_request(request: &Value, timeout: Duration) -> Option<Value> {
    let id = request_id(request)?;
    let message = format!(
        "MCP request timed out after {}ms",
        timeout.as_millis().max(1)
    );
    if request_method(request) == Some("tools/call") {
        Some(tool_error_response(
            id,
            message.clone(),
            Some(serde_json::json!({
                "error": message,
                "timed_out": true,
                "timeout_ms": timeout.as_millis().max(1),
            })),
        ))
    } else {
        Some(jsonrpc_error(
            id,
            -32000,
            message,
            Some(serde_json::json!({
                "timed_out": true,
                "timeout_ms": timeout.as_millis().max(1),
            })),
        ))
    }
}

pub fn timeout_http_result(request: &Value, timeout: Duration) -> McpHttpProcessResult {
    let response = timeout_response_for_request(request, timeout);
    McpHttpProcessResult {
        accepted_notification: response.is_none(),
        response,
        notifications: Vec::new(),
        session_stale: true,
    }
}

pub fn request_id(request: &Value) -> Option<Value> {
    request
        .as_object()
        .and_then(|object| object.get("id"))
        .cloned()
}

/// Decide whether an HTTP request may allocate a fresh MCP session. Ordinary
/// notifications do not need an ID, but `initialize` must be a request with a
/// usable JSON-RPC ID; otherwise it could consume a session slot invisibly.
pub fn http_initialize_request(payload: &Value) -> Result<bool, Value> {
    let Some(object) = payload.as_object() else {
        return Ok(false);
    };
    if object.get("method").and_then(Value::as_str) != Some("initialize") {
        return Ok(false);
    }
    if object.contains_key("result")
        || object.contains_key("error")
        || object
            .get("id")
            .is_none_or(|id| !matches!(id, Value::String(_) | Value::Number(_)))
    {
        return Err(jsonrpc_error(
            Value::Null,
            -32600,
            "MCP initialize requires a string or number request ID".to_string(),
            None,
        ));
    }
    Ok(true)
}

fn request_method(request: &Value) -> Option<&str> {
    request
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn http_initialize_requires_a_request_id_before_session_admission() {
        assert_eq!(
            http_initialize_request(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"})),
            Ok(true)
        );
        assert_eq!(
            http_initialize_request(&json!({"jsonrpc":"2.0","id":"start","method":"initialize"})),
            Ok(true)
        );
        assert_eq!(
            http_initialize_request(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            Ok(false)
        );
        for malformed in [
            json!({"jsonrpc":"2.0","method":"initialize"}),
            json!({"jsonrpc":"2.0","id":null,"method":"initialize"}),
            json!({"jsonrpc":"2.0","id":true,"method":"initialize"}),
            json!({"jsonrpc":"2.0","id":{},"method":"initialize"}),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","result":{}}),
        ] {
            let error = http_initialize_request(&malformed).expect_err("invalid initialize");
            assert_eq!(error["error"]["code"], -32600);
            assert!(error["id"].is_null());
        }
    }

    #[test]
    fn request_classification_is_conservative_for_unknown_and_effectful_tools() {
        let call = |name: &str, arguments: Value| json!({"method": "tools/call", "params": {"name": name, "arguments": arguments}});
        assert!(request_is_read_only(&json!({"method": "tools/list"})));
        assert!(request_is_read_only(&call("note_get", json!({}))));
        assert!(request_is_read_only(&call("web_fetch", json!({}))));
        assert!(!request_is_read_only(&call("note_create", json!({}))));
        assert!(!request_is_read_only(&call("custom_tool", json!({}))));
        assert!(request_is_read_only(&call("graph_communities", json!({}))));
        // Listing suggestions refreshes their cache rows.
        assert!(!request_is_read_only(&call("link_suggestions", json!({}))));
        assert!(!request_is_read_only(&call(
            "link_suggestion_resolve",
            json!({})
        )));
        assert!(!request_is_read_only(&json!({"method": "tools/call"})));
    }

    #[derive(Default)]
    struct Handler {
        calls: usize,
    }

    impl McpMethodHandler for Handler {
        fn handle_method(
            &mut self,
            method: &str,
            _params: Option<&Value>,
        ) -> Result<McpMethodOutcome, McpMethodError> {
            self.calls += 1;
            match method {
                "test/change" => Ok(McpMethodOutcome {
                    response: Some(json!({"ok": true})),
                    emit_list_notifications: true,
                }),
                "test/tool-error" => Err(McpMethodError::tool("denied")),
                _ => Err(McpMethodError::method_not_found("unknown")),
            }
        }

        fn list_changed_notifications(&mut self) -> Vec<Value> {
            vec![json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})]
        }
    }

    #[test]
    fn stdio_and_http_share_method_results_and_keep_transport_notification_rules() {
        let request = json!({"jsonrpc": "2.0", "id": 7, "method": "test/change"});
        let mut handler = Handler::default();
        let stdio = process_stdio_request(&mut handler, request.clone());
        let http = process_http_request(&mut handler, &request).unwrap();
        assert_eq!(stdio[0], http.notifications[0]);
        assert_eq!(stdio[1], http.response.unwrap());

        let notification = json!({"jsonrpc": "2.0", "method": "test/change"});
        let http_notification = process_http_request(&mut handler, &notification).unwrap();
        assert!(http_notification.accepted_notification);
        assert!(http_notification.response.is_none());
        assert_eq!(http_notification.notifications.len(), 1);
        assert_eq!(handler.calls, 3);
    }

    #[test]
    fn tool_errors_and_timeouts_preserve_json_rpc_shapes() {
        let mut handler = Handler::default();
        let request = json!({"jsonrpc": "2.0", "id": "abc", "method": "test/tool-error"});
        let response = process_http_request(&mut handler, &request)
            .unwrap()
            .response
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(response["id"], "abc");
        let timeout = timeout_response_for_request(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call"}),
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(timeout["result"]["structuredContent"]["timed_out"], true);
    }

    #[test]
    fn invalid_requests_do_not_invoke_the_handler() {
        let mut handler = Handler::default();
        let result = process_http_request(&mut handler, &json!([]));
        assert_eq!(result.unwrap_err()["error"]["code"], -32600);
        assert_eq!(handler.calls, 0);
    }
}
