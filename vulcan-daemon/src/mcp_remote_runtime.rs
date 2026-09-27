//! Device-local named MCP runtime and consent grant policy.

use std::collections::BTreeMap;
use ulid::Ulid;
use vulcan_core::{resolve_permission_profile, VaultPaths};

use crate::mcp_remote::McpRemoteId;
use crate::mcp_session::McpSessionAuthority;
use crate::mcp_state::{CreateConnectionGrant, McpAuthorizationStore};
use crate::registry::WikiId;

#[derive(Debug, Clone)]
pub struct NamedMcpRuntime {
    pub remote_id: McpRemoteId,
    pub vaults: BTreeMap<WikiId, NamedMcpVaultRuntime>,
    pub authorization_store: McpAuthorizationStore,
}

#[derive(Debug, Clone)]
pub struct NamedMcpVaultRuntime {
    pub paths: VaultPaths,
    pub ceiling_profile: String,
    pub default_profile: String,
    pub eligible_tool_packs: Vec<String>,
}

pub struct NamedConsentRequest<'a> {
    pub remote_instance_id: Ulid,
    pub client_id: &'a str,
    pub subject: &'a str,
    pub scopes: &'a [String],
    pub resource: &'a str,
    pub form: &'a BTreeMap<String, String>,
    pub now: u64,
}

pub struct NamedTokenRequest<'a> {
    pub remote_instance_id: Ulid,
    pub grant_id: Ulid,
    pub client_id: &'a str,
    pub subject: Option<&'a str>,
    pub scopes: &'a [String],
    pub resource: &'a str,
    pub credential: &'a str,
    pub now: u64,
}

#[derive(Debug, Clone)]
pub struct NamedMcpSessionConfig {
    pub paths: VaultPaths,
    pub permission_profile: String,
    pub tool_packs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedConsentError {
    pub status: u16,
    pub message: String,
}

impl NamedConsentError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: 500,
            message: message.into(),
        }
    }
}

impl NamedMcpRuntime {
    /// Select the grant-bound vault and startup capability set for an HTTP session.
    pub fn session_config(
        &self,
        authority: &McpSessionAuthority,
        remote_instance_id: Ulid,
    ) -> Result<NamedMcpSessionConfig, &'static str> {
        let wiki_id = authority
            .wiki_id
            .as_ref()
            .ok_or("grant has no vault binding")?;
        let vault = self
            .vaults
            .get(wiki_id)
            .ok_or("grant vault is unavailable")?;
        if authority.remote_id.as_ref() != Some(&self.remote_id)
            || authority.remote_instance_id != remote_instance_id
            || authority.grant_id.is_none()
            || authority.permission_profile.is_none()
            || authority.tool_packs.is_empty()
            || !authority
                .tool_packs
                .iter()
                .all(|pack| vault.eligible_tool_packs.contains(pack))
        {
            return Err("grant authority does not match this remote");
        }
        Ok(NamedMcpSessionConfig {
            paths: vault.paths.clone(),
            permission_profile: authority
                .permission_profile
                .clone()
                .expect("checked profile binding"),
            tool_packs: authority.tool_packs.clone(),
        })
    }

    /// Revalidate a named access token against durable consent and current vault policy.
    pub fn authorize_token(
        &self,
        request: &NamedTokenRequest<'_>,
    ) -> Result<McpSessionAuthority, String> {
        let grant = self
            .authorization_store
            .resolve_active_grant(
                request.grant_id,
                request.remote_instance_id,
                request.client_id,
                request.resource,
                request.now,
            )
            .map_err(|error| error.to_string())?;
        let vault = self
            .vaults
            .get(&grant.wiki_id)
            .ok_or("connection grant vault is no longer exposed")?;
        if grant.remote_id != self.remote_id
            || request.subject != Some(grant.subject.as_str())
            || !request
                .scopes
                .iter()
                .all(|scope| grant.scopes.contains(scope))
            || !grant
                .tool_packs
                .iter()
                .all(|pack| vault.eligible_tool_packs.contains(pack))
        {
            return Err("connection grant does not match this token authority".to_string());
        }
        let current_profile =
            resolve_permission_profile(&vault.paths, Some(&grant.permission_profile))
                .map_err(|error| error.to_string())?;
        let ceiling = resolve_permission_profile(&vault.paths, Some(&vault.ceiling_profile))
            .map_err(|error| error.to_string())?;
        if !current_profile
            .grant
            .is_subset_of(&grant.approved_permissions)
            || !current_profile.grant.is_subset_of(&ceiling.grant)
        {
            return Err(
                "current permission policy is not a safe attenuation of the approved grant"
                    .to_string(),
            );
        }
        self.authorization_store
            .attenuate_grant_permissions(grant.id, &current_profile.grant, request.now)
            .map_err(|error| error.to_string())?;
        self.authorization_store
            .mark_grant_used(grant.id, request.now)
            .map_err(|error| error.to_string())?;
        Ok(McpSessionAuthority::granted(
            self.remote_id.clone(),
            request.remote_instance_id,
            grant.id,
            request.client_id.to_string(),
            grant.subject,
            grant.wiki_id,
            grant.audience,
            current_profile.name,
            grant.tool_packs,
            request.scopes.to_vec(),
            request.credential,
        ))
    }

    pub fn create_connection_grant(
        &self,
        request: &NamedConsentRequest<'_>,
    ) -> Result<Ulid, NamedConsentError> {
        let (wiki_id, vault) = match request.form.get("wiki_id") {
            Some(value) => self
                .vaults
                .iter()
                .find(|(wiki_id, _)| wiki_id.as_str() == value)
                .ok_or_else(|| NamedConsentError::bad_request("selected vault is not exposed"))?,
            None if self.vaults.len() == 1 => self.vaults.iter().next().expect("one vault"),
            None => {
                return Err(NamedConsentError::bad_request(
                    "select a vault for this connection",
                ))
            }
        };
        let profile_field = if self.vaults.len() == 1 {
            "permission_profile".to_string()
        } else {
            format!("permission_profile_{wiki_id}")
        };
        let profile_name = request
            .form
            .get(&profile_field)
            .filter(|value| !value.is_empty())
            .map_or(vault.default_profile.as_str(), String::as_str);
        let selected = resolve_permission_profile(&vault.paths, Some(profile_name))
            .map_err(|error| NamedConsentError::bad_request(error.to_string()))?;
        let ceiling = resolve_permission_profile(&vault.paths, Some(&vault.ceiling_profile))
            .map_err(|error| NamedConsentError::internal(error.to_string()))?;
        if !selected.grant.is_subset_of(&ceiling.grant) {
            return Err(NamedConsentError::bad_request(
                "selected permission profile exceeds this remote's ceiling",
            ));
        }
        let pack_prefix = if self.vaults.len() == 1 {
            "pack_".to_string()
        } else {
            format!("pack_{wiki_id}_")
        };
        let tool_packs = vault
            .eligible_tool_packs
            .iter()
            .filter(|pack| request.form.contains_key(&format!("{pack_prefix}{pack}")))
            .cloned()
            .collect::<Vec<_>>();
        if tool_packs.is_empty() {
            return Err(NamedConsentError::bad_request(
                "select at least one eligible tool pack",
            ));
        }
        let lifetime_days = request
            .form
            .get("expiry_days")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|days| matches!(days, 1 | 7 | 30))
            .ok_or_else(|| NamedConsentError::bad_request("expiry must be 1, 7, or 30 days"))?;
        let expires_at = request
            .now
            .checked_add(lifetime_days * 24 * 60 * 60)
            .ok_or_else(|| NamedConsentError::internal("grant expiry is out of range"))?;
        let report = self
            .authorization_store
            .create_grant(
                CreateConnectionGrant {
                    remote_id: self.remote_id.clone(),
                    remote_instance_id: request.remote_instance_id,
                    client_id: request.client_id.to_string(),
                    subject: request.subject.to_string(),
                    wiki_id: wiki_id.clone(),
                    permission_profile: selected.name,
                    approved_permissions: selected.grant,
                    tool_packs,
                    scopes: request.scopes.to_vec(),
                    audience: request.resource.to_string(),
                    created_at: request.now,
                    expires_at,
                },
                false,
            )
            .map_err(|error| NamedConsentError::internal(error.to_string()))?;
        Ok(report.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use vulcan_core::initialize_vulcan_dir;

    fn runtime() -> (tempfile::TempDir, NamedMcpRuntime) {
        let temporary = tempfile::tempdir().expect("temporary state");
        let vault_root = temporary.path().join("personal");
        fs::create_dir_all(&vault_root).expect("vault root");
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("initialize vault");
        let wiki_id = WikiId::parse("personal").expect("wiki ID");
        let runtime = NamedMcpRuntime {
            remote_id: McpRemoteId::parse("chatgpt").expect("remote ID"),
            vaults: BTreeMap::from([(
                wiki_id,
                NamedMcpVaultRuntime {
                    paths,
                    ceiling_profile: "readonly".to_string(),
                    default_profile: "readonly".to_string(),
                    eligible_tool_packs: vec!["notes-read".to_string(), "search".to_string()],
                },
            )]),
            authorization_store: McpAuthorizationStore::at(temporary.path().join("state")),
        };
        (temporary, runtime)
    }

    fn request<'a>(
        instance_id: Ulid,
        form: &'a BTreeMap<String, String>,
        scopes: &'a [String],
    ) -> NamedConsentRequest<'a> {
        NamedConsentRequest {
            remote_instance_id: instance_id,
            client_id: "https://client.example/app.json",
            subject: "https://identity.example/alice",
            scopes,
            resource: "https://mcp.example/personal",
            form,
            now: 1_700_000_000,
        }
    }

    #[test]
    fn consent_persists_only_selected_profile_packs_and_expiry() {
        let (_temporary, runtime) = runtime();
        let instance_id = Ulid::new();
        let scopes = vec!["mcp:tools".to_string()];
        let form = BTreeMap::from([
            ("pack_notes-read".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "7".to_string()),
        ]);
        let id = runtime
            .create_connection_grant(&request(instance_id, &form, &scopes))
            .expect("approved consent");
        let grant = runtime
            .authorization_store
            .resolve_active_grant(
                id,
                instance_id,
                "https://client.example/app.json",
                "https://mcp.example/personal",
                1_700_000_001,
            )
            .expect("durable grant");
        assert_eq!(grant.permission_profile, "readonly");
        assert_eq!(grant.tool_packs, vec!["notes-read"]);
        assert_eq!(grant.scopes, scopes);
        assert_eq!(grant.expires_at, 1_700_000_000 + 7 * 24 * 60 * 60);
    }

    #[test]
    fn token_authority_revalidates_consent_binding_and_revocation() {
        let (_temporary, runtime) = runtime();
        let instance_id = Ulid::new();
        let scopes = vec!["mcp:tools".to_string()];
        let form = BTreeMap::from([
            ("pack_notes-read".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "7".to_string()),
        ]);
        let grant_id = runtime
            .create_connection_grant(&request(instance_id, &form, &scopes))
            .expect("approved consent");
        let mut token = NamedTokenRequest {
            remote_instance_id: instance_id,
            grant_id,
            client_id: "https://client.example/app.json",
            subject: Some("https://identity.example/alice"),
            scopes: &scopes,
            resource: "https://mcp.example/personal",
            credential: "secret access token",
            now: 1_700_000_001,
        };
        let authority = runtime.authorize_token(&token).expect("bound authority");
        assert_eq!(authority.grant_id, Some(grant_id));
        assert_eq!(authority.tool_packs, vec!["notes-read"]);
        assert_eq!(authority.scopes, scopes);
        assert_eq!(authority.permission_profile.as_deref(), Some("readonly"));

        token.subject = Some("https://identity.example/bob");
        assert!(runtime.authorize_token(&token).is_err());
        token.subject = Some("https://identity.example/alice");
        token.client_id = "https://other.example/app.json";
        assert!(runtime.authorize_token(&token).is_err());
        token.client_id = "https://client.example/app.json";
        token.resource = "https://other.example/mcp";
        assert!(runtime.authorize_token(&token).is_err());
        token.resource = "https://mcp.example/personal";
        let widened = vec!["mcp:tools".to_string(), "mcp:prompts".to_string()];
        token.scopes = &widened;
        assert!(runtime.authorize_token(&token).is_err());
        token.scopes = &scopes;

        runtime
            .authorization_store
            .revoke_grant(grant_id, token.now, false)
            .expect("revoke grant");
        assert!(runtime.authorize_token(&token).is_err());
    }

    #[test]
    fn token_authority_rejects_removed_vault_or_eligible_pack() {
        let (_temporary, mut runtime) = runtime();
        let instance_id = Ulid::new();
        let scopes = vec!["mcp:tools".to_string()];
        let form = BTreeMap::from([
            ("pack_notes-read".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "7".to_string()),
        ]);
        let grant_id = runtime
            .create_connection_grant(&request(instance_id, &form, &scopes))
            .expect("approved consent");
        let token = NamedTokenRequest {
            remote_instance_id: instance_id,
            grant_id,
            client_id: "https://client.example/app.json",
            subject: Some("https://identity.example/alice"),
            scopes: &scopes,
            resource: "https://mcp.example/personal",
            credential: "secret access token",
            now: 1_700_000_001,
        };
        runtime
            .vaults
            .get_mut(&WikiId::parse("personal").expect("wiki ID"))
            .expect("vault")
            .eligible_tool_packs
            .clear();
        assert!(runtime.authorize_token(&token).is_err());
        runtime.vaults.clear();
        assert_eq!(
            runtime.authorize_token(&token).expect_err("removed vault"),
            "connection grant vault is no longer exposed"
        );
    }

    #[test]
    fn session_config_stays_bound_to_the_named_vault_profile_and_packs() {
        let (_temporary, runtime) = runtime();
        let instance_id = Ulid::new();
        let authority = McpSessionAuthority::granted(
            runtime.remote_id.clone(),
            instance_id,
            Ulid::new(),
            "https://client.example/app.json".to_string(),
            "https://identity.example/alice".to_string(),
            WikiId::parse("personal").expect("wiki ID"),
            "https://mcp.example/personal".to_string(),
            "readonly".to_string(),
            vec!["notes-read".to_string()],
            vec!["mcp:tools".to_string()],
            "secret access token",
        );
        let selected = runtime
            .session_config(&authority, instance_id)
            .expect("named session configuration");
        assert_eq!(
            selected.paths.vault_root(),
            runtime
                .vaults
                .get(&WikiId::parse("personal").expect("wiki ID"))
                .expect("vault")
                .paths
                .vault_root()
        );
        assert_eq!(selected.permission_profile, "readonly");
        assert_eq!(selected.tool_packs, ["notes-read"]);
        assert!(runtime.session_config(&authority, Ulid::new()).is_err());
        let direct = McpSessionAuthority::direct(
            instance_id,
            "local token",
            None,
            None,
            Some("readonly".to_string()),
            vec!["notes-read".to_string()],
            Vec::new(),
        );
        assert!(runtime.session_config(&direct, instance_id).is_err());

        let mut wrong_pack = authority.clone();
        wrong_pack.tool_packs = vec!["notes-write".to_string()];
        assert!(runtime.session_config(&wrong_pack, instance_id).is_err());
        let mut missing_vault = authority.clone();
        missing_vault.wiki_id = None;
        assert_eq!(
            runtime
                .session_config(&missing_vault, instance_id)
                .unwrap_err(),
            "grant has no vault binding"
        );
        let mut removed_vault = runtime.clone();
        removed_vault.vaults.clear();
        assert_eq!(
            removed_vault
                .session_config(&authority, instance_id)
                .unwrap_err(),
            "grant vault is unavailable"
        );
    }

    #[test]
    fn narrowing_a_profile_durably_prevents_later_re_expansion() {
        let (_temporary, mut runtime) = runtime();
        let vault = runtime
            .vaults
            .get_mut(&WikiId::parse("personal").expect("wiki ID"))
            .expect("vault");
        vault.ceiling_profile = "agent".to_string();
        vault.default_profile = "agent".to_string();
        let config_file = vault.paths.config_file().to_path_buf();
        fs::write(
            &config_file,
            "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"all\"\n",
        )
        .expect("wide profile");
        let instance_id = Ulid::new();
        let scopes = vec!["mcp:tools".to_string()];
        let form = BTreeMap::from([
            ("pack_notes-read".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "7".to_string()),
        ]);
        let grant_id = runtime
            .create_connection_grant(&request(instance_id, &form, &scopes))
            .expect("approved consent");
        let token = NamedTokenRequest {
            remote_instance_id: instance_id,
            grant_id,
            client_id: "https://client.example/app.json",
            subject: Some("https://identity.example/alice"),
            scopes: &scopes,
            resource: "https://mcp.example/personal",
            credential: "secret access token",
            now: 1_700_000_001,
        };
        runtime.authorize_token(&token).expect("original approval");

        fs::write(
            &config_file,
            "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"none\"\n",
        )
        .expect("narrow profile");
        runtime.authorize_token(&token).expect("narrowed approval");
        assert!(runtime
            .authorization_store
            .show_grant(grant_id)
            .expect("durable grant")
            .approved_permissions
            .write
            .allow
            .is_empty());

        fs::write(
            &config_file,
            "[permissions.profiles.agent]\nread = \"all\"\nwrite = \"all\"\n",
        )
        .expect("re-expand profile");
        assert!(runtime.authorize_token(&token).is_err());
    }

    #[test]
    fn consent_rejects_profile_expansion_and_unselected_packs() {
        let (_temporary, runtime) = runtime();
        let scopes = vec!["mcp:tools".to_string()];
        let form = BTreeMap::from([
            ("permission_profile".to_string(), "unrestricted".to_string()),
            ("pack_notes-read".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "1".to_string()),
        ]);
        let error = runtime
            .create_connection_grant(&request(Ulid::new(), &form, &scopes))
            .expect_err("profile exceeds ceiling");
        assert_eq!(error.status, 400);
        assert!(error.message.contains("ceiling"));

        let forged = BTreeMap::from([
            ("pack_notes-write".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "1".to_string()),
        ]);
        let error = runtime
            .create_connection_grant(&request(Ulid::new(), &forged, &scopes))
            .expect_err("ineligible pack does not count as a selection");
        assert_eq!(error.status, 400);
        assert!(error.message.contains("at least one"));
        assert!(runtime
            .authorization_store
            .list_grants(None)
            .expect("grant list")
            .is_empty());
    }

    #[test]
    fn multi_vault_consent_requires_an_explicit_exposed_vault() {
        let (temporary, mut runtime) = runtime();
        let team_root = temporary.path().join("team");
        fs::create_dir_all(&team_root).expect("team vault");
        let paths = VaultPaths::new(&team_root);
        initialize_vulcan_dir(&paths).expect("initialize team vault");
        runtime.vaults.insert(
            WikiId::parse("team").expect("team ID"),
            NamedMcpVaultRuntime {
                paths,
                ceiling_profile: "readonly".to_string(),
                default_profile: "readonly".to_string(),
                eligible_tool_packs: vec!["search".to_string()],
            },
        );
        let scopes = vec!["mcp:tools".to_string()];
        let mut form = BTreeMap::from([
            ("pack_team_search".to_string(), "on".to_string()),
            ("expiry_days".to_string(), "30".to_string()),
        ]);
        let error = runtime
            .create_connection_grant(&request(Ulid::new(), &form, &scopes))
            .expect_err("vault selection required");
        assert_eq!(error.message, "select a vault for this connection");
        form.insert("wiki_id".to_string(), "team".to_string());
        let instance_id = Ulid::new();
        let id = runtime
            .create_connection_grant(&request(instance_id, &form, &scopes))
            .expect("team grant");
        let grant = runtime
            .authorization_store
            .resolve_active_grant(
                id,
                instance_id,
                "https://client.example/app.json",
                "https://mcp.example/personal",
                1_700_000_001,
            )
            .expect("durable team grant");
        assert_eq!(grant.wiki_id.as_str(), "team");
        assert_eq!(grant.tool_packs, vec!["search"]);
    }
}
