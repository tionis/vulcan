//! Bounded, single-use browser transactions for local MCP OAuth issuers.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::prelude::{Engine as _, BASE64_URL_SAFE_NO_PAD};
use subtle::ConstantTimeEq;

use crate::mcp_http_codec::McpHttpResponse;
use crate::mcp_remote_runtime::NamedMcpRuntime;

const MAX_PENDING_TRANSACTIONS: usize = 256;
const TRANSACTION_LIFETIME: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct PendingIndieAuth {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub indieauth_code_verifier: String,
    pub state: Option<String>,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct PendingConsent {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub subject: String,
    pub scopes: Vec<String>,
    pub resource: String,
    pub state: Option<String>,
    pub csrf_token: String,
    pub expires_at: Instant,
}

pub type PendingIndieAuthMap = Mutex<BTreeMap<String, PendingIndieAuth>>;
pub type PendingConsentMap = Mutex<BTreeMap<String, PendingConsent>>;

#[derive(Debug, Clone)]
pub struct IndieAuthConfig {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub me: Option<String>,
}

#[must_use]
pub fn client_redirect(
    redirect_uri: &str,
    result_query: &str,
    state: Option<&str>,
) -> McpHttpResponse {
    let separator = if redirect_uri.contains('?') { '&' } else { '?' };
    let mut location = format!("{redirect_uri}{separator}{result_query}");
    if let Some(state) = state {
        location.push_str("&state=");
        location.push_str(&percent_encode(state));
    }
    McpHttpResponse {
        status: 302,
        content_type: None,
        body: Vec::new(),
        extra_headers: vec![
            ("Location".to_string(), location),
            ("Cache-Control".to_string(), "no-store".to_string()),
        ],
    }
}

#[must_use]
pub fn redirect_to_indieauth(
    indieauth: &IndieAuthConfig,
    state: &str,
    code_challenge: &str,
) -> McpHttpResponse {
    let separator = if indieauth.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut location = format!(
        "{}{separator}response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
        indieauth.authorization_endpoint,
        percent_encode(&indieauth.client_id),
        percent_encode(&indieauth.redirect_uri),
        percent_encode(state),
        percent_encode(code_challenge)
    );
    if let Some(me) = indieauth.me.as_ref() {
        location.push_str("&me=");
        location.push_str(&percent_encode(me));
    }
    McpHttpResponse {
        status: 302,
        content_type: None,
        body: Vec::new(),
        extra_headers: vec![
            ("Location".to_string(), location),
            ("Cache-Control".to_string(), "no-store".to_string()),
        ],
    }
}

#[must_use]
pub fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            write!(output, "%{byte:02X}").expect("writing to a String should not fail");
        }
    }
    output
}

pub struct ConsentPage<'a> {
    pub transaction_id: &'a str,
    pub pending: &'a PendingConsent,
    pub client_name: &'a str,
    pub fallback_vault_root: &'a str,
    pub profile: &'a str,
    pub packs: &'a [String],
    pub named_runtime: Option<&'a NamedMcpRuntime>,
}

#[must_use]
pub fn render_consent_page(page: &ConsentPage<'_>) -> McpHttpResponse {
    let (vault_control, profile_control, pack_controls, expiry_control) =
        page.named_runtime.map_or_else(
            || (
                html_escape(page.fallback_vault_root),
                html_escape(page.profile),
                html_escape(&page.packs.join(", ")),
                String::new(),
            ),
            |named| {
                let multi = named.vaults.len() > 1;
                let controls = named.vaults.iter().map(|(wiki_id, vault)| {
                    let wiki = html_escape(wiki_id.as_str());
                    let profile_field = if multi { format!("permission_profile_{wiki}") } else { "permission_profile".to_string() };
                    let profile_control = format!(
                        "<input name=\"{profile_field}\" value=\"{}\" list=\"profiles_{wiki}\" required><datalist id=\"profiles_{wiki}\"><option value=\"{}\"><option value=\"{}\"></datalist>",
                        html_escape(&vault.default_profile),
                        html_escape(&vault.default_profile),
                        html_escape(&vault.ceiling_profile),
                    );
                    let pack_controls = vault.eligible_tool_packs.iter().map(|pack| {
                        let field = if multi { format!("pack_{wiki}_{pack}") } else { format!("pack_{pack}") };
                        format!("<label><input type=\"checkbox\" name=\"{}\" value=\"on\" checked> {}</label>", html_escape(&field), html_escape(pack))
                    }).collect::<Vec<_>>().join(" ");
                    let label = format!("{} ({})", wiki, html_escape(&vault.paths.vault_root().display().to_string()));
                    (label, profile_control, pack_controls)
                }).collect::<Vec<_>>();
                let expiry = "<label>Expiry <select name=\"expiry_days\"><option value=\"1\">1 day</option><option value=\"7\">7 days</option><option value=\"30\" selected>30 days</option></select></label>".to_string();
                if multi {
                    use std::fmt::Write as _;
                    let mut vaults = String::new();
                    for ((wiki_id, _), (label, profile, packs)) in named.vaults.iter().zip(&controls) {
                        write!(&mut vaults, "<fieldset><legend><label><input type=\"radio\" name=\"wiki_id\" value=\"{}\" required> {label}</label></legend><p>Permission profile: {profile}</p><p>Tool packs: {packs}</p></fieldset>", html_escape(wiki_id.as_str())).expect("writing to a String cannot fail");
                    }
                    (vaults, "Choose one vault below".to_string(), "Each vault has its own eligible packs".to_string(), expiry)
                } else {
                    let (label, profile, packs) = controls.into_iter().next().expect("named remote has a vault");
                    (label, profile, packs, expiry)
                }
            },
        );
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Authorize Vulcan MCP</title></head>\
         <body><main><h1>Authorize this MCP connection?</h1>\
         <form method=\"post\" action=\"/oauth/consent\">\
         <dl><dt>Client</dt><dd>{}</dd><dt>Identity</dt><dd>{}</dd>\
         <dt>Resource</dt><dd>{}</dd><dt>Vault</dt><dd>{}</dd>\
         <dt>Permission profile</dt><dd>{}</dd><dt>Tool packs</dt><dd>{}</dd>\
         <dt>OAuth scopes</dt><dd>{}</dd></dl>\
         <p>Tool packs control discovery. The permission profile remains the authority ceiling.</p>\
         <input type=\"hidden\" name=\"transaction\" value=\"{}\">\
         <input type=\"hidden\" name=\"csrf_token\" value=\"{}\">\
         {}<button type=\"submit\" name=\"decision\" value=\"approve\">Approve</button>\
         <button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button>\
         </form></main></body></html>",
        html_escape(page.client_name),
        html_escape(&page.pending.subject),
        html_escape(&page.pending.resource),
        vault_control,
        profile_control,
        pack_controls,
        html_escape(&page.pending.scopes.join(" ")),
        html_escape(page.transaction_id),
        html_escape(&page.pending.csrf_token),
        expiry_control,
    );
    McpHttpResponse {
        status: 200,
        content_type: Some("text/html; charset=utf-8"),
        body: body.into_bytes(),
        extra_headers: vec![
            ("Cache-Control".to_string(), "no-store".to_string()),
            ("X-Frame-Options".to_string(), "DENY".to_string()),
            (
                "Content-Security-Policy".to_string(),
                "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
                    .to_string(),
            ),
        ],
    }
}

#[must_use]
pub fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginError {
    Capacity,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeError {
    Unknown,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentError {
    Unknown,
    Expired,
    InvalidCsrf,
    InvalidDecision,
}

fn random_token() -> Result<String, BeginError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| BeginError::Random)?;
    Ok(BASE64_URL_SAFE_NO_PAD.encode(bytes))
}

pub fn begin_indieauth(
    pending: &PendingIndieAuthMap,
    mut transaction: PendingIndieAuth,
) -> Result<String, BeginError> {
    let mut entries = pending.lock().expect("pending IndieAuth lock poisoned");
    entries.retain(|_, entry| entry.expires_at >= Instant::now());
    if entries.len() >= MAX_PENDING_TRANSACTIONS {
        return Err(BeginError::Capacity);
    }
    let state = random_token()?;
    transaction.expires_at = Instant::now() + TRANSACTION_LIFETIME;
    entries.insert(state.clone(), transaction);
    Ok(state)
}

pub fn take_indieauth(
    pending: &PendingIndieAuthMap,
    state: &str,
) -> Result<PendingIndieAuth, TakeError> {
    let transaction = pending
        .lock()
        .expect("pending IndieAuth lock poisoned")
        .remove(state)
        .ok_or(TakeError::Unknown)?;
    if transaction.expires_at < Instant::now() {
        return Err(TakeError::Expired);
    }
    Ok(transaction)
}

pub fn begin_consent(
    pending: &PendingConsentMap,
    mut transaction: PendingConsent,
) -> Result<(String, PendingConsent), BeginError> {
    let mut entries = pending.lock().expect("pending consent lock poisoned");
    entries.retain(|_, entry| entry.expires_at >= Instant::now());
    if entries.len() >= MAX_PENDING_TRANSACTIONS {
        return Err(BeginError::Capacity);
    }
    let id = random_token()?;
    transaction.csrf_token = random_token()?;
    transaction.expires_at = Instant::now() + TRANSACTION_LIFETIME;
    entries.insert(id.clone(), transaction.clone());
    Ok((id, transaction))
}

pub fn consume_consent(
    pending: &PendingConsentMap,
    id: &str,
    csrf: &str,
    decision: &str,
) -> Result<PendingConsent, ConsentError> {
    let mut entries = pending.lock().expect("pending consent lock poisoned");
    let transaction = entries.get(id).ok_or(ConsentError::Unknown)?;
    if transaction.expires_at < Instant::now() {
        entries.remove(id);
        return Err(ConsentError::Expired);
    }
    if transaction
        .csrf_token
        .as_bytes()
        .ct_eq(csrf.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(ConsentError::InvalidCsrf);
    }
    if !matches!(decision, "approve" | "deny") {
        return Err(ConsentError::InvalidDecision);
    }
    Ok(entries.remove(id).expect("validated consent exists"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_remote::McpRemoteId;
    use crate::mcp_remote_runtime::NamedMcpVaultRuntime;
    use crate::mcp_state::McpAuthorizationStore;
    use crate::registry::WikiId;
    use std::sync::Arc;
    use vulcan_core::VaultPaths;

    fn consent() -> PendingConsent {
        PendingConsent {
            client_id: "client".into(),
            redirect_uri: "https://client.example/callback".into(),
            code_challenge: "challenge".into(),
            subject: "https://user.example/".into(),
            scopes: vec!["mcp:tools".into()],
            resource: "https://remote.example/mcp".into(),
            state: None,
            csrf_token: String::new(),
            expires_at: Instant::now(),
        }
    }

    fn indieauth() -> PendingIndieAuth {
        PendingIndieAuth {
            client_id: "client".into(),
            redirect_uri: "https://client.example/callback".into(),
            code_challenge: "challenge".into(),
            scopes: vec!["mcp:tools".into()],
            resource: "https://remote.example/mcp".into(),
            indieauth_code_verifier: "verifier".into(),
            state: Some("original-client-state".into()),
            expires_at: Instant::now(),
        }
    }

    #[test]
    fn indieauth_state_is_random_bounded_and_single_use() {
        let map = PendingIndieAuthMap::default();
        let state = begin_indieauth(&map, indieauth()).unwrap();
        assert_eq!(state.len(), 43);
        assert_eq!(
            take_indieauth(&map, &state).unwrap().state.as_deref(),
            Some("original-client-state")
        );
        assert!(matches!(
            take_indieauth(&map, &state),
            Err(TakeError::Unknown)
        ));
        for _ in 0..MAX_PENDING_TRANSACTIONS {
            begin_indieauth(&map, indieauth()).unwrap();
        }
        assert_eq!(
            begin_indieauth(&map, indieauth()).unwrap_err(),
            BeginError::Capacity
        );
        let first = map.lock().unwrap().keys().next().unwrap().clone();
        map.lock().unwrap().get_mut(&first).unwrap().expires_at =
            Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            take_indieauth(&map, &first),
            Err(TakeError::Expired)
        ));
        assert!(begin_indieauth(&map, indieauth()).is_ok());
    }

    #[test]
    fn direct_consent_page_escapes_client_fields_and_denies_framing() {
        let mut transaction = consent();
        transaction.subject = "<script>alert(1)</script>".into();
        transaction.csrf_token = "csrf-secret".into();
        let response = render_consent_page(&ConsentPage {
            transaction_id: "transaction",
            pending: &transaction,
            client_name: "<bad-client>",
            fallback_vault_root: "/vault<&>",
            profile: "readonly",
            packs: &["notes-read".into()],
            named_runtime: None,
        });
        let html = String::from_utf8(response.body).unwrap();
        assert!(html.contains("&lt;bad-client&gt;"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("/vault&lt;&amp;&gt;"));
        assert!(!html.contains("<script>"));
        assert!(response
            .extra_headers
            .iter()
            .any(|(name, value)| name == "X-Frame-Options" && value == "DENY"));
        assert!(response
            .extra_headers
            .iter()
            .any(|(name, value)| name == "Cache-Control" && value == "no-store"));
        assert!(response
            .extra_headers
            .iter()
            .any(|(name, value)| name == "Content-Security-Policy"
                && value.contains("frame-ancestors 'none'")));
    }

    #[test]
    fn named_consent_page_requires_one_vault_selection() {
        let temp = tempfile::tempdir().unwrap();
        let vaults = ["personal", "work"]
            .into_iter()
            .map(|name| {
                (
                    WikiId::parse(name).unwrap(),
                    NamedMcpVaultRuntime {
                        paths: VaultPaths::new(temp.path().join(name)),
                        ceiling_profile: "agent".into(),
                        default_profile: "readonly".into(),
                        eligible_tool_packs: vec!["notes-read".into(), "search".into()],
                    },
                )
            })
            .collect();
        let named = NamedMcpRuntime {
            remote_id: McpRemoteId::parse("personal-chatgpt").unwrap(),
            vaults,
            authorization_store: McpAuthorizationStore::at(temp.path().join("state")),
        };
        let transaction = consent();
        let response = render_consent_page(&ConsentPage {
            transaction_id: "transaction",
            pending: &transaction,
            client_name: "client",
            fallback_vault_root: "ignored",
            profile: "ignored",
            packs: &[],
            named_runtime: Some(&named),
        });
        let html = String::from_utf8(response.body).unwrap();
        assert_eq!(html.matches("name=\"wiki_id\"").count(), 2);
        assert!(html.contains("permission_profile_personal"));
        assert!(html.contains("permission_profile_work"));
        assert!(html.contains("pack_personal_search"));
        assert!(html.contains("pack_work_search"));
        assert!(html.contains("name=\"expiry_days\""));
    }

    #[test]
    fn browser_redirects_encode_state_and_preserve_registered_queries() {
        let client = client_redirect(
            "https://client.example/callback?existing=one",
            "code=issued-code",
            Some("a&b c"),
        );
        assert!(client.extra_headers.iter().any(|(name, value)| name == "Location" && value == "https://client.example/callback?existing=one&code=issued-code&state=a%26b%20c"));
        assert!(client
            .extra_headers
            .iter()
            .any(|(name, value)| name == "Cache-Control" && value == "no-store"));

        let upstream = redirect_to_indieauth(
            &IndieAuthConfig {
                authorization_endpoint: "https://indie.example/authorize?existing=one".into(),
                token_endpoint: "https://indie.example/token".into(),
                client_id: "https://remote.example/".into(),
                redirect_uri: "https://remote.example/oauth/indieauth/callback".into(),
                me: Some("https://person.example/a?b=c&d=e".into()),
            },
            "state&value",
            "challenge+value",
        );
        let location = upstream
            .extra_headers
            .iter()
            .find_map(|(name, value)| (name == "Location").then_some(value.as_str()))
            .unwrap();
        assert!(location
            .starts_with("https://indie.example/authorize?existing=one&response_type=code&"));
        assert!(location.contains("state=state%26value"));
        assert!(location.contains("code_challenge=challenge%2Bvalue"));
        assert!(location.contains("me=https%3A%2F%2Fperson.example%2Fa%3Fb%3Dc%26d%3De"));
        assert!(upstream
            .extra_headers
            .iter()
            .any(|(name, value)| name == "Cache-Control" && value == "no-store"));
    }

    #[test]
    fn consent_is_random_bounded_and_single_use() {
        let map = PendingConsentMap::default();
        let (id, record) = begin_consent(&map, consent()).unwrap();
        assert_eq!(id.len(), 43);
        assert_eq!(record.csrf_token.len(), 43);
        assert_ne!(id, record.csrf_token);
        assert!(matches!(
            consume_consent(&map, &id, "wrong", "approve"),
            Err(ConsentError::InvalidCsrf)
        ));
        assert!(matches!(
            consume_consent(&map, &id, &record.csrf_token, "other"),
            Err(ConsentError::InvalidDecision)
        ));
        assert!(consume_consent(&map, &id, &record.csrf_token, "deny").is_ok());
        assert!(matches!(
            consume_consent(&map, &id, &record.csrf_token, "approve"),
            Err(ConsentError::Unknown)
        ));
    }

    #[test]
    fn concurrent_approval_consumes_at_most_once() {
        let map = Arc::new(PendingConsentMap::default());
        let (id, record) = begin_consent(&map, consent()).unwrap();
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let map = Arc::clone(&map);
                let id = id.clone();
                let csrf = record.csrf_token.clone();
                std::thread::spawn(move || consume_consent(&map, &id, &csrf, "approve").is_ok())
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|approved| *approved)
                .count(),
            1
        );
    }

    #[test]
    fn capacity_reclaims_expired_transactions() {
        let map = PendingConsentMap::default();
        for _ in 0..MAX_PENDING_TRANSACTIONS {
            begin_consent(&map, consent()).unwrap();
        }
        assert_eq!(
            begin_consent(&map, consent()).unwrap_err(),
            BeginError::Capacity
        );
        let first = map.lock().unwrap().keys().next().unwrap().clone();
        map.lock().unwrap().get_mut(&first).unwrap().expires_at =
            Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(begin_consent(&map, consent()).is_ok());
    }
}
