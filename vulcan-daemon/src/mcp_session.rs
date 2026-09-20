//! Authority binding for one Streamable HTTP MCP session.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use ulid::Ulid;

use crate::mcp_remote::McpRemoteId;
use crate::registry::WikiId;

#[derive(Clone, Eq, Serialize)]
pub struct McpSessionAuthority {
    pub remote_id: Option<McpRemoteId>,
    pub remote_instance_id: Ulid,
    pub grant_id: Option<Ulid>,
    pub client_id: Option<String>,
    pub subject: Option<String>,
    pub wiki_id: Option<WikiId>,
    pub audience: Option<String>,
    pub permission_profile: Option<String>,
    pub tool_packs: Vec<String>,
    pub scopes: Vec<String>,
    #[serde(skip)]
    credential_fingerprint: String,
}

impl std::fmt::Debug for McpSessionAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpSessionAuthority")
            .field("remote_id", &self.remote_id)
            .field("remote_instance_id", &self.remote_instance_id)
            .field("grant_id", &self.grant_id)
            .field("client_id", &self.client_id)
            .field("subject", &self.subject)
            .field("wiki_id", &self.wiki_id)
            .field("audience", &self.audience)
            .field("permission_profile", &self.permission_profile)
            .field("tool_packs", &self.tool_packs)
            .field("scopes", &self.scopes)
            .field("credential_fingerprint", &"[REDACTED]")
            .finish()
    }
}

impl PartialEq for McpSessionAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.remote_id == other.remote_id
            && self.remote_instance_id == other.remote_instance_id
            && self.grant_id == other.grant_id
            && self.client_id == other.client_id
            && self.subject == other.subject
            && self.wiki_id == other.wiki_id
            && self.audience == other.audience
            && self.permission_profile == other.permission_profile
            && self.tool_packs == other.tool_packs
            && self.scopes == other.scopes
            && bool::from(
                self.credential_fingerprint
                    .as_bytes()
                    .ct_eq(other.credential_fingerprint.as_bytes()),
            )
    }
}

impl McpSessionAuthority {
    #[must_use]
    pub fn direct(
        remote_instance_id: Ulid,
        credential: &str,
        client_id: Option<String>,
        subject: Option<String>,
        permission_profile: Option<String>,
        tool_packs: Vec<String>,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            remote_id: None,
            remote_instance_id,
            grant_id: None,
            client_id,
            subject,
            wiki_id: None,
            audience: None,
            permission_profile,
            tool_packs,
            scopes,
            credential_fingerprint: fingerprint(credential),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn granted(
        remote_id: McpRemoteId,
        remote_instance_id: Ulid,
        grant_id: Ulid,
        client_id: String,
        subject: String,
        wiki_id: WikiId,
        audience: String,
        permission_profile: String,
        tool_packs: Vec<String>,
        scopes: Vec<String>,
        credential: &str,
    ) -> Self {
        Self {
            remote_id: Some(remote_id),
            remote_instance_id,
            grant_id: Some(grant_id),
            client_id: Some(client_id),
            subject: Some(subject),
            wiki_id: Some(wiki_id),
            audience: Some(audience),
            permission_profile: Some(permission_profile),
            tool_packs,
            scopes,
            credential_fingerprint: fingerprint(credential),
        }
    }

    #[must_use]
    pub fn matches(&self, request: &Self) -> bool {
        self == request
    }

    #[must_use]
    pub fn allows_scope(&self, required: &str) -> bool {
        self.scopes.iter().any(|scope| scope == required)
    }
}

fn fingerprint(credential: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(credential.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(
        instance: Ulid,
        subject: &str,
        grant: Ulid,
        credential: &str,
    ) -> McpSessionAuthority {
        McpSessionAuthority::granted(
            McpRemoteId::parse("chatgpt").expect("remote"),
            instance,
            grant,
            "client".to_string(),
            subject.to_string(),
            WikiId::parse("personal").expect("wiki"),
            "https://mcp.example.test/personal".to_string(),
            "readonly".to_string(),
            vec!["notes-read".to_string()],
            vec!["mcp:tools".to_string()],
            credential,
        )
    }

    #[test]
    fn session_authority_isolated_by_every_security_boundary() {
        let instance = Ulid::new();
        let grant = Ulid::new();
        let expected = authority(instance, "https://id.example/alice", grant, "token-a");
        assert!(expected.matches(&authority(
            instance,
            "https://id.example/alice",
            grant,
            "token-a"
        )));
        assert!(!expected.matches(&authority(
            instance,
            "https://id.example/bob",
            grant,
            "token-a"
        )));
        assert!(!expected.matches(&authority(
            Ulid::new(),
            "https://id.example/alice",
            grant,
            "token-a"
        )));
        assert!(!expected.matches(&authority(
            instance,
            "https://id.example/alice",
            Ulid::new(),
            "token-a"
        )));
        assert!(!expected.matches(&authority(
            instance,
            "https://id.example/alice",
            grant,
            "token-b"
        )));
    }

    #[test]
    fn reports_never_serialize_the_credential_fingerprint() {
        let value = serde_json::to_string(&authority(
            Ulid::new(),
            "https://id.example/alice",
            Ulid::new(),
            "raw-secret",
        ))
        .expect("serialize");
        assert!(!value.contains("raw-secret"));
        assert!(!value.contains("credential_fingerprint"));
    }
}
