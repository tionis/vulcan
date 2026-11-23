//! Transport-neutral MCP session authority, pack state, and discovery snapshots.

use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use vulcan_core::{
    resolve_permission_profile, PermissionError, ProfilePermissionGuard, ResolvedPermissionProfile,
    VaultPaths,
};

use crate::mcp_assistant::{
    prompt_files_fingerprint, resource_files_fingerprint, tool_catalog_fingerprint,
};
use crate::mcp_catalog::{active_tool_names, McpToolPack, McpToolPackMode};
use crate::mcp_changesets::{McpChangesetBinding, McpChangesetSwitch};
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
    /// The vault, the attached changeset, and the writes each tree's
    /// session state left uncommitted while the session worked elsewhere.
    changeset: McpChangesetBinding,
    parked_written_paths: BTreeMap<Option<String>, BTreeSet<String>>,
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
            changeset: McpChangesetBinding::new(paths),
            parked_written_paths: BTreeMap::new(),
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
        // The vault's configuration decides, never a changeset's copy of it,
        // which the session itself may have edited.
        let current = resolve_permission_profile(&self.changeset.vault, Some(&self.selection.name))
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
        let result = McpToolExecution {
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
            changeset: &mut self.changeset,
        }
        .call_tool(name, arguments);
        if let Some(switch) = self.changeset.pending.take() {
            self.apply_changeset_switch(switch);
        }
        result
    }

    /// The changeset this session works in, if any.
    #[must_use]
    pub fn active_changeset(&self) -> Option<&str> {
        self.changeset.active.as_deref()
    }

    /// Point the session's tools at a changeset worktree or back at the
    /// vault, keeping the same permission selection; each tree keeps its own
    /// record of this session's uncommitted writes.
    fn apply_changeset_switch(&mut self, switch: McpChangesetSwitch) {
        let (active, paths) = match switch {
            McpChangesetSwitch::Attach { id, worktree } => (Some(id), VaultPaths::new(worktree)),
            McpChangesetSwitch::Detach => (None, self.changeset.vault.clone()),
        };
        let previous = std::mem::replace(&mut self.changeset.active, active.clone());
        let written = std::mem::take(&mut self.written_paths);
        if !written.is_empty() {
            self.parked_written_paths.insert(previous, written);
        }
        self.written_paths = self
            .parked_written_paths
            .remove(&active)
            .unwrap_or_default();
        self.guard = ProfilePermissionGuard::new(&paths, self.selection.clone());
        self.paths = paths;
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

    #[test]
    fn a_session_stages_work_in_a_changeset_and_lands_it() {
        let _lock = crate::trust::test_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", state.path());
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let git = |dir: &std::path::Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
            String::from_utf8(output.stdout).unwrap()
        };
        git(root, &["init", "--quiet", "-b", "main"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.com"]);
        let paths = VaultPaths::new(root);
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        std::fs::write(root.join("Home.md"), "# Home\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "--quiet", "-m", "seed"]);
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();

        let mut session = McpSessionState::new(
            &paths,
            Some("unrestricted"),
            BTreeSet::from([McpToolPack::Git, McpToolPack::NotesWrite]),
            McpToolPackMode::Static,
            CustomToolRegistryOptions::default,
        )
        .unwrap();
        let registry = CustomToolRegistryOptions::default();
        let call = |session: &mut McpSessionState, name: &str, arguments: Value| {
            let result = session
                .call_tool(&registry, name, arguments.as_object().unwrap())
                .unwrap();
            assert_eq!(result["isError"], false, "{name}: {result}");
            result["structuredContent"].clone()
        };

        let begun = call(
            &mut session,
            "changeset_begin",
            serde_json::json!({"title": "Grow home"}),
        );
        let id = begun["id"].as_str().unwrap().to_string();
        assert_eq!(session.active_changeset(), Some(id.as_str()));
        assert_ne!(session.paths().vault_root(), root);

        // Writes go to the worktree; the vault does not change.
        let appended = call(
            &mut session,
            "note_append",
            serde_json::json!({"note": "Home.md", "text": "Staged."}),
        );
        assert_eq!(appended["commit"]["status"], "uncommitted", "{appended}");
        assert_eq!(
            std::fs::read_to_string(root.join("Home.md")).unwrap(),
            "# Home\n"
        );
        let listed = call(&mut session, "changeset_list", serde_json::json!({}));
        assert_eq!(listed["attached"], id.as_str());
        assert_eq!(listed["changesets"][0]["uncommitted"][0], "Home.md");

        // Back on the vault the session sees the vault; it can return.
        call(&mut session, "changeset_switch", serde_json::json!({}));
        assert_eq!(session.paths().vault_root(), root);
        assert!(session.written_paths().is_empty());
        call(
            &mut session,
            "changeset_switch",
            serde_json::json!({"id": id}),
        );
        assert!(session.written_paths().contains("Home.md"));

        let landed = call(
            &mut session,
            "changeset_land",
            serde_json::json!({"message": "Grow home"}),
        );
        assert_eq!(landed["landed"], true, "{landed}");
        assert_eq!(landed["attached"], Value::Null);
        assert_eq!(session.active_changeset(), None);
        assert_eq!(session.paths().vault_root(), root);
        assert!(std::fs::read_to_string(root.join("Home.md"))
            .unwrap()
            .contains("Staged."));
        assert!(
            git(root, &["log", "-1", "--format=%B"]).contains("Vulcan-MCP-Profile: unrestricted")
        );

        match previous {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}
