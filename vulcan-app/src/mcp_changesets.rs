//! MCP changeset tools: stage a session's edits in a worktree outside the
//! vault, then land or abandon them (see [`crate::changesets`]).

use serde_json::{json, Value};
use std::path::PathBuf;
use vulcan_core::{PermissionGuard, ProfilePermissionGuard, VaultPaths};

use crate::changesets::{self, ChangesetLandCommit};
use crate::mcp_protocol::{
    McpChangesetBeginArgs, McpChangesetIdArgs, McpChangesetLandArgs, McpMethodError,
};
use crate::mcp_session::McpClientAttribution;

/// Which tree a session works in. Tools run against the attached changeset's
/// worktree, or the vault when none is attached; a changeset tool asks the
/// session to switch by setting `pending`, which the session applies after
/// the call.
#[derive(Debug, Clone)]
pub struct McpChangesetBinding {
    /// The vault itself, which owns permissions and changesets.
    pub vault: VaultPaths,
    pub active: Option<String>,
    pub pending: Option<McpChangesetSwitch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpChangesetSwitch {
    Attach { id: String, worktree: PathBuf },
    Detach,
}

impl McpChangesetBinding {
    #[must_use]
    pub fn new(vault: &VaultPaths) -> Self {
        Self {
            vault: vault.clone(),
            active: None,
            pending: None,
        }
    }

    fn target(&self, id: Option<&str>) -> Result<String, McpMethodError> {
        id.map(str::to_string)
            .or_else(|| self.active.clone())
            .ok_or_else(|| {
                McpMethodError::invalid_params(
                    "no changeset is attached to this session; pass `id`",
                )
            })
    }

    fn detach_if_active(&mut self, id: &str) {
        if self.active.as_deref() == Some(id) {
            self.pending = Some(McpChangesetSwitch::Detach);
        }
    }
}

fn tool_error(error: impl std::fmt::Display) -> McpMethodError {
    McpMethodError::tool(error.to_string())
}

fn to_value(report: impl serde::Serialize) -> Result<Value, McpMethodError> {
    serde_json::to_value(report).map_err(|error| McpMethodError::internal(error.to_string()))
}

fn with_attached(mut value: Value, attached: Option<&str>) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.insert("attached".to_string(), json!(attached));
    }
    value
}

pub fn changeset_begin(
    binding: &mut McpChangesetBinding,
    guard: &ProfilePermissionGuard,
    args: &McpChangesetBeginArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let changeset =
        changesets::begin_changeset(&binding.vault, args.title.as_deref()).map_err(tool_error)?;
    binding.pending = Some(McpChangesetSwitch::Attach {
        id: changeset.id.clone(),
        worktree: changeset.worktree.clone(),
    });
    Ok(with_attached(to_value(&changeset)?, Some(&changeset.id)))
}

pub fn changeset_switch(
    binding: &mut McpChangesetBinding,
    guard: &ProfilePermissionGuard,
    args: &McpChangesetIdArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let Some(id) = args.id.as_deref() else {
        binding.pending = Some(McpChangesetSwitch::Detach);
        return Ok(json!({ "attached": null }));
    };
    let changeset = changesets::find_changeset(&binding.vault, id).map_err(tool_error)?;
    binding.pending = Some(McpChangesetSwitch::Attach {
        id: changeset.id.clone(),
        worktree: changeset.worktree.clone(),
    });
    Ok(with_attached(to_value(&changeset)?, Some(id)))
}

pub fn changeset_list(
    binding: &McpChangesetBinding,
    guard: &ProfilePermissionGuard,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let changesets = changesets::list_changesets(&binding.vault).map_err(tool_error)?;
    Ok(json!({
        "changesets": changesets,
        "attached": binding.active,
    }))
}

pub fn changeset_land(
    binding: &mut McpChangesetBinding,
    guard: &ProfilePermissionGuard,
    profile_name: &str,
    client: &McpClientAttribution,
    args: &McpChangesetLandArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let id = binding.target(args.id.as_deref())?;
    let trailers = client.trailers(profile_name);
    let commit = args.message.as_deref().map(|message| ChangesetLandCommit {
        message,
        trailers: &trailers,
        permission_profile: Some(profile_name),
    });
    let report =
        changesets::land_changeset(&binding.vault, &id, commit.as_ref()).map_err(tool_error)?;
    binding.detach_if_active(&id);
    let attached = match &binding.pending {
        Some(McpChangesetSwitch::Detach) => None,
        _ => binding.active.clone(),
    };
    Ok(with_attached(to_value(report)?, attached.as_deref()))
}

pub fn changeset_abandon(
    binding: &mut McpChangesetBinding,
    guard: &ProfilePermissionGuard,
    args: &McpChangesetIdArgs,
) -> Result<Value, McpMethodError> {
    guard.check_git().map_err(tool_error)?;
    let id = binding.target(args.id.as_deref())?;
    let changeset = changesets::abandon_changeset(&binding.vault, &id).map_err(tool_error)?;
    binding.detach_if_active(&id);
    let attached = match &binding.pending {
        Some(McpChangesetSwitch::Detach) => None,
        _ => binding.active.clone(),
    };
    Ok(with_attached(
        json!({ "abandoned": changeset }),
        attached.as_deref(),
    ))
}
