//! Device-global definitions for remotely reachable MCP server instances.
//!
//! These definitions describe deployment ceilings and routing. They deliberately
//! contain no OAuth secrets, issued grants, refresh tokens, or session state.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::SocketAddr;
use ulid::Ulid;

use crate::registry::WikiId;

pub const MCP_REMOTE_DEFINITION_VERSION: u32 = 1;
pub const DEFAULT_MCP_TOOL_PACKS: &[&str] = &["notes-read", "search", "status"];
const MAX_REMOTE_ID_BYTES: usize = 64;
const MAX_PROFILE_BYTES: usize = 128;
const MAX_URL_BYTES: usize = 2_048;
const MAX_TOOL_PACKS: usize = 32;
const KNOWN_TOOL_PACKS: &[&str] = &[
    "notes-read",
    "search",
    "status",
    "graph",
    "custom",
    "daily",
    "tasks",
    "notes-write",
    "notes-manage",
    "web",
    "config",
    "index",
    "sync",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct McpRemoteId(String);

impl McpRemoteId {
    pub fn parse(value: impl Into<String>) -> Result<Self, McpRemoteValidationError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_REMOTE_ID_BYTES
            && value.bytes().enumerate().all(|(index, byte)| match byte {
                b'a'..=b'z' | b'0'..=b'9' => true,
                b'-' | b'_' => index > 0,
                _ => false,
            });
        if !valid {
            return Err(McpRemoteValidationError::new(format!(
                "invalid remote MCP ID `{value}`; use 1-64 lowercase ASCII letters, digits, `-`, or `_`, starting with a letter or digit"
            )));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for McpRemoteId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpRemoteAuthentication {
    IndieAuth { identity: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRemoteVault {
    pub wiki_id: WikiId,
    pub ceiling_profile: String,
    #[serde(default = "default_permission_profile")]
    pub default_profile: String,
    #[serde(default = "default_tool_packs")]
    pub tool_packs: Vec<String>,
}

fn default_permission_profile() -> String {
    "readonly".to_string()
}

fn default_tool_packs() -> Vec<String> {
    DEFAULT_MCP_TOOL_PACKS
        .iter()
        .map(|pack| (*pack).to_string())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRemoteDefinition {
    pub version: u32,
    pub id: McpRemoteId,
    pub instance_id: Ulid,
    pub bind: String,
    pub public_url: String,
    pub authentication: McpRemoteAuthentication,
    pub vaults: Vec<McpRemoteVault>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddMcpRemoteRequest {
    pub id: McpRemoteId,
    pub bind: String,
    pub public_url: String,
    pub authentication: McpRemoteAuthentication,
    pub vaults: Vec<McpRemoteVault>,
}

impl AddMcpRemoteRequest {
    pub(crate) fn into_definition(self) -> Result<McpRemoteDefinition, McpRemoteValidationError> {
        let mut definition = McpRemoteDefinition {
            version: MCP_REMOTE_DEFINITION_VERSION,
            id: self.id,
            instance_id: Ulid::new(),
            bind: self.bind,
            public_url: self.public_url,
            authentication: self.authentication,
            vaults: self.vaults,
        };
        normalize_definition(&mut definition)?;
        Ok(definition)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRemoteValidationError {
    detail: String,
}

impl McpRemoteValidationError {
    fn new(detail: String) -> Self {
        Self { detail }
    }
}

impl Display for McpRemoteValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for McpRemoteValidationError {}

pub(crate) fn validate_definition(
    definition: &McpRemoteDefinition,
) -> Result<(), McpRemoteValidationError> {
    if definition.version != MCP_REMOTE_DEFINITION_VERSION {
        return Err(McpRemoteValidationError::new(format!(
            "remote MCP `{}` uses unsupported definition version {}",
            definition.id, definition.version
        )));
    }
    McpRemoteId::parse(definition.id.as_str())?;
    validate_loopback_bind(&definition.bind)?;
    validate_https_url(&definition.public_url, "public URL")?;
    match &definition.authentication {
        McpRemoteAuthentication::IndieAuth { identity } => {
            validate_https_url(identity, "IndieAuth identity")?;
        }
    }
    if definition.vaults.is_empty() {
        return Err(McpRemoteValidationError::new(format!(
            "remote MCP `{}` must expose at least one registered wiki",
            definition.id
        )));
    }
    let mut wiki_ids = std::collections::BTreeSet::new();
    for vault in &definition.vaults {
        if !wiki_ids.insert(&vault.wiki_id) {
            return Err(McpRemoteValidationError::new(format!(
                "remote MCP `{}` contains duplicate wiki `{}`",
                definition.id, vault.wiki_id
            )));
        }
        validate_profile_name(&vault.ceiling_profile, "ceiling")?;
        validate_profile_name(&vault.default_profile, "default")?;
        validate_tool_packs(&vault.tool_packs)?;
    }
    Ok(())
}

fn normalize_definition(
    definition: &mut McpRemoteDefinition,
) -> Result<(), McpRemoteValidationError> {
    definition.public_url = normalized_https_url(&definition.public_url, "public URL")?;
    match &mut definition.authentication {
        McpRemoteAuthentication::IndieAuth { identity } => {
            *identity = normalized_https_url(identity, "IndieAuth identity")?;
        }
    }
    for vault in &mut definition.vaults {
        vault.tool_packs.sort();
        vault.tool_packs.dedup();
    }
    definition
        .vaults
        .sort_by(|left, right| left.wiki_id.cmp(&right.wiki_id));
    validate_definition(definition)
}

fn validate_loopback_bind(bind: &str) -> Result<(), McpRemoteValidationError> {
    let address = bind.parse::<SocketAddr>().map_err(|error| {
        McpRemoteValidationError::new(format!(
            "remote MCP bind address `{bind}` is invalid: {error}"
        ))
    })?;
    if !address.ip().is_loopback() {
        return Err(McpRemoteValidationError::new(format!(
            "remote MCP bind address must be loopback, got `{address}`"
        )));
    }
    Ok(())
}

fn validate_https_url(value: &str, label: &str) -> Result<(), McpRemoteValidationError> {
    normalized_https_url(value, label).map(|_| ())
}

fn normalized_https_url(value: &str, label: &str) -> Result<String, McpRemoteValidationError> {
    let url = reqwest::Url::parse(value).map_err(|error| {
        McpRemoteValidationError::new(format!("remote MCP {label} is invalid: {error}"))
    })?;
    if value.len() > MAX_URL_BYTES
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(McpRemoteValidationError::new(format!(
            "remote MCP {label} must be a bounded HTTPS URL without credentials, query, or fragment"
        )));
    }
    Ok(url.to_string())
}

fn validate_profile_name(value: &str, label: &str) -> Result<(), McpRemoteValidationError> {
    if value.is_empty()
        || value.len() > MAX_PROFILE_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(McpRemoteValidationError::new(format!(
            "remote MCP {label} permission profile must contain 1-{MAX_PROFILE_BYTES} non-control bytes"
        )));
    }
    Ok(())
}

fn validate_tool_packs(tool_packs: &[String]) -> Result<(), McpRemoteValidationError> {
    if tool_packs.is_empty() || tool_packs.len() > MAX_TOOL_PACKS {
        return Err(McpRemoteValidationError::new(format!(
            "remote MCP tool packs must contain 1-{MAX_TOOL_PACKS} entries"
        )));
    }
    let mut unique = std::collections::BTreeSet::new();
    for pack in tool_packs {
        if !KNOWN_TOOL_PACKS.contains(&pack.as_str()) {
            return Err(McpRemoteValidationError::new(format!(
                "unknown remote MCP tool pack `{pack}`"
            )));
        }
        if !unique.insert(pack) {
            return Err(McpRemoteValidationError::new(format!(
                "duplicate remote MCP tool pack `{pack}`"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> McpRemoteDefinition {
        AddMcpRemoteRequest {
            id: McpRemoteId::parse("personal-chatgpt").expect("remote ID"),
            bind: "127.0.0.1:8765".to_string(),
            public_url: "https://mcp.example.test/personal".to_string(),
            authentication: McpRemoteAuthentication::IndieAuth {
                identity: "https://example.test/eric".to_string(),
            },
            vaults: vec![McpRemoteVault {
                wiki_id: WikiId::parse("personal").expect("wiki ID"),
                ceiling_profile: "agent".to_string(),
                default_profile: "readonly".to_string(),
                tool_packs: default_tool_packs(),
            }],
        }
        .into_definition()
        .expect("definition")
    }

    #[test]
    fn remote_definition_normalizes_urls_and_pack_order() {
        let mut request = definition();
        request.public_url = "https://mcp.example.test/personal/../notes".to_string();
        request.vaults[0].tool_packs = vec!["status".to_string(), "notes-read".to_string()];
        normalize_definition(&mut request).expect("normalized definition");
        assert_eq!(request.public_url, "https://mcp.example.test/notes");
        assert_eq!(request.vaults[0].tool_packs, ["notes-read", "status"]);
    }

    #[test]
    fn remote_definition_rejects_unsafe_network_and_authority_values() {
        let mut candidate = definition();
        candidate.bind = "0.0.0.0:8765".to_string();
        assert!(validate_definition(&candidate).is_err());

        let mut candidate = definition();
        candidate.public_url = "http://mcp.example.test/personal".to_string();
        assert!(validate_definition(&candidate).is_err());

        let mut candidate = definition();
        candidate.vaults[0].tool_packs = vec!["unknown".to_string()];
        assert!(validate_definition(&candidate).is_err());
    }

    #[test]
    fn remote_ids_are_url_safe_and_bounded() {
        assert!(McpRemoteId::parse("personal-chatgpt").is_ok());
        for invalid in ["", "ChatGPT", "-remote", "remote/path"] {
            assert!(McpRemoteId::parse(invalid).is_err());
        }
    }
}
