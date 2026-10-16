//! The daemon REST API (Roadmap 10.3): request principals and the routes a
//! per-vault API token may reach. See `docs/specs/daemon-rest-api.md`.

use axum::http::Method;
use ulid::Ulid;

/// Who a request acts for, resolved from its bearer credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// The companion credential: every route and registered vault.
    Device,
    /// An API token: the REST routes of one registration.
    Vault {
        wiki_id: String,
        registration_id: Ulid,
        token_id: Ulid,
    },
}

impl Principal {
    /// Whether this principal may see the registered wiki `wiki_id`.
    #[must_use]
    pub fn sees_wiki(&self, wiki_id: &str) -> bool {
        match self {
            Self::Device => true,
            Self::Vault { wiki_id: own, .. } => own == wiki_id,
        }
    }

    /// The scope idempotency keys of this principal live in.
    #[must_use]
    pub fn idempotency_scope(&self, companion_credential_id: &str) -> String {
        match self {
            Self::Device => companion_credential_id.to_string(),
            Self::Vault { token_id, .. } => format!("api-token:{token_id}"),
        }
    }
}

/// Daemon-wide routes an API token may call.
const VAULT_PRINCIPAL_DAEMON_ROUTES: &[&str] =
    &["/capabilities", "/health", "/vaults", "/openapi.json"];

/// The first path segment after `/{id}/` of every per-vault REST route.
pub const REST_VAULT_SEGMENTS: &[&str] = &[
    "search",
    "notes",
    "links",
    "backlinks",
    "graph",
    "query",
    "history",
    "diff",
    "move",
    "inbox",
    "scan",
];

/// Whether a vault principal for `wiki_id` may call `method path`. Sync
/// administration, routed `ops`, jobs, events, and shutdown stay device-only,
/// and another vault's routes are out of reach.
#[must_use]
pub fn vault_principal_reaches(method: &Method, path: &str, wiki_id: &str) -> bool {
    if VAULT_PRINCIPAL_DAEMON_ROUTES.contains(&path) {
        return method == Method::GET;
    }
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    let Some((id, route)) = rest.split_once('/') else {
        return false;
    };
    let segment = route.split('/').next().unwrap_or_default();
    id == wiki_id && REST_VAULT_SEGMENTS.contains(&segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vault_principal_reaches_only_its_own_rest_routes() {
        let get = Method::GET;
        for path in ["/capabilities", "/health", "/vaults", "/openapi.json"] {
            assert!(vault_principal_reaches(&get, path, "notes"), "{path}");
        }
        assert!(!vault_principal_reaches(&Method::POST, "/vaults", "notes"));
        for path in [
            "/notes/search",
            "/notes/notes/Projects/Plan.md",
            "/notes/graph/stats",
            "/notes/history/Home.md",
        ] {
            assert!(vault_principal_reaches(&get, path, "notes"), "{path}");
        }
        for path in [
            "/other/search",
            "/notes/sync/status",
            "/notes/sync/pause",
            "/notes/ops/query",
            "/jobs/1",
            "/events",
            "/shutdown",
            "/sync",
            "/notes",
        ] {
            assert!(!vault_principal_reaches(&get, path, "notes"), "{path}");
        }
    }

    #[test]
    fn principals_see_their_wikis_and_scope_idempotency_keys() {
        let vault = Principal::Vault {
            wiki_id: "notes".to_string(),
            registration_id: Ulid::new(),
            token_id: Ulid::new(),
        };
        assert!(vault.sees_wiki("notes"));
        assert!(!vault.sees_wiki("other"));
        assert!(Principal::Device.sees_wiki("other"));
        assert_ne!(
            vault.idempotency_scope("companion"),
            Principal::Device.idempotency_scope("companion")
        );
    }
}
