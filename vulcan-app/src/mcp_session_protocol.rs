//! The host-facing MCP adapter: the stateless core plus, for legacy
//! clients, the `initialize` connection. stdio and HTTP hosts feed it one
//! JSON-RPC message at a time.

use serde_json::Value;
use std::sync::Arc;
use vulcan_core::{PermissionError, ResolvedPermissionProfile, VaultPaths};

use crate::mcp_authority::{McpAuthority, McpClientAttribution};
pub use crate::mcp_core::McpProtocolHost;
use crate::mcp_core::{handle, modern_meta, McpEra, McpLegacyConnection, McpRequestContext};
use crate::mcp_dispatch::{
    acquire_request_ordinary_write_gate, jsonrpc_error, process_http_request,
    process_stdio_request, request_id, McpHttpProcessResult, McpMethodHandler,
};
use crate::mcp_operations::McpOperationLookup;
use crate::mcp_protocol::{McpMethodError, McpMethodOutcome};

/// One client of one vault: the authority it acts under, the authenticated
/// OAuth client when there is one, and the legacy connection state that a
/// 2025 client's `initialize` creates. Modern requests use none of the
/// latter: each carries its own client information.
#[derive(Debug, Clone)]
pub struct McpProtocolCore {
    authority: McpAuthority,
    legacy: McpLegacyConnection,
    oauth_client_id: Option<String>,
    subject: Option<String>,
    host: McpProtocolHost,
    /// Whether legacy list changes ride along with responses. A host with
    /// its own change watcher turns this off and polls instead.
    list_changes_on_responses: bool,
    /// Operation records, where the host keeps them.
    operations: Option<Arc<dyn McpOperationLookup>>,
}

impl McpProtocolCore {
    pub fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        host: McpProtocolHost,
    ) -> Result<Self, PermissionError> {
        let authority = McpAuthority::new(paths, requested_profile)?;
        Ok(Self {
            legacy: McpLegacyConnection::new(&authority),
            authority,
            oauth_client_id: None,
            subject: None,
            host,
            list_changes_on_responses: true,
            operations: None,
        })
    }

    #[must_use]
    pub fn paths(&self) -> &VaultPaths {
        self.authority.paths()
    }

    #[must_use]
    pub fn selection(&self) -> &ResolvedPermissionProfile {
        self.authority.selection()
    }

    #[must_use]
    pub fn authority(&self) -> &McpAuthority {
        &self.authority
    }

    /// Re-read the profile; it may narrow, never widen.
    pub fn attenuate_profile(&mut self) -> Result<(), String> {
        self.authority.attenuate_profile()
    }

    /// Attribute requests to an authenticated remote client.
    pub fn set_authenticated_client(&mut self, client_id: Option<String>, subject: Option<String>) {
        self.oauth_client_id = client_id;
        self.subject = subject;
    }

    /// Offer `operation_get` over the host's operation records.
    pub fn keep_operations(&mut self, operations: Arc<dyn McpOperationLookup>) {
        self.operations = Some(operations);
    }

    /// Deliver legacy list changes through [`Self::poll_list_changes`] from a
    /// change watcher instead of with responses.
    pub fn watch_list_changes(&mut self) {
        self.list_changes_on_responses = false;
    }

    /// `list_changed` notifications for a legacy connection that has
    /// initialized, for what changed since the last poll.
    pub fn poll_list_changes(&mut self) -> Vec<Value> {
        if self.legacy.version().is_none() {
            return Vec::new();
        }
        self.legacy.list_changed_notifications(&self.authority)
    }

    /// The legacy connection's negotiated version, once `initialize` ran.
    #[must_use]
    pub fn legacy_version(&self) -> Option<&str> {
        self.legacy.version()
    }

    fn client(&self, self_reported: &McpClientAttribution) -> McpClientAttribution {
        McpClientAttribution {
            oauth_client_id: self.oauth_client_id.clone(),
            subject: self.subject.clone(),
            ..self_reported.clone()
        }
    }

    /// Process a local request while excluding cooperating writers from reads
    /// and refusing canonical state with an unfinished ordinary-write journal.
    pub fn process_request(&mut self, request: Value) -> Vec<Value> {
        let _read_guard = match acquire_request_ordinary_write_gate(self.paths(), &request) {
            Ok(guard) => guard,
            Err(message) => {
                return request_id(&request)
                    .map_or_default(|id| vec![jsonrpc_error(id, -32603, message, None)]);
            }
        };
        process_stdio_request(self, request)
    }

    /// Apply the same canonical read barrier before HTTP protocol dispatch.
    pub fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
        let _read_guard = match acquire_request_ordinary_write_gate(self.paths(), request) {
            Ok(guard) => guard,
            Err(message) => {
                return if let Some(id) = request_id(request) {
                    Err(jsonrpc_error(id, -32603, message, None))
                } else {
                    Ok(McpHttpProcessResult {
                        response: None,
                        notifications: Vec::new(),
                        accepted_notification: true,
                        session_stale: false,
                    })
                };
            }
        };
        process_http_request(self, request)
    }
}

impl McpMethodHandler for McpProtocolCore {
    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError> {
        if let Some(meta) = modern_meta(params)? {
            let mut reported = McpClientAttribution::default();
            reported.record_implementation(meta.client_info.as_ref());
            let client = self.client(&reported);
            let response = handle(
                &McpRequestContext {
                    authority: &self.authority,
                    client: &client,
                    era: McpEra::Modern,
                    host: &self.host,
                    operations: self.operations.as_deref(),
                },
                method,
                params,
            )?;
            // Modern clients learn of list changes by subscribing, not
            // through notifications piggybacked on responses.
            return Ok(McpMethodOutcome {
                response,
                emit_list_notifications: false,
            });
        }
        let response = match method {
            "initialize" => Some(self.legacy.initialize(&self.authority, &self.host, params)),
            "ping" => Some(Value::Object(serde_json::Map::new())),
            "notifications/initialized" => None,
            _ => {
                let client = self.client(self.legacy.client());
                handle(
                    &McpRequestContext {
                        authority: &self.authority,
                        client: &client,
                        era: McpEra::Legacy,
                        host: &self.host,
                        operations: self.operations.as_deref(),
                    },
                    method,
                    params,
                )?
            }
        };
        Ok(McpMethodOutcome {
            response,
            emit_list_notifications: !matches!(
                method,
                "initialize" | "notifications/initialized" | "notifications/cancelled"
            ),
        })
    }

    fn list_changed_notifications(&mut self) -> Vec<Value> {
        if self.list_changes_on_responses {
            self.legacy.list_changed_notifications(&self.authority)
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{McpProtocolCore, McpProtocolHost};
    use crate::mcp_dispatch::McpMethodHandler;
    use crate::mcp_help::HelpTopicReport;
    use vulcan_core::VaultPaths;

    fn no_command_help(_: &[String]) -> Result<HelpTopicReport, String> {
        Err("unknown command help".to_string())
    }

    fn no_help_candidates(_: &str) -> Vec<String> {
        vec![]
    }

    #[test]
    fn one_core_serves_legacy_and_modern_requests() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let mut core = McpProtocolCore::new(
            &paths,
            Some("readonly"),
            McpProtocolHost {
                command_help: no_command_help,
                help_candidates: no_help_candidates,
                server_version: "test-version",
            },
        )
        .unwrap();

        // A modern request needs no handshake and carries its own client.
        let meta = serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        }});
        let modern = core.handle_method("tools/list", Some(&meta)).unwrap();
        assert!(!modern.emit_list_notifications);
        let listed = modern.response.unwrap();
        assert_eq!(listed["resultType"], "complete");
        assert!(listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "note_get"));
        assert_eq!(core.legacy_version(), None);

        // A legacy client still negotiates through initialize.
        let initialize = core
            .handle_method(
                "initialize",
                Some(&serde_json::json!({"protocolVersion": "2025-11-25"})),
            )
            .unwrap();
        assert_eq!(
            initialize.response.unwrap()["protocolVersion"],
            "2025-11-25"
        );
        assert_eq!(core.legacy_version(), Some("2025-11-25"));
        let legacy = core.handle_method("tools/list", None).unwrap();
        assert!(legacy.response.unwrap().get("resultType").is_none());
        assert!(core.handle_method("ping", None).unwrap().response.is_some());
    }

    #[test]
    fn a_watching_host_polls_list_changes_once_the_client_initialized() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        let mut core = McpProtocolCore::new(
            &paths,
            Some("unrestricted"),
            McpProtocolHost {
                command_help: no_command_help,
                help_candidates: no_help_candidates,
                server_version: "watch",
            },
        )
        .unwrap();
        core.watch_list_changes();
        let prompts = vault.path().join("AI/Prompts");
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(prompts.join("one.md"), "---\nname: one\n---\nOne.\n").unwrap();
        // Nothing is announced before the client initialized.
        assert!(core.poll_list_changes().is_empty());
        core.handle_method("initialize", None).unwrap();
        // Responses no longer carry list changes; the poll does.
        let listed = core.handle_method("prompts/list", None).unwrap();
        assert!(listed.emit_list_notifications);
        assert!(core.list_changed_notifications().is_empty());
        let changes = core.poll_list_changes();
        assert!(
            changes
                .iter()
                .any(|change| change["method"] == "notifications/prompts/list_changed"),
            "{changes:?}"
        );
        assert!(core.poll_list_changes().is_empty());
    }

    #[test]
    fn persistent_core_routes_methods_with_injected_host_catalog() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let mut core = McpProtocolCore::new(
            &paths,
            Some("readonly"),
            McpProtocolHost {
                command_help: no_command_help,
                help_candidates: no_help_candidates,
                server_version: "app-core-test",
            },
        )
        .unwrap();
        let initialized = core.handle_method("initialize", None).unwrap();
        assert_eq!(
            initialized.response.unwrap()["serverInfo"]["version"],
            "app-core-test"
        );
        let listed = core.handle_method("tools/list", None).unwrap();
        assert!(listed.response.unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "note_get"));
    }

    #[test]
    fn both_core_transports_refuse_invalid_pending_write_state_without_removing_it() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        let mut core = McpProtocolCore::new(
            &paths,
            Some("readonly"),
            McpProtocolHost {
                command_help: no_command_help,
                help_candidates: no_help_candidates,
                server_version: "app-core-test",
            },
        )
        .unwrap();
        let state = paths
            .operational_state_dir()
            .unwrap()
            .join("ordinary-write");
        std::fs::create_dir_all(&state).unwrap();
        let journal = state.join("journal.json");
        std::fs::write(&journal, b"{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let request = serde_json::json!({"jsonrpc":"2.0", "id":7, "method":"tools/list"});
        let stdio = core.process_request(request.clone());
        assert_eq!(stdio[0]["id"], 7);
        assert_eq!(stdio[0]["error"]["code"], -32603);
        let http = core.process_http_request(&request).unwrap_err();
        assert_eq!(http, stdio[0]);
        let notification =
            serde_json::json!({"jsonrpc":"2.0", "method":"notifications/initialized"});
        assert!(core.process_request(notification.clone()).is_empty());
        let accepted = core.process_http_request(&notification).unwrap();
        assert!(accepted.accepted_notification);
        assert!(accepted.response.is_none());
        assert_eq!(std::fs::read(&journal).unwrap(), b"{}");
    }
}
