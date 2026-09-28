//! Transport-neutral MCP tool execution over shared application workflows.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use vulcan_core::{PermissionProfile, ProfilePermissionGuard, VaultPaths};

use crate::mcp_catalog::{
    active_tool_names, authorize_builtin_tool_call, mutate_tool_packs, require_adaptive_pack_mode,
    tool_pack_state, McpToolId, McpToolPack, McpToolPackMode,
};
use crate::mcp_protocol::{
    self, McpConfigSetArgs, McpConfigShowArgs, McpDailyArgs, McpDailyListArgs, McpDailyShowArgs,
    McpGraphCommunitiesArgs, McpIndexScanArgs, McpMethodError, McpNoteAppendArgs,
    McpNoteCreateArgs, McpNoteDeleteArgs, McpNoteGetArgs, McpNoteInfoArgs, McpNoteOutlineArgs,
    McpNotePatchArgs, McpNoteSetArgs, McpQueryArgs, McpSearchArgs, McpSuggestLinksArgs,
    McpSyncConflictsArgs, McpSyncDoctorArgs, McpSyncTargetArgs, McpTaskCompleteArgs,
    McpTaskCreateArgs, McpTaskListArgs, McpTaskQueryArgs, McpTaskRescheduleArgs,
    McpToolPackMutationArgs, McpToolResourceStore, McpWebFetchArgs, McpWebSearchArgs,
    MCP_INLINE_TEXT_LIMIT, MCP_QUERY_DEFAULT_LIMIT, MCP_STRUCTURED_CONTENT_LIMIT,
};
use crate::mcp_read_tools::MCP_QUERY_HARD_MAX;
use crate::tools::CustomToolRegistryOptions;
use crate::{
    browse, mcp_config, mcp_custom, mcp_graph, mcp_notes, mcp_read_tools, mcp_scan, mcp_sync,
    mcp_tasks, mcp_web,
};

/// Session-local state and host-supplied registry policy needed by one tool call.
pub struct McpToolExecution<'a> {
    pub paths: &'a VaultPaths,
    pub guard: &'a ProfilePermissionGuard,
    pub profile_name: &'a str,
    pub profile: &'a PermissionProfile,
    pub selected_packs: &'a mut BTreeSet<McpToolPack>,
    pub pinned_packs: &'a BTreeSet<McpToolPack>,
    pub pack_mode: McpToolPackMode,
    pub resources: &'a mut McpToolResourceStore,
    pub custom_registry: &'a CustomToolRegistryOptions,
}

impl McpToolExecution<'_> {
    #[allow(clippy::too_many_lines)]
    pub fn call_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        let Some(authorized) = authorize_builtin_tool_call(
            name,
            arguments,
            self.selected_packs,
            self.profile,
            self.profile_name,
        )?
        else {
            let report = mcp_custom::call_custom_tool(
                self.paths,
                self.profile_name,
                self.selected_packs,
                self.custom_registry,
                name,
                arguments,
            )?;
            return Ok(self.resources.custom_success_response(
                &report.name,
                report.result,
                report.text.as_deref(),
            ));
        };
        let tool = authorized.tool;
        let arguments = authorized.arguments.as_ref();
        match tool.id {
            McpToolId::NoteGet => {
                let args: McpNoteGetArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_read_tools::note_get(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::NoteOutline => {
                let args: McpNoteOutlineArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_read_tools::note_outline(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::Search => {
                let args: McpSearchArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_read_tools::search(self.paths, self.guard, args)?,
                )
            }
            McpToolId::Query => {
                let args: McpQueryArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_read_tools::query(self.paths, self.guard, args)?,
                )
            }
            McpToolId::Status => {
                let report = browse::build_vault_status_report(self.paths)
                    .map_err(|error| McpMethodError::tool(error.to_string()))?;
                self.report(tool.name, &report)
            }
            McpToolId::Capabilities => {
                let names = active_tool_names(self.selected_packs, self.profile);
                let state = tool_pack_state(
                    self.selected_packs,
                    self.pinned_packs,
                    self.pack_mode,
                    self.profile,
                );
                self.value(
                    tool.name,
                    json!({
                        "routing": mcp_protocol::routing_guidance(&names),
                        "activeTools": names,
                        "toolPacks": state,
                        "resultLimits": {
                            "inlineTextBytes": MCP_INLINE_TEXT_LIMIT,
                            "structuredContentBytes": MCP_STRUCTURED_CONTENT_LIMIT,
                            "queryDefaultRows": MCP_QUERY_DEFAULT_LIMIT,
                            "queryMaximumRows": MCP_QUERY_HARD_MAX,
                        },
                    }),
                )
            }
            McpToolId::SyncStatus | McpToolId::SyncPlan => {
                let args: McpSyncTargetArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_sync::sync_preview(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::SyncDoctor => {
                let args: McpSyncDoctorArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_sync::sync_doctor(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::SyncConflicts => {
                let args: McpSyncConflictsArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_sync::sync_conflicts(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::Daily => {
                let args: McpDailyArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_read_tools::daily(self.paths, self.guard, args)?,
                )
            }
            McpToolId::DailyShow => {
                let args: McpDailyShowArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_read_tools::daily_show(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::DailyList => {
                let args: McpDailyListArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_read_tools::daily_list(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::GraphCommunities => {
                let args: McpGraphCommunitiesArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_graph::graph_communities(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::SuggestLinks => {
                let args: McpSuggestLinksArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_graph::link_suggestions(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::TaskList => {
                let args: McpTaskListArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_read_tools::task_list(self.paths, self.guard, args)?,
                )
            }
            McpToolId::TaskQuery => {
                let args: McpTaskQueryArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_read_tools::task_query(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::TaskCreate => {
                let args: McpTaskCreateArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_tasks::task_create(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::TaskComplete => {
                let args: McpTaskCompleteArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_tasks::task_complete(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::TaskReschedule => {
                let args: McpTaskRescheduleArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_tasks::task_reschedule(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::NoteCreate => {
                let args: McpNoteCreateArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_notes::note_create(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::NoteAppend => {
                let args: McpNoteAppendArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_notes::note_append(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::NotePatch => {
                let args: McpNotePatchArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_notes::note_patch(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::NoteInfo => {
                let args: McpNoteInfoArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_read_tools::note_info(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::NoteSet => {
                let args: McpNoteSetArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_notes::note_set(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::NoteDelete => {
                let args: McpNoteDeleteArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_notes::note_delete(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::WebSearch => {
                let args: McpWebSearchArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_web::web_search(self.paths, self.guard, args)?,
                )
            }
            McpToolId::WebFetch => {
                let args: McpWebFetchArgs = parse(arguments)?;
                self.value(tool.name, mcp_web::web_fetch(self.paths, self.guard, args)?)
            }
            McpToolId::ConfigShow => {
                let args: McpConfigShowArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_config::config_show(self.paths, self.guard, self.profile_name, &args)?,
                )
            }
            McpToolId::ConfigSet => {
                let args: McpConfigSetArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_config::config_set(self.paths, self.guard, self.profile_name, &args)?,
                )
            }
            McpToolId::IndexScan => {
                let args: McpIndexScanArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_scan::index_scan(self.paths, self.guard, self.profile_name, &args)?,
                )
            }
            McpToolId::ToolPacks => {
                require_adaptive_pack_mode(self.pack_mode)?;
                let args: McpToolPackMutationArgs = parse(arguments)?;
                let state = mutate_tool_packs(
                    self.selected_packs,
                    self.pinned_packs,
                    self.pack_mode,
                    self.profile,
                    &args,
                )?;
                self.value(tool.name, state)
            }
        }
    }

    fn report<T: Serialize>(
        &mut self,
        tool_name: &str,
        report: &T,
    ) -> Result<Value, McpMethodError> {
        let value = serde_json::to_value(report).map_err(|error| {
            McpMethodError::internal(format!("failed to serialize `{tool_name}` report: {error}"))
        })?;
        self.value(tool_name, value)
    }

    #[allow(clippy::unnecessary_wraps)] // Keeps infallible response branches uniform with fallible workflow arms.
    fn value(&mut self, tool_name: &str, value: Value) -> Result<Value, McpMethodError> {
        Ok(self.resources.success_response(tool_name, value))
    }
}

fn parse<T: DeserializeOwned>(arguments: &Map<String, Value>) -> Result<T, McpMethodError> {
    serde_json::from_value(Value::Object(arguments.clone()))
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_catalog::resolve_selected_tool_packs;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn shared_executor_preserves_session_pack_state_and_readonly_denial() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let selection = resolve_permission_profile(&paths, Some("readonly")).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let mut selected = resolve_selected_tool_packs(&[], McpToolPackMode::Adaptive);
        let pinned = BTreeSet::from([
            McpToolPack::NotesRead,
            McpToolPack::Search,
            McpToolPack::Status,
        ]);
        let mut resources = McpToolResourceStore::default();
        let registry = CustomToolRegistryOptions::default();
        let mut executor = McpToolExecution {
            paths: &paths,
            guard: &guard,
            profile_name: &selection.name,
            profile: &selection.profile,
            selected_packs: &mut selected,
            pinned_packs: &pinned,
            pack_mode: McpToolPackMode::Adaptive,
            resources: &mut resources,
            custom_registry: &registry,
        };
        let capabilities = executor.call_tool("capabilities", &Map::new()).unwrap();
        assert!(capabilities["structuredContent"]["activeTools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "note_get"));
        let enable = Map::from_iter([("packs".to_string(), json!(["notes-write"]))]);
        executor.call_tool("tool_pack_enable", &enable).unwrap();
        assert!(executor.selected_packs.contains(&McpToolPack::NotesWrite));
        assert!(matches!(
            executor.call_tool("note_create", &Map::new()),
            Err(McpMethodError::Tool { .. })
        ));
    }
}
