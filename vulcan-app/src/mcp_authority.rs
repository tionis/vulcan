//! Who an MCP request acts as: the vault, the permission profile, and the
//! client. No connection state: the stateless core resolves this per request.

use serde_json::{Map, Value};
use vulcan_core::{
    resolve_permission_profile, PermissionError, ProfilePermissionGuard, ResolvedPermissionProfile,
    VaultPaths,
};

use crate::mcp_catalog::active_tool_names;
use crate::mcp_discovery::McpDiscovery;
use crate::mcp_operations::McpOperationLookup;
use crate::mcp_protocol::McpMethodError;
use crate::mcp_tool_exec::McpToolExecution;

/// Who is on the other end, as far as Vulcan can tell. The client name and
/// version are self-reported (per request in `_meta`, or in a legacy
/// `initialize`); the OAuth client and subject come from an authenticated
/// remote grant.
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
        self.record_implementation(params.and_then(|params| params.get("clientInfo")));
    }

    /// Record a client `Implementation` object (`name`, `version`).
    pub fn record_implementation(&mut self, info: Option<&Value>) {
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

/// The vault and permission profile a request acts under.
#[derive(Debug, Clone)]
pub struct McpAuthority {
    paths: VaultPaths,
    selection: ResolvedPermissionProfile,
    guard: ProfilePermissionGuard,
}

impl McpAuthority {
    pub fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
    ) -> Result<Self, PermissionError> {
        let selection = resolve_permission_profile(paths, requested_profile)?;
        let guard = ProfilePermissionGuard::new(paths, selection.clone());
        Ok(Self {
            paths: paths.clone(),
            selection,
            guard,
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
        active_tool_names(&self.selection.profile)
    }

    /// Re-read the profile from the vault's configuration: it may narrow
    /// what the caller was granted, never widen it.
    pub fn attenuate_profile(&mut self) -> Result<(), String> {
        let current = resolve_permission_profile(&self.paths, Some(&self.selection.name))
            .map_err(|error| error.to_string())?;
        if !current.grant.is_subset_of(&self.selection.grant) {
            return Err("MCP permission profile widened after it was granted".to_string());
        }
        self.guard = ProfilePermissionGuard::new(&self.paths, current.clone());
        self.selection = current;
        Ok(())
    }

    #[must_use]
    pub fn discovery(&self) -> McpDiscovery<'_> {
        McpDiscovery {
            paths: &self.paths,
            guard: &self.guard,
            profile_name: self.selection.name.as_str(),
            profile: &self.selection.profile,
            operations: false,
        }
    }

    pub fn call_tool(
        &self,
        client: &McpClientAttribution,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        self.call_tool_with(client, None, None, name, arguments)
    }

    /// Call a tool where the transport keeps operation records.
    pub fn call_tool_with(
        &self,
        client: &McpClientAttribution,
        operations: Option<&dyn McpOperationLookup>,
        progress: Option<&crate::mcp_progress::McpProgress>,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        McpToolExecution {
            paths: &self.paths,
            guard: &self.guard,
            profile_name: self.selection.name.as_str(),
            profile: &self.selection.profile,
            client,
            operations,
            progress,
        }
        .call_tool(name, arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_lists_the_profile_tools_and_keeps_no_result_store() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let mut first = McpAuthority::new(&paths, Some("readonly")).unwrap();
        assert!(first.active_tool_names().contains(&"note_get".to_string()));
        assert!(first
            .active_tool_names()
            .contains(&"task_query".to_string()));
        assert!(!first
            .active_tool_names()
            .contains(&"note_create".to_string()));
        assert_eq!(first.selection().name, "readonly");
        assert!(first.attenuate_profile().is_ok());
        assert!(matches!(
            first
                .discovery()
                .read_resource("vulcan://tool-results/1.json", |_| Err(
                    "unknown".to_string()
                )),
            Err(McpMethodError::JsonRpc {
                code: crate::mcp_protocol::MCP_RESOURCE_NOT_FOUND,
                ..
            })
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn tools_work_in_a_changeset_named_per_call_and_land_it() {
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

        let session = McpAuthority::new(&paths, Some("unrestricted")).unwrap();
        let call = |session: &McpAuthority, name: &str, arguments: Value| {
            let result = session
                .call_tool(
                    &McpClientAttribution::default(),
                    name,
                    arguments.as_object().unwrap(),
                )
                .unwrap();
            assert_eq!(result["isError"], false, "{name}: {result}");
            result["structuredContent"].clone()
        };

        let begun = call(
            &session,
            "changeset_begin",
            serde_json::json!({"title": "Grow home"}),
        );
        let id = begun["id"].as_str().unwrap().to_string();

        // Writes naming the changeset go to its worktree; the vault does not
        // change, and the vault's own reads do not see them.
        let appended = call(
            &session,
            "note_append",
            serde_json::json!({"note": "Home.md", "text": "Staged.", "changeset": id}),
        );
        assert_eq!(appended["commit"]["status"], "uncommitted", "{appended}");
        assert_eq!(
            std::fs::read_to_string(root.join("Home.md")).unwrap(),
            "# Home\n"
        );
        let staged = call(
            &session,
            "note_get",
            serde_json::json!({"note": "Home.md", "changeset": id}),
        );
        assert!(staged["content"].as_str().unwrap().contains("Staged."));
        let vault = call(&session, "note_get", serde_json::json!({"note": "Home.md"}));
        assert!(!vault["content"].as_str().unwrap().contains("Staged."));
        let listed = call(&session, "changeset_list", serde_json::json!({}));
        assert_eq!(listed["changesets"][0]["uncommitted"][0], "Home.md");

        // In a changeset, a bare commit takes everything in it; in the vault
        // it must name what it commits.
        call(
            &session,
            "git_commit",
            serde_json::json!({"message": "Stage", "changeset": id}),
        );
        let parameter = |name: &str, arguments: Value, session: &McpAuthority| {
            let error = session
                .call_tool(
                    &McpClientAttribution::default(),
                    name,
                    arguments.as_object().unwrap(),
                )
                .unwrap_err();
            let McpMethodError::Tool {
                structured: Some(structured),
                ..
            } = error
            else {
                panic!("{name}: {error:?}");
            };
            structured["error"].clone()
        };
        let bare = parameter("git_commit", serde_json::json!({"message": "x"}), &session);
        assert_eq!(bare["parameter"], "paths", "{bare}");

        let landed = call(&session, "changeset_land", serde_json::json!({"id": id}));
        assert_eq!(landed["landed"], true, "{landed}");
        assert!(std::fs::read_to_string(root.join("Home.md"))
            .unwrap()
            .contains("Staged."));
        assert!(
            git(root, &["log", "-1", "--format=%B"]).contains("Vulcan-MCP-Profile: unrestricted")
        );

        // A changeset that no longer exists is a correctable error naming
        // the argument.
        let gone = parameter(
            "note_get",
            serde_json::json!({"note": "Home.md", "changeset": id}),
            &session,
        );
        assert_eq!(gone["code"], "not_found");
        assert_eq!(gone["parameter"], "changeset");
        // Tools outside the changeset families reject the argument.
        let status = parameter("status", serde_json::json!({"changeset": id}), &session);
        assert_eq!(status["code"], "invalid_argument", "{status}");

        match previous {
            Some(value) => std::env::set_var("XDG_STATE_HOME", value),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}
