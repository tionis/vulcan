//! Device grants that let a vault's skill command run exact host commands.
//!
//! A skill command declares the argument vectors it needs in
//! `host_commands`. Declaring them grants nothing: someone on this device
//! runs `vulcan tool grant <tool>`, which records the vault, the tool, those
//! argument vectors, and a BLAKE3 digest of the whole skill directory in the
//! user config directory, beside the trusted-vaults list. The grant lives
//! outside the vault, so neither sync nor an agent writing vault files can
//! create or widen it, and any change to the skill's files (a synced commit
//! included) leaves it stale until it is granted again.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vulcan_core::durable::{self, Durability};
use vulcan_core::VaultPaths;

use crate::tools::CustomToolRegistryOptions;
use crate::AppError;

pub const TOOL_GRANTS_FILE_NAME: &str = "tool_grants.json";
const MAX_TOOL_GRANTS_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SKILL_FILES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolGrant {
    pub vault: PathBuf,
    pub tool: String,
    pub host_commands: Vec<Vec<String>>,
    /// BLAKE3 over the skill directory's files when the grant was made.
    pub digest: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ToolGrants {
    grants: Vec<ToolGrant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolGrantState {
    /// The skill is unchanged since the grant; its commands may run.
    Active,
    /// The skill changed since the grant; nothing runs until it is granted again.
    Stale,
    /// The tool no longer exists in the vault.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolGrantStatus {
    #[serde(flatten)]
    pub grant: ToolGrant,
    pub state: ToolGrantState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolGrantReport {
    pub dry_run: bool,
    pub grant: ToolGrant,
    /// Whether this replaced an earlier grant for the same tool.
    pub replaced: bool,
}

fn grants_file() -> Result<PathBuf, AppError> {
    vulcan_core::paths::vulcan_user_config_dir()
        .map(|directory| directory.join(TOOL_GRANTS_FILE_NAME))
        .ok_or_else(|| AppError::operation("could not determine user config directory"))
}

fn load() -> Result<ToolGrants, AppError> {
    Ok(
        durable::read_json(&grants_file()?, MAX_TOOL_GRANTS_FILE_BYTES)
            .map_err(AppError::operation)?
            .unwrap_or_default(),
    )
}

fn save(grants: &ToolGrants) -> Result<(), AppError> {
    durable::replace_json(&grants_file()?, grants, Durability::Full).map_err(AppError::operation)
}

fn vault_key(paths: &VaultPaths) -> Result<PathBuf, AppError> {
    paths
        .vault_root()
        .canonicalize()
        .map_err(|error| AppError::operation(format!("cannot canonicalize vault path: {error}")))
}

/// BLAKE3 over every file below `directory`: relative path, then contents,
/// in path order. A symbolic link contributes its target, not what it
/// points at.
pub fn skill_digest(directory: &Path) -> Result<String, AppError> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).map_err(AppError::operation)? {
            let entry = entry.map_err(AppError::operation)?;
            let kind = entry.file_type().map_err(AppError::operation)?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                files.push((entry.path(), kind.is_symlink()));
            }
            if files.len() > MAX_SKILL_FILES {
                return Err(AppError::operation(format!(
                    "`{}` holds more than {MAX_SKILL_FILES} files",
                    directory.display()
                )));
            }
        }
    }
    files.sort();
    let mut hasher = blake3::Hasher::new();
    for (path, symlink) in files {
        let relative = path.strip_prefix(directory).unwrap_or(&path);
        hasher.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update(&[0]);
        if symlink {
            let target = fs::read_link(&path).map_err(AppError::operation)?;
            hasher.update(b"symlink:");
            hasher.update(target.to_string_lossy().as_bytes());
        } else {
            let bytes = fs::read(&path).map_err(AppError::operation)?;
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
        hasher.update(&[0]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Grant `tool` the host commands it declares, pinned to its current files.
pub fn grant_tool(
    paths: &VaultPaths,
    tool: &str,
    options: &CustomToolRegistryOptions,
    dry_run: bool,
) -> Result<ToolGrantReport, AppError> {
    let (name, directory, declared) =
        crate::tools::skill_command_host_commands(paths, tool, options)?;
    if declared.is_empty() {
        return Err(AppError::operation(format!(
            "tool `{name}` declares no `host_commands`, so there is nothing to grant"
        )));
    }
    let grant = ToolGrant {
        vault: vault_key(paths)?,
        tool: name,
        host_commands: declared,
        digest: skill_digest(&directory)?,
    };
    let mut grants = load()?;
    let before = grants.grants.len();
    grants
        .grants
        .retain(|existing| !(existing.vault == grant.vault && existing.tool == grant.tool));
    let replaced = grants.grants.len() != before;
    grants.grants.push(grant.clone());
    if !dry_run {
        save(&grants)?;
    }
    Ok(ToolGrantReport {
        dry_run,
        grant,
        replaced,
    })
}

/// Remove `tool`'s grant in this vault. Returns whether one existed.
pub fn revoke_tool_grant(paths: &VaultPaths, tool: &str, dry_run: bool) -> Result<bool, AppError> {
    let vault = vault_key(paths)?;
    let mut grants = load()?;
    let before = grants.grants.len();
    grants
        .grants
        .retain(|existing| !(existing.vault == vault && existing.tool == tool));
    let removed = grants.grants.len() != before;
    if removed && !dry_run {
        save(&grants)?;
    }
    Ok(removed)
}

/// This vault's grants and whether each still applies.
pub fn list_tool_grants(
    paths: &VaultPaths,
    options: &CustomToolRegistryOptions,
) -> Result<Vec<ToolGrantStatus>, AppError> {
    let vault = vault_key(paths)?;
    Ok(load()?
        .grants
        .into_iter()
        .filter(|grant| grant.vault == vault)
        .map(|grant| {
            let state = match crate::tools::skill_command_host_commands(paths, &grant.tool, options)
            {
                Ok((_, directory, _)) => {
                    if skill_digest(&directory).is_ok_and(|digest| digest == grant.digest) {
                        ToolGrantState::Active
                    } else {
                        ToolGrantState::Stale
                    }
                }
                Err(_) => ToolGrantState::Missing,
            };
            ToolGrantStatus { grant, state }
        })
        .collect())
}

/// The host commands `tool` may run now: those granted on this device that
/// it still declares, provided its skill directory is unchanged since.
#[must_use]
pub fn active_host_commands(
    paths: &VaultPaths,
    tool: &str,
    directory: &Path,
    declared: &[Vec<String>],
) -> Vec<Vec<String>> {
    if declared.is_empty() {
        return Vec::new();
    }
    let (Ok(vault), Ok(grants)) = (vault_key(paths), load()) else {
        return Vec::new();
    };
    let Some(grant) = grants
        .grants
        .into_iter()
        .find(|grant| grant.vault == vault && grant.tool == tool)
    else {
        return Vec::new();
    };
    if !skill_digest(directory).is_ok_and(|digest| digest == grant.digest) {
        return Vec::new();
    }
    grant
        .host_commands
        .into_iter()
        .filter(|argv| declared.contains(argv))
        .collect()
}
