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
    McpTaskCreateArgs, McpTaskListArgs, McpTaskQueryArgs, McpTaskReminderAddArgs,
    McpTaskReminderRemoveArgs, McpTaskRescheduleArgs, McpToolPackMutationArgs,
    McpToolResourceStore, McpWebFetchArgs, McpWebSearchArgs, MCP_INLINE_TEXT_LIMIT,
    MCP_QUERY_DEFAULT_LIMIT, MCP_STRUCTURED_CONTENT_LIMIT,
};
use crate::mcp_read_tools::MCP_QUERY_HARD_MAX;
use crate::tools::CustomToolRegistryOptions;
use crate::{
    browse, mcp_config, mcp_custom, mcp_git, mcp_graph, mcp_notes, mcp_read_tools, mcp_scan,
    mcp_sync, mcp_tasks, mcp_web,
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
        // As the CLI does before each command: finish an interrupted mdbase
        // write this connection may touch, so it does not block the call. A
        // failure leaves it pending for the tool to report.
        let _ = crate::mdbase::recover_interrupted_mdbase_write_with_guard(self.paths, self.guard);
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
        let flag = |name: &str| arguments.get(name).and_then(Value::as_bool) == Some(true);
        let commit_probe = (reports_commit(tool.id) && !flag("dry_run")).then(|| {
            (
                crate::commit::head_revision(self.paths.vault_root()),
                flag("no_commit"),
            )
        });
        let structured = match tool.id {
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
            McpToolId::TaskReminderAdd => {
                let args: McpTaskReminderAddArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_tasks::task_reminder_add(self.paths, self.guard, self.profile_name, args)?,
                )
            }
            McpToolId::TaskReminderRemove => {
                let args: McpTaskReminderRemoveArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_tasks::task_reminder_remove(
                        self.paths,
                        self.guard,
                        self.profile_name,
                        args,
                    )?,
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
            McpToolId::SyncRefresh => {
                self.value(tool.name, mcp_sync::sync_refresh(self.paths, self.guard)?)
            }
            McpToolId::SyncResume => {
                self.value(tool.name, mcp_sync::sync_resume(self.paths, self.guard)?)
            }
            McpToolId::NoteMove => {
                let args: mcp_protocol::McpNoteMoveArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_notes::note_move(self.paths, self.guard, self.profile_name, &args)?,
                )
            }
            McpToolId::GitStatus => {
                self.value(tool.name, mcp_git::git_status(self.paths, self.guard)?)
            }
            McpToolId::GitDiff => {
                let args: mcp_protocol::McpGitDiffArgs = parse(arguments)?;
                self.value(tool.name, mcp_git::git_diff(self.paths, self.guard, &args)?)
            }
            McpToolId::GitLog => {
                let args: mcp_protocol::McpGitLogArgs = parse(arguments)?;
                self.value(tool.name, mcp_git::git_log(self.paths, self.guard, &args)?)
            }
            McpToolId::GitBlame => {
                let args: mcp_protocol::McpGitBlameArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_git::git_blame(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::GitCommit => {
                let args: mcp_protocol::McpGitCommitArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_git::git_commit(self.paths, self.guard, self.profile_name, &args)?,
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
        }?;
        let mut structured = wrap_structured(structured);
        if let (Some((before, no_commit)), Some(object)) =
            (commit_probe, structured.as_object_mut())
        {
            let outcome = crate::commit::commit_outcome(self.paths, before.as_deref(), no_commit);
            object.insert(
                "commit".to_string(),
                serde_json::to_value(outcome).unwrap_or(Value::Null),
            );
        }
        Ok(self.resources.success_response(tool.name, structured))
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

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)] // Keeps infallible response branches uniform with fallible workflow arms.
    fn value(&mut self, _tool_name: &str, value: Value) -> Result<Value, McpMethodError> {
        Ok(value)
    }
}

/// Tools that change vault files, whose results say whether and how the
/// change was committed.
const fn reports_commit(tool: McpToolId) -> bool {
    matches!(
        tool,
        McpToolId::NoteCreate
            | McpToolId::NoteAppend
            | McpToolId::NotePatch
            | McpToolId::NoteSet
            | McpToolId::NoteDelete
            | McpToolId::NoteMove
            | McpToolId::TaskCreate
            | McpToolId::TaskComplete
            | McpToolId::TaskReschedule
            | McpToolId::TaskReminderAdd
            | McpToolId::TaskReminderRemove
            | McpToolId::ConfigSet
    )
}

fn wrap_structured(structured: Value) -> Value {
    if structured.is_object() {
        structured
    } else {
        json!({ "result": structured })
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

    fn git(root: &std::path::Path, arguments: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(arguments)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    fn append(root: &std::path::Path, text: &str) -> Value {
        let paths = VaultPaths::new(root);
        let selection = resolve_permission_profile(&paths, Some("unrestricted")).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let mut selected = BTreeSet::from([McpToolPack::NotesWrite]);
        let pinned = BTreeSet::new();
        let mut resources = McpToolResourceStore::default();
        let registry = CustomToolRegistryOptions::default();
        let mut executor = McpToolExecution {
            paths: &paths,
            guard: &guard,
            profile_name: &selection.name,
            profile: &selection.profile,
            selected_packs: &mut selected,
            pinned_packs: &pinned,
            pack_mode: McpToolPackMode::Static,
            resources: &mut resources,
            custom_registry: &registry,
        };
        let arguments = Map::from_iter([
            ("note".to_string(), json!("Home.md")),
            ("text".to_string(), json!(text)),
        ]);
        executor.call_tool("note_append", &arguments).unwrap()["structuredContent"].clone()
    }

    fn call(
        root: &std::path::Path,
        profile: &str,
        packs: &[McpToolPack],
        name: &str,
        arguments: &Value,
    ) -> Result<Value, McpMethodError> {
        let paths = VaultPaths::new(root);
        let selection = resolve_permission_profile(&paths, Some(profile)).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let mut selected = packs.iter().copied().collect::<BTreeSet<_>>();
        let pinned = BTreeSet::new();
        let mut resources = McpToolResourceStore::default();
        let registry = CustomToolRegistryOptions::default();
        let mut executor = McpToolExecution {
            paths: &paths,
            guard: &guard,
            profile_name: &selection.name,
            profile: &selection.profile,
            selected_packs: &mut selected,
            pinned_packs: &pinned,
            pack_mode: McpToolPackMode::Static,
            resources: &mut resources,
            custom_registry: &registry,
        };
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        executor
            .call_tool(name, &arguments)
            .map(|result| result["structuredContent"].clone())
    }

    #[test]
    fn the_git_pack_inspects_and_commits_and_note_move_rewrites_links() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "--quiet"]);
        vulcan_core::paths::initialize_vulcan_dir(&VaultPaths::new(root)).unwrap();
        std::fs::write(root.join(".gitignore"), ".vulcan/\n").unwrap();
        std::fs::write(root.join("Home.md"), "See [[Inbox/Idea]].\n").unwrap();
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::write(root.join("Inbox/Idea.md"), "# Idea\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "--quiet", "-m", "seed"]);
        vulcan_core::scan_vault(&VaultPaths::new(root), vulcan_core::ScanMode::Full).unwrap();

        // A read-only connection sees no commit tool.
        let denied = call(
            root,
            "readonly",
            &[McpToolPack::Git],
            "git_commit",
            &json!({"message": "x"}),
        );
        assert!(
            matches!(denied, Err(McpMethodError::Tool { .. })),
            "{denied:?}"
        );

        let moved = call(
            root,
            "unrestricted",
            &[McpToolPack::NotesManage],
            "note_move",
            &json!({"note": "Inbox/Idea", "destination": "Projects/Idea", "no_commit": true}),
        )
        .unwrap();
        assert_eq!(moved["destination_path"], "Projects/Idea.md", "{moved}");
        assert!(root.join("Projects/Idea.md").exists());
        let home = std::fs::read_to_string(root.join("Home.md")).unwrap();
        assert!(!home.contains("Inbox/Idea"), "{home}");
        assert_eq!(
            moved["rewritten_files"].as_array().unwrap().len(),
            1,
            "{moved}"
        );

        let git_pack = [McpToolPack::Git];
        let status = call(root, "unrestricted", &git_pack, "git_status", &json!({})).unwrap();
        assert_eq!(status["clean"], false, "{status}");
        let diff = call(
            root,
            "unrestricted",
            &git_pack,
            "git_diff",
            &json!({"path": "Home.md"}),
        )
        .unwrap();
        assert!(
            diff["diff"]
                .as_str()
                .unwrap()
                .contains("-See [[Inbox/Idea]]"),
            "{diff}"
        );
        let committed = call(
            root,
            "unrestricted",
            &git_pack,
            "git_commit",
            &json!({"message": "Move the idea"}),
        )
        .unwrap();
        assert_eq!(committed["committed"], true, "{committed}");
        let log = call(
            root,
            "unrestricted",
            &git_pack,
            "git_log",
            &json!({"limit": 1}),
        )
        .unwrap();
        assert_eq!(log["entries"].as_array().unwrap().len(), 1, "{log}");
        let status = call(root, "unrestricted", &git_pack, "git_status", &json!({})).unwrap();
        assert_eq!(status["clean"], true, "{status}");
        let blame = call(
            root,
            "unrestricted",
            &git_pack,
            "git_blame",
            &json!({"path": "Home.md"}),
        )
        .unwrap();
        assert_eq!(blame["lines"].as_array().unwrap().len(), 1, "{blame}");
    }

    #[test]
    fn writes_say_whether_and_how_their_change_was_committed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "--quiet"]);
        std::fs::write(root.join("Home.md"), "# Home\n").unwrap();
        git(root, &["add", "Home.md"]);
        git(root, &["commit", "--quiet", "-m", "home"]);

        let uncommitted = append(root, "first");
        assert_eq!(
            uncommitted["commit"]["status"], "uncommitted",
            "{uncommitted}"
        );

        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::write(
            root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\nstrategy = \"batched\"\n",
        )
        .unwrap();
        assert_eq!(append(root, "second")["commit"]["status"], "batched");

        std::fs::write(
            root.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\n",
        )
        .unwrap();
        std::fs::write(root.join(".gitignore"), ".vulcan/\n").unwrap();
        let committed = append(root, "third");
        assert_eq!(committed["commit"]["status"], "committed", "{committed}");
        assert_eq!(
            committed["commit"]["sha"].as_str(),
            crate::commit::head_revision(root).as_deref()
        );
    }
}
