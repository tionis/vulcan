//! Per-listener MCP host state shared by foreground and resident adapters.
//!
//! Pack selection is already resolved before crossing this boundary. OAuth
//! endpoints and authentication use this same state, rather than rebuilding
//! transport-specific authority from CLI options.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use ulid::Ulid;
use vulcan_app::mcp_catalog::{
    pack_name_list, resolve_selected_tool_packs, McpToolPack, McpToolPackMode, ALL_MCP_TOOL_PACKS,
};
#[cfg(feature = "oauth")]
use vulcan_core::LocalOAuthIssuer;
use vulcan_core::VaultPaths;

#[cfg(feature = "oauth")]
use crate::mcp_http_auth::McpOAuthMode;
use crate::mcp_http_auth::{authenticate_mcp_http_request, McpHttpAuthError, McpHttpAuthOptions};
use crate::mcp_session::{McpSessionAuthority, McpSessionRegistry};
#[cfg(feature = "oauth")]
use crate::{
    mcp_oauth_authorize::{IndieAuthExchange, McpAuthorizeEndpoint},
    mcp_oauth_browser::{IndieAuthConfig, PendingConsentMap, PendingIndieAuthMap},
    mcp_oauth_clients::OAuthClientRegistry,
    mcp_oauth_codes::McpAuthorizationCodeMap,
    mcp_oauth_consent::McpConsentEndpoint,
    mcp_oauth_routes::{McpLocalOAuthRoutes, McpOAuthRoutes},
    mcp_oauth_token::McpLocalTokenEndpoint,
    mcp_remote_runtime::NamedMcpRuntime,
};

#[derive(Debug, Clone)]
pub struct McpHttpHost<C> {
    pub paths: VaultPaths,
    pub requested_profile: Option<String>,
    pub selected_tool_packs: BTreeSet<McpToolPack>,
    pub tool_pack_mode: McpToolPackMode,
    pub endpoint: String,
    pub auth_token: Option<String>,
    pub bind_addr: SocketAddr,
    pub instance_id: Ulid,
    pub sessions: Arc<McpSessionRegistry<C>>,
    pub request_timeout: Duration,
    #[cfg(feature = "oauth")]
    pub oauth: Option<McpOAuthMode>,
    #[cfg(feature = "oauth")]
    pub oauth_codes: Arc<McpAuthorizationCodeMap>,
    #[cfg(feature = "oauth")]
    pub oauth_clients: Arc<OAuthClientRegistry>,
    #[cfg(feature = "oauth")]
    pub oauth_pending_indieauth: Arc<PendingIndieAuthMap>,
    #[cfg(feature = "oauth")]
    pub oauth_pending_consent: Arc<PendingConsentMap>,
    #[cfg(feature = "oauth")]
    pub oauth_dcr_enabled: bool,
    #[cfg(feature = "oauth")]
    pub oauth_dcr_allowed_redirect_hosts: Vec<String>,
    #[cfg(feature = "oauth")]
    pub oauth_local_redirect_uris: Vec<String>,
    #[cfg(feature = "oauth")]
    pub oauth_indieauth: Option<IndieAuthConfig>,
    #[cfg(feature = "oauth")]
    pub named_runtime: Option<NamedMcpRuntime>,
}

/// Authority-bound inputs to the transport-neutral application protocol core.
#[derive(Debug, Clone)]
pub struct McpHttpProtocolConfig {
    pub paths: VaultPaths,
    pub permission_profile: Option<String>,
    pub selected_tool_packs: BTreeSet<McpToolPack>,
    pub tool_pack_mode: McpToolPackMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpHttpProtocolConfigError {
    pub status: u16,
    pub message: String,
}

impl<C> McpHttpHost<C> {
    /// Select the consent-bound vault and catalog before creating session state.
    /// CLI parsing is not part of this boundary; authenticated named grants must
    /// not fall back to the listener's default vault, profile, or pack selection.
    pub fn protocol_config(
        &self,
        authority: &McpSessionAuthority,
    ) -> Result<McpHttpProtocolConfig, McpHttpProtocolConfigError> {
        #[cfg(feature = "oauth")]
        let named = self
            .named_runtime
            .as_ref()
            .map(|runtime| runtime.session_config(authority, self.instance_id))
            .transpose()
            .map_err(|message| McpHttpProtocolConfigError {
                status: 403,
                message: message.to_string(),
            })?;
        #[cfg(feature = "oauth")]
        let (paths, named_profile, named_packs) =
            named.as_ref().map_or((&self.paths, None, None), |named| {
                (
                    &named.paths,
                    Some(named.permission_profile.as_str()),
                    Some(named.tool_packs.as_slice()),
                )
            });
        #[cfg(not(feature = "oauth"))]
        let (paths, named_profile, named_packs): (_, Option<&str>, Option<&[String]>) =
            (&self.paths, None, None);
        let profile = named_profile
            .or(authority.permission_profile.as_deref())
            .or(self.requested_profile.as_deref());
        let pack_names = named_packs.or_else(|| {
            authority
                .grant_id
                .is_some()
                .then_some(authority.tool_packs.as_slice())
        });
        let packs = pack_names
            .map(|names| {
                names
                    .iter()
                    .map(|name| {
                        ALL_MCP_TOOL_PACKS
                            .iter()
                            .copied()
                            .find(|pack| *pack != McpToolPack::ToolPacks && pack.as_str() == name)
                            .ok_or_else(|| McpHttpProtocolConfigError {
                                status: 500,
                                message: format!(
                                    "named MCP remote contains unknown tool pack `{name}`"
                                ),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|packs| resolve_selected_tool_packs(&packs, self.tool_pack_mode))
            })
            .transpose()?
            .unwrap_or_else(|| self.selected_tool_packs.clone());
        Ok(McpHttpProtocolConfig {
            paths: paths.clone(),
            permission_profile: profile.map(str::to_string),
            selected_tool_packs: packs,
            tool_pack_mode: self.tool_pack_mode,
        })
    }

    pub fn authenticate(
        &self,
        headers: &BTreeMap<String, String>,
    ) -> Result<McpSessionAuthority, McpHttpAuthError> {
        authenticate_mcp_http_request(
            McpHttpAuthOptions {
                instance_id: self.instance_id,
                bind_addr: self.bind_addr,
                auth_token: self.auth_token.as_deref(),
                permission_profile: self.requested_profile.as_deref(),
                packs: pack_name_list(&self.selected_tool_packs),
                #[cfg(feature = "oauth")]
                oauth: self.oauth.as_ref(),
                #[cfg(feature = "oauth")]
                named_runtime: self.named_runtime.as_ref(),
            },
            headers,
        )
    }

    #[cfg(feature = "oauth")]
    #[must_use]
    pub fn oauth_routes(&self, exchange: IndieAuthExchange) -> Option<McpOAuthRoutes<'_>> {
        self.oauth.as_ref().map(|oauth| match oauth {
            McpOAuthMode::External(server) => McpOAuthRoutes::External(server),
            McpOAuthMode::Local(issuer) => McpOAuthRoutes::Local(Box::new(McpLocalOAuthRoutes {
                issuer,
                authorize: self.authorize_endpoint(issuer, exchange),
                consent: self.consent_endpoint(issuer),
                token: self.token_endpoint(issuer),
                dcr_enabled: self.oauth_dcr_enabled,
                allowed_redirect_hosts: &self.oauth_dcr_allowed_redirect_hosts,
                refresh_supported: self.named_runtime.is_some(),
            })),
        })
    }

    #[cfg(feature = "oauth")]
    #[must_use]
    pub fn authorize_endpoint<'a>(
        &'a self,
        issuer: &'a LocalOAuthIssuer,
        exchange: IndieAuthExchange,
    ) -> McpAuthorizeEndpoint<'a> {
        McpAuthorizeEndpoint {
            issuer,
            clients: &self.oauth_clients,
            codes: &self.oauth_codes,
            pending_indieauth: &self.oauth_pending_indieauth,
            pending_consent: &self.oauth_pending_consent,
            indieauth: self.oauth_indieauth.as_ref(),
            named_runtime: self.named_runtime.as_ref(),
            requested_profile: self.requested_profile.clone(),
            selected_packs: pack_name_list(&self.selected_tool_packs),
            fallback_vault_root: self.paths.vault_root().display().to_string(),
            local_redirect_uris: &self.oauth_local_redirect_uris,
            allowed_redirect_hosts: &self.oauth_dcr_allowed_redirect_hosts,
            exchange,
        }
    }

    #[cfg(feature = "oauth")]
    #[must_use]
    pub fn token_endpoint<'a>(&'a self, issuer: &'a LocalOAuthIssuer) -> McpLocalTokenEndpoint<'a> {
        McpLocalTokenEndpoint {
            issuer,
            clients: &self.oauth_clients,
            codes: &self.oauth_codes,
            named_runtime: self.named_runtime.as_ref(),
            instance_id: self.instance_id,
            allowed_redirect_hosts: &self.oauth_dcr_allowed_redirect_hosts,
        }
    }

    #[cfg(feature = "oauth")]
    #[must_use]
    pub fn consent_endpoint<'a>(&'a self, issuer: &'a LocalOAuthIssuer) -> McpConsentEndpoint<'a> {
        McpConsentEndpoint {
            issuer,
            pending: &self.oauth_pending_consent,
            codes: &self.oauth_codes,
            named_runtime: self.named_runtime.as_ref(),
            instance_id: self.instance_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(paths: &VaultPaths) -> McpHttpHost<()> {
        McpHttpHost {
            paths: paths.clone(),
            requested_profile: Some("readonly".into()),
            selected_tool_packs: BTreeSet::from([McpToolPack::NotesRead, McpToolPack::Search]),
            tool_pack_mode: McpToolPackMode::Static,
            endpoint: "/mcp".into(),
            auth_token: None,
            bind_addr: "127.0.0.1:4321".parse().unwrap(),
            instance_id: Ulid::new(),
            sessions: Arc::new(McpSessionRegistry::new()),
            request_timeout: Duration::from_secs(30),
            #[cfg(feature = "oauth")]
            oauth: None,
            #[cfg(feature = "oauth")]
            oauth_codes: Arc::new(McpAuthorizationCodeMap::default()),
            #[cfg(feature = "oauth")]
            oauth_clients: Arc::new(OAuthClientRegistry::ephemeral()),
            #[cfg(feature = "oauth")]
            oauth_pending_indieauth: Arc::new(PendingIndieAuthMap::default()),
            #[cfg(feature = "oauth")]
            oauth_pending_consent: Arc::new(PendingConsentMap::default()),
            #[cfg(feature = "oauth")]
            oauth_dcr_enabled: true,
            #[cfg(feature = "oauth")]
            oauth_dcr_allowed_redirect_hosts: vec!["client.example.test".into()],
            #[cfg(feature = "oauth")]
            oauth_local_redirect_uris: Vec::new(),
            #[cfg(feature = "oauth")]
            oauth_indieauth: None,
            #[cfg(feature = "oauth")]
            named_runtime: None,
        }
    }

    #[test]
    fn direct_protocol_config_preserves_listener_packs_and_profile_precedence() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let listener = host(&paths);
        let mut authority = listener.authenticate(&BTreeMap::new()).unwrap();
        // Direct callers use the invocation's resolved pack selection, not an
        // unbound list supplied independently of its listener.
        authority.tool_packs = vec!["notes-write".into()];
        authority.permission_profile = None;
        let selected = listener.protocol_config(&authority).unwrap();
        assert_eq!(selected.paths.vault_root(), paths.vault_root());
        assert_eq!(selected.permission_profile.as_deref(), Some("readonly"));
        assert_eq!(selected.selected_tool_packs, listener.selected_tool_packs);
        assert_eq!(selected.tool_pack_mode, McpToolPackMode::Static);
        authority.permission_profile = Some("prompt-reader".into());
        assert_eq!(
            listener
                .protocol_config(&authority)
                .unwrap()
                .permission_profile
                .as_deref(),
            Some("prompt-reader")
        );
    }

    #[cfg(feature = "oauth")]
    #[test]
    fn named_protocol_config_never_falls_back_from_the_consent_binding() {
        use crate::mcp_remote::McpRemoteId;
        use crate::mcp_remote_runtime::NamedMcpVaultRuntime;
        use crate::mcp_state::McpAuthorizationStore;
        use crate::registry::WikiId;

        let temporary = tempfile::tempdir().unwrap();
        let mut listener = host(&VaultPaths::new(temporary.path()));
        let consent_paths = VaultPaths::new(temporary.path().join("consented-vault"));
        let remote_id = McpRemoteId::parse("personal").unwrap();
        let wiki_id = WikiId::parse("consented").unwrap();
        listener.named_runtime = Some(NamedMcpRuntime {
            remote_id: remote_id.clone(),
            vaults: BTreeMap::from([(
                wiki_id.clone(),
                NamedMcpVaultRuntime {
                    paths: consent_paths.clone(),
                    ceiling_profile: "readonly".into(),
                    default_profile: "readonly".into(),
                    eligible_tool_packs: vec!["notes-read".into(), "tasks".into()],
                },
            )]),
            authorization_store: McpAuthorizationStore::at(temporary.path()),
        });
        let authority = McpSessionAuthority::granted(
            remote_id,
            listener.instance_id,
            Ulid::new(),
            "https://client.example/app.json".into(),
            "https://identity.example/alice".into(),
            wiki_id,
            "https://mcp.example/personal".into(),
            "prompt-reader".into(),
            vec!["tasks".into()],
            vec!["mcp:tools".into()],
            "test credential",
        );
        let selected = listener.protocol_config(&authority).unwrap();
        assert_eq!(selected.paths.vault_root(), consent_paths.vault_root());
        assert_eq!(
            selected.permission_profile.as_deref(),
            Some("prompt-reader")
        );
        assert_eq!(
            selected.selected_tool_packs,
            BTreeSet::from([McpToolPack::Tasks])
        );

        for changed in ["instance", "vault", "profile", "packs", "grant"] {
            let mut invalid = authority.clone();
            match changed {
                "instance" => invalid.remote_instance_id = Ulid::new(),
                "vault" => invalid.wiki_id = None,
                "profile" => invalid.permission_profile = None,
                "packs" => invalid.tool_packs.clear(),
                "grant" => invalid.grant_id = None,
                _ => unreachable!(),
            }
            assert_eq!(listener.protocol_config(&invalid).unwrap_err().status, 403);
        }
        listener.tool_pack_mode = McpToolPackMode::Adaptive;
        assert_eq!(
            listener
                .protocol_config(&authority)
                .unwrap()
                .selected_tool_packs,
            BTreeSet::from([McpToolPack::Tasks, McpToolPack::ToolPacks])
        );
        listener.named_runtime.as_mut().unwrap().vaults.clear();
        assert_eq!(
            listener.protocol_config(&authority).unwrap_err().status,
            403
        );
    }

    #[test]
    fn grant_pack_resolution_rejects_unknown_and_internal_pack_names() {
        let temporary = tempfile::tempdir().unwrap();
        let listener = host(&VaultPaths::new(temporary.path()));
        let mut authority = listener.authenticate(&BTreeMap::new()).unwrap();
        authority.grant_id = Some(Ulid::new());
        for name in ["unknown", "tool-packs"] {
            authority.tool_packs = vec![name.into()];
            let error = listener.protocol_config(&authority).unwrap_err();
            assert_eq!(error.status, 500);
            assert_eq!(
                error.message,
                format!("named MCP remote contains unknown tool pack `{name}`")
            );
        }
    }

    #[test]
    fn host_clones_share_sessions_but_independent_instances_do_not_share_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let first = host(&paths);
        let clone = first.clone();
        let second = host(&paths);
        assert!(Arc::ptr_eq(&first.sessions, &clone.sessions));
        assert!(!Arc::ptr_eq(&first.sessions, &second.sessions));
        let authority = first.authenticate(&BTreeMap::new()).unwrap();
        assert_eq!(authority.permission_profile.as_deref(), Some("readonly"));
        assert_eq!(authority.tool_packs, vec!["notes-read", "search"]);
        assert!(authority.matches(&clone.authenticate(&BTreeMap::new()).unwrap()));
        assert!(!authority.matches(&second.authenticate(&BTreeMap::new()).unwrap()));
    }

    #[cfg(feature = "oauth")]
    #[test]
    fn oauth_endpoint_composition_uses_the_same_listener_state() {
        use crate::mcp_http_codec::McpHttpRequest;
        use crate::mcp_http_routes::McpHttpRoute;
        use crate::mcp_oauth_authorize::default_indieauth_exchange;
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let mut listener = host(&paths);
        assert!(listener.oauth_routes(default_indieauth_exchange).is_none());
        let issuer = Arc::new(
            LocalOAuthIssuer::from_config(vulcan_core::LocalOAuthIssuerConfig {
                public_url: "https://mcp.example.test/mcp".into(),
                client_id: "static-client".into(),
                client_secret: "static-secret".into(),
                signing_key: "test-signing-key".into(),
                approval_token: String::new(),
                subject: "https://identity.example.test/me".into(),
                email: None,
                users: Vec::new(),
                dcr_enabled: true,
            })
            .unwrap(),
        );
        listener.oauth = Some(McpOAuthMode::Local(issuer.clone()));
        let clone = listener.clone();
        assert!(Arc::ptr_eq(&listener.oauth_codes, &clone.oauth_codes));
        assert!(Arc::ptr_eq(
            &listener.oauth_pending_consent,
            &clone.oauth_pending_consent
        ));
        let routes = listener.oauth_routes(default_indieauth_exchange).unwrap();
        let McpOAuthRoutes::Local(ref local) = routes else {
            panic!("local routes");
        };
        assert_eq!(local.authorize.selected_packs, vec!["notes-read", "search"]);
        assert_eq!(
            local.authorize.requested_profile.as_deref(),
            Some("readonly")
        );
        assert_eq!(local.token.instance_id, listener.instance_id);
        assert_eq!(local.consent.instance_id, listener.instance_id);
        assert!(std::ptr::eq(
            local.authorize.codes,
            listener.oauth_codes.as_ref()
        ));
        assert!(std::ptr::eq(local.consent.codes, local.token.codes));
        assert!(std::ptr::eq(local.authorize.clients, local.token.clients));
        let response = routes.handle(
            &McpHttpRequest {
                method: "GET".into(),
                path: "/.well-known/oauth-authorization-server".into(),
                query: String::new(),
                headers: BTreeMap::new(),
                body: Vec::new(),
            },
            McpHttpRoute::AuthorizationServerMetadata,
        );
        let metadata: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(metadata["issuer"], issuer.public_url());
        assert_eq!(
            metadata["grant_types_supported"],
            serde_json::json!(["authorization_code"])
        );
    }
}
