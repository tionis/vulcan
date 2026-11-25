//! Transport-neutral MCP tool execution over shared application workflows.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use vulcan_core::{PermissionProfile, ProfilePermissionGuard, VaultPaths};

use crate::mcp_catalog::{authorize_builtin_tool_call, McpToolId};
use crate::mcp_protocol::{
    self, McpConfigGetArgs, McpConfigSetArgs, McpDailyArgs, McpGraphCommunitiesArgs,
    McpIndexScanArgs, McpLinkSuggestionResolveArgs, McpLinkSuggestionsArgs, McpMethodError,
    McpNoteAppendArgs, McpNoteCreateArgs, McpNoteDeleteArgs, McpNoteGetArgs, McpNoteInfoArgs,
    McpNoteOutlineArgs, McpNotePatchArgs, McpNoteSetArgs, McpQueryArgs, McpSearchArgs,
    McpSyncConflictsArgs, McpSyncDoctorArgs, McpSyncTargetArgs, McpTaskCompleteArgs,
    McpTaskCreateArgs, McpTaskQueryArgs, McpTaskReminderAddArgs, McpTaskReminderRemoveArgs,
    McpTaskRescheduleArgs, McpWebFetchArgs, McpWebSearchArgs,
};
use crate::{
    browse, mcp_changesets, mcp_config, mcp_custom, mcp_files, mcp_git, mcp_graph, mcp_notes,
    mcp_read_tools, mcp_scan, mcp_sync, mcp_tasks, mcp_web,
};

/// Session-local state and host-supplied registry policy needed by one tool call.
pub struct McpToolExecution<'a> {
    pub paths: &'a VaultPaths,
    pub guard: &'a ProfilePermissionGuard,
    pub profile_name: &'a str,
    pub profile: &'a PermissionProfile,
    pub client: &'a crate::mcp_session::McpClientAttribution,
    /// Paths this session wrote and has not committed; `git_commit` takes
    /// these by default.
    pub written_paths: &'a mut BTreeSet<String>,
    /// The vault and the changeset this session works in.
    pub changeset: &'a mut crate::mcp_changesets::McpChangesetBinding,
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
        let Some(tool) = authorize_builtin_tool_call(name, self.profile, self.profile_name)? else {
            let report =
                mcp_custom::call_custom_tool(self.paths, self.profile_name, name, arguments)?;
            return Ok(crate::mcp_protocol::custom_tool_result(
                report.result,
                report.text.as_deref(),
            ));
        };
        let flag = |name: &str| arguments.get(name).and_then(Value::as_bool) == Some(true);
        let commit_probe = (reports_commit(tool.id) && !flag("dry_run")).then(|| {
            (
                crate::commit::head_revision(self.paths.vault_root()),
                flag("no_commit"),
            )
        });
        let (structured, written) = crate::commit::record_written_paths(|| match tool.id {
            McpToolId::NoteGet => {
                let args: McpNoteGetArgs = parse(arguments)?;
                let report = mcp_read_tools::note_get(self.paths, self.guard, &args)?;
                let mut value = serde_json::to_value(report)
                    .map_err(|error| McpMethodError::internal(error.to_string()))?;
                cap_note_content(&mut value);
                Ok(value)
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
                let mut status = serde_json::to_value(report)
                    .map_err(|error| McpMethodError::internal(error.to_string()))?;
                status["permissions"] = json!({
                    "profile": self.profile_name,
                    "grant": self.guard.selection().grant,
                });
                self.value(tool.name, status)
            }
            McpToolId::SyncStatus => {
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
            McpToolId::GraphCommunities => {
                let args: McpGraphCommunitiesArgs = parse(arguments)?;
                self.value(
                    tool.name,
                    mcp_graph::graph_communities(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::LinkSuggestions => {
                let args: McpLinkSuggestionsArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_graph::link_suggestions(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::LinkSuggestionResolve => {
                let args: McpLinkSuggestionResolveArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_graph::link_suggestion_resolve(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::TaskQuery => {
                let args: McpTaskQueryArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_read_tools::task_query(self.paths, self.guard, args)?,
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
            McpToolId::FileGet => {
                let args: mcp_protocol::McpFileGetArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_files::file_get(self.paths, self.guard, &args)?,
                )
            }
            McpToolId::FilePut => {
                let args: mcp_protocol::McpFilePutArgs = parse(arguments)?;
                self.report(
                    tool.name,
                    &mcp_files::file_put(self.paths, self.guard, self.profile_name, &args)?,
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
                let report = mcp_git::git_commit(
                    self.paths,
                    self.guard,
                    self.profile_name,
                    self.client,
                    self.written_paths,
                    &args,
                )?;
                self.value(tool.name, report)
            }
            McpToolId::ChangesetBegin => {
                let args: mcp_protocol::McpChangesetBeginArgs = parse(arguments)?;
                let report = mcp_changesets::changeset_begin(self.changeset, self.guard, &args)?;
                self.value(tool.name, report)
            }
            McpToolId::ChangesetSwitch => {
                let args: mcp_protocol::McpChangesetIdArgs = parse(arguments)?;
                let report = mcp_changesets::changeset_switch(self.changeset, self.guard, &args)?;
                self.value(tool.name, report)
            }
            McpToolId::ChangesetList => {
                let report = mcp_changesets::changeset_list(self.changeset, self.guard)?;
                self.value(tool.name, report)
            }
            McpToolId::ChangesetLand => {
                let args: mcp_protocol::McpChangesetLandArgs = parse(arguments)?;
                let report = mcp_changesets::changeset_land(
                    self.changeset,
                    self.guard,
                    self.profile_name,
                    self.client,
                    &args,
                )?;
                self.value(tool.name, report)
            }
            McpToolId::ChangesetAbandon => {
                let args: mcp_protocol::McpChangesetIdArgs = parse(arguments)?;
                let report = mcp_changesets::changeset_abandon(self.changeset, self.guard, &args)?;
                self.value(tool.name, report)
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
            McpToolId::ConfigGet => {
                let args: McpConfigGetArgs = parse(arguments)?;
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
        });
        // Arguments the handler could not use are the model's to correct: a
        // tool error, never a protocol error.
        let structured = structured.map_err(|error| match error {
            McpMethodError::JsonRpc {
                code: -32602,
                message,
                ..
            } => McpMethodError::invalid_argument(message, None),
            other => other,
        })?;
        crate::write_attribution::record_batched_writes(
            self.paths,
            &written,
            &self.client.trailers(self.profile_name),
        );
        self.written_paths.extend(written);
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
        let note_link = (tool.id == McpToolId::NoteGet)
            .then(|| structured.get("content_resource").cloned())
            .flatten();
        let mut result = crate::mcp_protocol::tool_result(structured);
        if let Some(Value::String(uri)) = note_link {
            if let Some(content) = result["content"].as_array_mut() {
                content.push(json!({
                    "type": "resource_link",
                    "uri": uri,
                    "name": uri.trim_start_matches("vulcan://note/"),
                    "mimeType": "text/markdown",
                    "description": "The whole note; this result holds only its beginning.",
                }));
            }
        }
        Ok(result)
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
            | McpToolId::FilePut
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

/// Keep a long note's content inline only up to the cap, at a line break,
/// and point at the note's resource for the rest.
fn cap_note_content(value: &mut Value) {
    let limit = crate::mcp_protocol::MCP_CONTENT_INLINE_LIMIT;
    let Some(content) = value.get("content").and_then(Value::as_str) else {
        return;
    };
    if content.len() <= limit {
        return;
    }
    let mut end = limit;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let end = content[..end].rfind('\n').map_or(end, |line| line + 1);
    let head = content[..end].to_string();
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    value["content"] = Value::String(head);
    value["truncated"] = Value::Bool(true);
    value["content_resource"] =
        Value::String(crate::mcp_files::vault_path_uri("vulcan://note/", &path));
}

/// Decode tool arguments; a failure is an `invalid_argument` tool error that
/// names the argument at fault.
fn parse<T: DeserializeOwned>(arguments: &Map<String, Value>) -> Result<T, McpMethodError> {
    serde_path_to_error::deserialize(Value::Object(arguments.clone())).map_err(|error| {
        let path = error.path().to_string();
        let inner = error.into_inner().to_string();
        // Serde reports positions within the argument object, which mean
        // nothing to a caller; drop them.
        let inner = inner
            .split(" at line ")
            .next()
            .unwrap_or(&inner)
            .to_string();
        let missing = inner
            .strip_prefix("missing field `")
            .and_then(|rest| rest.split('`').next())
            .map(ToString::to_string);
        let unknown = inner
            .strip_prefix("unknown field `")
            .and_then(|rest| rest.split('`').next())
            .map(ToString::to_string);
        let parameter = missing
            .or(unknown)
            .or_else(|| (path != ".").then(|| path.clone()));
        let message = match &parameter {
            Some(name) if !inner.contains(&format!("`{name}`")) => {
                format!("argument `{name}`: {inner}")
            }
            _ => inner,
        };
        McpMethodError::invalid_argument(message, parameter)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn shared_executor_lists_profile_tools_and_denies_readonly_writes() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();
        let selection = resolve_permission_profile(&paths, Some("readonly")).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let mut executor = McpToolExecution {
            paths: &paths,
            guard: &guard,
            profile_name: &selection.name,
            profile: &selection.profile,
            client: &crate::mcp_session::McpClientAttribution::default(),
            written_paths: &mut BTreeSet::new(),
            changeset: &mut crate::mcp_changesets::McpChangesetBinding::new(&paths),
        };
        let status = executor.call_tool("status", &Map::new()).unwrap();
        assert_eq!(
            status["structuredContent"]["permissions"]["profile"],
            "readonly"
        );
        assert_eq!(
            status["structuredContent"]["permissions"]["grant"]["destructive"],
            false
        );
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

    fn git_stdout(root: &std::path::Path, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(arguments)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {arguments:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn append(root: &std::path::Path, text: &str) -> Value {
        let paths = VaultPaths::new(root);
        let selection = resolve_permission_profile(&paths, Some("unrestricted")).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let mut executor = McpToolExecution {
            paths: &paths,
            guard: &guard,
            profile_name: &selection.name,
            profile: &selection.profile,
            client: &crate::mcp_session::McpClientAttribution::default(),
            written_paths: &mut BTreeSet::new(),
            changeset: &mut crate::mcp_changesets::McpChangesetBinding::new(&paths),
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
        name: &str,
        arguments: &Value,
    ) -> Result<Value, McpMethodError> {
        let paths = VaultPaths::new(root);
        let selection = resolve_permission_profile(&paths, Some(profile)).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let mut executor = McpToolExecution {
            paths: &paths,
            guard: &guard,
            profile_name: &selection.name,
            profile: &selection.profile,
            client: &crate::mcp_session::McpClientAttribution::default(),
            written_paths: &mut BTreeSet::new(),
            changeset: &mut crate::mcp_changesets::McpChangesetBinding::new(&paths),
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
        let denied = call(root, "readonly", "git_commit", &json!({"message": "x"}));
        assert!(
            matches!(denied, Err(McpMethodError::Tool { .. })),
            "{denied:?}"
        );

        let moved = call(
            root,
            "unrestricted",
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

        let status = call(root, "unrestricted", "git_status", &json!({})).unwrap();
        assert_eq!(status["clean"], false, "{status}");
        let diff = call(
            root,
            "unrestricted",
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
            "git_commit",
            &json!({"message": "Move the idea", "scope": "all"}),
        )
        .unwrap();
        assert_eq!(committed["committed"], true, "{committed}");
        let log = call(root, "unrestricted", "git_log", &json!({"limit": 1})).unwrap();
        assert_eq!(log["entries"].as_array().unwrap().len(), 1, "{log}");
        let status = call(root, "unrestricted", "git_status", &json!({})).unwrap();
        assert_eq!(status["clean"], true, "{status}");
        let blame = call(
            root,
            "unrestricted",
            "git_blame",
            &json!({"path": "Home.md"}),
        )
        .unwrap();
        assert_eq!(blame["lines"].as_array().unwrap().len(), 1, "{blame}");
    }

    #[test]
    fn git_commit_takes_only_this_sessions_writes_and_names_the_client() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        git(root, &["init", "--quiet"]);
        let paths = VaultPaths::new(root);
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        std::fs::write(root.join(".gitignore"), ".vulcan/\n").unwrap();
        std::fs::write(root.join("Home.md"), "See [[Inbox/Idea]].\n").unwrap();
        std::fs::create_dir_all(root.join("Inbox")).unwrap();
        std::fs::write(root.join("Inbox/Idea.md"), "# Idea\n").unwrap();
        std::fs::write(root.join("Other.md"), "# Other\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "--quiet", "-m", "seed"]);
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();

        let mut session =
            crate::mcp_session::McpSessionState::new(&paths, Some("unrestricted")).unwrap();
        session.record_client_info(Some(
            &json!({"clientInfo": {"name": "Agent\nX", "version": "2"}}),
        ));
        session.set_authenticated_client(Some("https://client.example/".into()), None);
        let mut call = |name: &str, arguments: Value| {
            session
                .call_tool(name, arguments.as_object().unwrap())
                .map(|result| result["structuredContent"].clone())
        };

        let nothing = call("git_commit", json!({"message": "Nothing yet"})).unwrap();
        assert_eq!(nothing["committed"], false, "{nothing}");

        // The move changes three files; another writer changes a fourth.
        call(
            "note_move",
            json!({"note": "Inbox/Idea", "destination": "Projects/Idea", "no_commit": true}),
        )
        .unwrap();
        std::fs::write(root.join("Other.md"), "# Other, edited elsewhere\n").unwrap();

        let committed = call("git_commit", json!({"message": "Move the idea"})).unwrap();
        assert_eq!(committed["committed"], true, "{committed}");
        assert_eq!(committed["scope"], "session");
        assert_eq!(
            committed["files"],
            json!(["Home.md", "Inbox/Idea.md", "Projects/Idea.md"]),
            "{committed}"
        );
        let body = git_stdout(root, &["log", "-1", "--format=%B"]);
        assert!(body.starts_with("Move the idea\n\n"), "{body}");
        for trailer in [
            "Vulcan-MCP-Client: Agent X 2",
            "Vulcan-MCP-Client-Id: https://client.example/",
            "Vulcan-MCP-Profile: unrestricted",
        ] {
            assert!(body.contains(trailer), "{body}");
        }
        assert_eq!(
            git_stdout(root, &["status", "--porcelain"]).trim(),
            "M Other.md"
        );
        assert!(session.written_paths().is_empty());

        let mut call = |name: &str, arguments: Value| {
            session
                .call_tool(name, arguments.as_object().unwrap())
                .map(|result| result["structuredContent"].clone())
        };
        let again = call("git_commit", json!({"message": "Again"})).unwrap();
        assert_eq!(again["committed"], false, "{again}");
        let rest = call("git_commit", json!({"message": "The rest", "scope": "all"})).unwrap();
        assert_eq!(rest["files"], json!(["Other.md"]), "{rest}");
        assert!(call(
            "git_commit",
            json!({"message": "x", "scope": "all", "paths": ["a.md"]})
        )
        .is_err());
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
