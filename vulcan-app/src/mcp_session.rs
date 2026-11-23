//! Transport-neutral MCP session authority, pack state, and discovery snapshots.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use vulcan_core::{
    resolve_permission_profile, PermissionError, ProfilePermissionGuard, ResolvedPermissionProfile,
    VaultPaths,
};

use crate::mcp_assistant::{
    prompt_files_fingerprint, resource_files_fingerprint, tool_catalog_fingerprint,
};
use crate::mcp_catalog::{active_tool_names, McpToolPack, McpToolPackMode};
use crate::mcp_discovery::McpDiscovery;
use crate::mcp_protocol::{McpListSnapshot, McpMethodError, McpToolResourceStore};
use crate::mcp_tool_exec::McpToolExecution;
use crate::tools::CustomToolRegistryOptions;

/// Who is on the other end of a session, as far as Vulcan can tell. The
/// client name and version are self-reported in `initialize`; the OAuth
/// client and subject come from an authenticated remote grant.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct McpClientAttribution {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl McpClientAttribution {
    /// Record `clientInfo` from an `initialize` request.
    pub fn record_client_info(&mut self, params: Option<&Value>) {
        let info = params.and_then(|params| params.get("clientInfo"));
        let field = |name: &str| {
            info.and_then(|info| info.get(name))
                .and_then(Value::as_str)
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .map(|text| text.chars().take(120).collect::<String>())
                .filter(|text| !text.is_empty())
        };
        self.name = field("name");
        self.version = field("version");
    }

    /// Commit trailers naming the client and the profile it acted under.
    #[must_use]
    pub fn trailers(&self, profile: &str) -> Vec<(String, String)> {
        let mut trailers = Vec::new();
        if let Some(name) = &self.name {
            let client = match &self.version {
                Some(version) => format!("{name} {version}"),
                None => name.clone(),
            };
            trailers.push(("Vulcan-MCP-Client".to_string(), client));
        }
        if let Some(id) = &self.oauth_client_id {
            trailers.push(("Vulcan-MCP-Client-Id".to_string(), id.clone()));
        }
        if let Some(subject) = &self.subject {
            trailers.push(("Vulcan-MCP-Subject".to_string(), subject.clone()));
        }
        trailers.push(("Vulcan-MCP-Profile".to_string(), profile.to_string()));
        trailers
    }
}

#[derive(Debug, Clone)]
pub struct McpSessionState {
    paths: VaultPaths,
    selection: ResolvedPermissionProfile,
    guard: ProfilePermissionGuard,
    pack_mode: McpToolPackMode,
    pinned_packs: BTreeSet<McpToolPack>,
    selected_packs: BTreeSet<McpToolPack>,
    resources: McpToolResourceStore,
    snapshot: McpListSnapshot,
    client: McpClientAttribution,
    /// Vault-relative paths this session's writes changed and that no
    /// session-scoped commit has taken yet.
    written_paths: BTreeSet<String>,
}

impl McpSessionState {
    pub fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        selected_packs: BTreeSet<McpToolPack>,
        pack_mode: McpToolPackMode,
        registry_options: impl FnOnce() -> CustomToolRegistryOptions,
    ) -> Result<Self, PermissionError> {
        let selection = resolve_permission_profile(paths, requested_profile)?;
        let pinned_packs = selected_packs
            .iter()
            .copied()
            .filter(|pack| *pack != McpToolPack::ToolPacks)
            .collect();
        let guard = ProfilePermissionGuard::new(paths, selection.clone());
        let snapshot = McpListSnapshot {
            tools: tool_catalog_fingerprint(
                paths,
                Some(selection.name.as_str()),
                &selected_packs,
                &selection.profile,
                registry_options,
            ),
            prompts: prompt_files_fingerprint(paths, &guard),
            resources: resource_files_fingerprint(paths, &guard),
        };
        Ok(Self {
            paths: paths.clone(),
            selection,
            guard,
            pack_mode,
            pinned_packs,
            selected_packs,
            resources: McpToolResourceStore::default(),
            snapshot,
            client: McpClientAttribution::default(),
            written_paths: BTreeSet::new(),
        })
    }

    #[must_use]
    pub fn paths(&self) -> &VaultPaths {
        &self.paths
    }

    #[must_use]
    pub fn client(&self) -> &McpClientAttribution {
        &self.client
    }

    /// Record `clientInfo` from the session's `initialize` request.
    pub fn record_client_info(&mut self, params: Option<&Value>) {
        self.client.record_client_info(params);
    }

    /// Attribute the session to an authenticated remote client.
    pub fn set_authenticated_client(&mut self, client_id: Option<String>, subject: Option<String>) {
        self.client.oauth_client_id = client_id;
        self.client.subject = subject;
    }

    #[must_use]
    pub fn written_paths(&self) -> &BTreeSet<String> {
        &self.written_paths
    }

    #[must_use]
    pub fn selection(&self) -> &ResolvedPermissionProfile {
        &self.selection
    }

    #[must_use]
    pub fn guard(&self) -> &ProfilePermissionGuard {
        &self.guard
    }

    #[must_use]
    pub fn active_tool_names(&self) -> Vec<String> {
        active_tool_names(&self.selected_packs, &self.selection.profile)
    }

    /// Existing sessions can narrow to current profile policy, never widen.
    pub fn attenuate_profile(&mut self) -> Result<(), String> {
        let current = resolve_permission_profile(&self.paths, Some(&self.selection.name))
            .map_err(|error| error.to_string())?;
        if !current.grant.is_subset_of(&self.selection.grant) {
            return Err("MCP permission profile widened after session initialization".to_string());
        }
        self.guard = ProfilePermissionGuard::new(&self.paths, current.clone());
        self.selection = current;
        Ok(())
    }

    #[must_use]
    pub fn discovery<'a>(&'a self, registry: &'a CustomToolRegistryOptions) -> McpDiscovery<'a> {
        McpDiscovery {
            paths: &self.paths,
            guard: &self.guard,
            profile_name: self.selection.name.as_str(),
            profile: &self.selection.profile,
            selected_packs: &self.selected_packs,
            resources: &self.resources,
            custom_registry: registry,
        }
    }

    pub fn call_tool(
        &mut self,
        registry: &CustomToolRegistryOptions,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        McpToolExecution {
            paths: &self.paths,
            guard: &self.guard,
            profile_name: self.selection.name.as_str(),
            profile: &self.selection.profile,
            selected_packs: &mut self.selected_packs,
            pinned_packs: &self.pinned_packs,
            pack_mode: self.pack_mode,
            resources: &mut self.resources,
            custom_registry: registry,
            client: &self.client,
            written_paths: &mut self.written_paths,
        }
        .call_tool(name, arguments)
    }

    pub fn list_changed_notifications(
        &mut self,
        registry_options: impl FnOnce() -> CustomToolRegistryOptions,
    ) -> Vec<Value> {
        let current = McpListSnapshot {
            tools: tool_catalog_fingerprint(
                &self.paths,
                Some(self.selection.name.as_str()),
                &self.selected_packs,
                &self.selection.profile,
                registry_options,
            ),
            prompts: prompt_files_fingerprint(&self.paths, &self.guard),
            resources: resource_files_fingerprint(&self.paths, &self.guard),
        };
        self.snapshot.changed_notifications(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_keeps_packs_and_result_resources_private() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let packs = BTreeSet::from([McpToolPack::NotesRead, McpToolPack::ToolPacks]);
        let mut first = McpSessionState::new(
            &paths,
            Some("readonly"),
            packs.clone(),
            McpToolPackMode::Adaptive,
            CustomToolRegistryOptions::default,
        )
        .unwrap();
        let mut second = McpSessionState::new(
            &paths,
            Some("readonly"),
            packs,
            McpToolPackMode::Adaptive,
            CustomToolRegistryOptions::default,
        )
        .unwrap();
        assert!(first.active_tool_names().contains(&"note_get".to_string()));
        assert_eq!(first.selection().name, "readonly");
        assert!(first.attenuate_profile().is_ok());
        let registry = CustomToolRegistryOptions::default();
        let state = first
            .call_tool(
                &registry,
                "tool_packs",
                &Map::from_iter([
                    ("operation".to_string(), Value::String("enable".to_string())),
                    ("packs".to_string(), serde_json::json!(["tasks"])),
                ]),
            )
            .unwrap();
        assert_eq!(state["isError"], false);
        assert!(first.active_tool_names().contains(&"task_list".to_string()));
        assert!(!second
            .active_tool_names()
            .contains(&"task_list".to_string()));
        let uri = first.resources.store_json("test", "{\"private\":true}")["uri"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(first
            .discovery(&registry)
            .read_resource(&uri, |_| Err("unknown".to_string()))
            .is_ok());
        assert!(matches!(
            second
                .discovery(&registry)
                .read_resource(&uri, |_| Err("unknown".to_string())),
            Err(McpMethodError::JsonRpc {
                code: crate::mcp_protocol::MCP_RESOURCE_NOT_FOUND,
                ..
            })
        ));
        assert!(second
            .list_changed_notifications(CustomToolRegistryOptions::default)
            .is_empty());
    }
}
