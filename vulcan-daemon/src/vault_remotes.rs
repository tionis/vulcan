//! Named MCP remotes that a vault describes in its shared configuration
//! (`[mcp_remote]` in `.vulcan/config.toml`) and that a device serves after
//! opting in on its `[[vault]]` entry in `daemon.toml`:
//!
//! ```toml
//! [[vault]]
//! id = "notes"
//! path = "~/notes"
//! mcp_remote = { bind = "0.0.0.0:8765" }
//! ```
//!
//! The registry builds these definitions each time it loads `daemon.toml`
//! and never writes them back. A push to the vault cannot expose it (only
//! the device opts in and chooses the address) and cannot widen it: the
//! ceiling must be a built-in or device-local profile, and a changed
//! `IndieAuth` identity is not used until the device accepts it
//! (`vulcan daemon apply --accept-remote <id>`). Until then the remote keeps
//! the identity the device accepted before.

use serde::{Deserialize, Serialize};
use vulcan_app::daemon_config_file::Identities;
use vulcan_core::config::VaultMcpRemoteConfig;

use crate::mcp_remote::{
    validate_definition, AddMcpRemoteRequest, McpRemoteAuthentication, McpRemoteDefinition,
    McpRemoteId, McpRemoteVault, DEFAULT_MCP_TOOL_PACKS,
};
use crate::registry::{DaemonConfig, WikiRegistration};

/// A `[[vault]]` entry's opt-in to serve the remote its vault describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultRemoteOptIn {
    /// Listener address on this device, such as `0.0.0.0:8765`.
    pub bind: String,
}

/// How an opted-in vault's remote resolved on this load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultRemoteState {
    /// Built into `DaemonConfig::mcp_remotes` from the vault's settings.
    Served,
    /// A `[[mcp_remote]]` with the same ID in `daemon.toml` wins.
    Overridden,
    /// The vault's path does not exist yet, for example before a clone.
    VaultMissing,
    /// The vault's settings are missing or invalid; `note` says why.
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultRemoteStatus {
    /// Remote ID, which is the vault ID.
    pub id: String,
    pub state: VaultRemoteState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The identity the vault asks for when this device accepted another
    /// one, which the remote keeps using until it is accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_identity: Option<String>,
    /// The identity in use was not accepted before; loading records it.
    pub first_acceptance: bool,
}

impl VaultRemoteStatus {
    #[must_use]
    pub fn served(&self) -> bool {
        self.state == VaultRemoteState::Served
    }
}

/// Adds a definition for every opted-in vault to `config.mcp_remotes` and
/// records how each resolved in `config.vault_remotes`. Problems never fail
/// the load: such a remote is left out and its status says why, so one
/// vault's bad settings cannot stop the daemon.
pub(crate) fn expand(config: &mut DaemonConfig, identities: &Identities) {
    config.vault_remotes.clear();
    let opted_in = config
        .vaults
        .iter()
        .filter_map(|wiki| wiki.mcp_remote.clone().map(|opt_in| (wiki.clone(), opt_in)))
        .collect::<Vec<_>>();
    for (wiki, opt_in) in opted_in {
        let mut status = VaultRemoteStatus {
            id: wiki.id.to_string(),
            state: VaultRemoteState::Invalid,
            note: None,
            pending_identity: None,
            first_acceptance: false,
        };
        if config
            .mcp_remotes
            .iter()
            .any(|remote| remote.id.as_str() == wiki.id.as_str())
        {
            status.state = VaultRemoteState::Overridden;
            status.note = Some(format!(
                "the [[mcp_remote]] `{}` in daemon.toml wins over the vault's [mcp_remote]",
                wiki.id
            ));
        } else if !wiki.path.is_dir() {
            status.state = VaultRemoteState::VaultMissing;
            status.note = Some(format!("starts once {} exists", wiki.path.display()));
        } else {
            match build(&wiki, &opt_in, identities, config) {
                Ok((definition, pending, first)) => {
                    status.state = VaultRemoteState::Served;
                    status.pending_identity = pending;
                    status.first_acceptance = first;
                    config.mcp_remotes.push(definition);
                }
                Err(problem) => status.note = Some(problem),
            }
        }
        config.vault_remotes.push(status);
    }
    config
        .mcp_remotes
        .sort_by(|left, right| left.id.cmp(&right.id));
}

type Built = (McpRemoteDefinition, Option<String>, bool);

fn build(
    wiki: &WikiRegistration,
    opt_in: &VaultRemoteOptIn,
    identities: &Identities,
    config: &DaemonConfig,
) -> Result<Built, String> {
    let id = McpRemoteId::parse(wiki.id.as_str()).map_err(|error| error.to_string())?;
    let loaded = vulcan_core::config::load_vault_config(&vulcan_core::VaultPaths::new(&wiki.path));
    let settings = loaded.config.mcp_remote;
    if settings.is_empty() {
        let mut problem = "the vault's .vulcan/config.toml has no [mcp_remote]".to_string();
        if !loaded.diagnostics.is_empty() {
            let details = loaded
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone())
                .collect::<Vec<_>>()
                .join("; ");
            problem = format!("{problem} ({details})");
        }
        return Err(problem);
    }
    let VaultMcpRemoteConfig {
        public_url,
        identity,
        ceiling_profile,
        default_profile,
        tool_packs,
    } = settings;
    let public_url = public_url.ok_or("the vault's [mcp_remote] sets no public_url")?;
    let identity = identity.ok_or("the vault's [mcp_remote] sets no identity")?;
    let request = AddMcpRemoteRequest {
        id: id.clone(),
        bind: opt_in.bind.clone(),
        public_url,
        authentication: McpRemoteAuthentication::IndieAuth { identity },
        vaults: vec![McpRemoteVault {
            wiki_id: wiki.id.clone(),
            ceiling_profile: ceiling_profile.unwrap_or_else(|| "readonly".to_string()),
            default_profile: default_profile.unwrap_or_else(|| "readonly".to_string()),
            tool_packs: tool_packs.unwrap_or_else(|| {
                DEFAULT_MCP_TOOL_PACKS
                    .iter()
                    .map(|pack| (*pack).to_string())
                    .collect()
            }),
        }],
    };
    let mut definition = request
        .into_definition()
        .map_err(|error| format!("the vault's [mcp_remote]: {error}"))?;
    // The registry fills the stored identity, as for any declaration.
    definition.instance_id = ulid::Ulid::nil();
    let McpRemoteAuthentication::IndieAuth { identity: asked } = &definition.authentication;
    let asked = asked.clone();
    let accepted = identities
        .mcp_remotes
        .get(id.as_str())
        .and_then(|stored| stored.accepted_identity.clone());
    let (pending, first) = match accepted {
        Some(accepted) if accepted != asked => {
            definition.authentication = McpRemoteAuthentication::IndieAuth { identity: accepted };
            (Some(asked), false)
        }
        Some(_) => (None, false),
        None => (None, true),
    };
    validate_definition(&definition).map_err(|error| error.to_string())?;
    for other in &config.mcp_remotes {
        if other.bind == definition.bind {
            return Err(format!(
                "bind `{}` is already used by remote `{}`",
                definition.bind, other.id
            ));
        }
        if other.public_url == definition.public_url {
            return Err(format!(
                "public URL `{}` is already used by remote `{}`",
                definition.public_url, other.id
            ));
        }
    }
    Ok((definition, pending, first))
}

/// `config` without the remotes built from vault opt-ins, for writing
/// `daemon.toml`: they belong to the vaults.
pub(crate) fn declared_only(config: &DaemonConfig) -> DaemonConfig {
    let mut declared = config.clone();
    declared
        .mcp_remotes
        .retain(|remote| !is_vault_remote(config, remote.id.as_str()));
    declared.vault_remotes.clear();
    declared
}

pub(crate) fn is_vault_remote(config: &DaemonConfig, id: &str) -> bool {
    config
        .vault_remotes
        .iter()
        .any(|status| status.served() && status.id == id)
}
