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
        })
    }

    #[must_use]
    pub fn paths(&self) -> &VaultPaths {
        &self.paths
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
