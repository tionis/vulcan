//! Device-local named MCP runtime and consent grant policy.

use std::collections::BTreeMap;
use ulid::Ulid;
use vulcan_core::{resolve_permission_profile, VaultPaths};

use crate::mcp_remote::McpRemoteId;
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
